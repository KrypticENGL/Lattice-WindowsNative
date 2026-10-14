//! Layer B for DWARF (ELF objects and MinGW COFF objects).
//!
//! Relocatable objects leave DWARF cross-section offsets (into `.debug_str`,
//! `.debug_abbrev`, ...) to the linker, so each debug section is copied and
//! its relocations applied before gimli reads it. Subprograms are matched to
//! symbols by linkage name rather than address: with one section per
//! function, every function starts at address 0 of its own section.

use std::borrow::Cow;

use gimli::{AttributeValue, EndianSlice, RunTimeEndian};
use object::{Object, ObjectSection, ObjectSymbol, RelocationTarget};

use crate::debug::{DebugInfo, FuncInfo, Location, VarInfo};

type R<'a> = EndianSlice<'a, RunTimeEndian>;

/// Bounds DIE-tree and type-chain recursion on hostile input.
const MAX_DEPTH: usize = 64;
const MAX_TYPE_DEPTH: usize = 8;

/// Reads DWARF, or `None` if the object has no `.debug_info`.
pub(crate) fn read(file: &object::File<'_>, warnings: &mut Vec<String>) -> Option<DebugInfo> {
    file.section_by_name(".debug_info")?;
    let endian = if file.is_little_endian() {
        RunTimeEndian::Little
    } else {
        RunTimeEndian::Big
    };
    let loaded = gimli::DwarfSections::load(|id| -> Result<Cow<'_, [u8]>, gimli::Error> {
        Ok(load_section(file, id.name(), endian))
    });
    let owned = match loaded {
        Ok(d) => d,
        Err(e) => {
            warnings.push(format!("DWARF: {e}"));
            return None;
        }
    };
    let dwarf = owned.borrow(|section| EndianSlice::new(section, endian));

    let mut info = DebugInfo::default();
    let mut units = dwarf.units();
    loop {
        let header = match units.next() {
            Ok(Some(h)) => h,
            Ok(None) => break,
            Err(e) => {
                warnings.push(format!("DWARF unit header: {e}"));
                break;
            }
        };
        let unit = match dwarf.unit(header) {
            Ok(u) => u,
            Err(e) => {
                warnings.push(format!("DWARF unit: {e}"));
                continue;
            }
        };
        let cx = Cx {
            dwarf: &dwarf,
            unit: &unit,
        };
        if let Err(e) = cx.walk_unit(&mut info) {
            warnings.push(format!("DWARF entries: {e}"));
        }
    }
    Some(info)
}

/// Section bytes with relocations applied (empty if absent or unreadable).
fn load_section<'data>(
    file: &object::File<'data>,
    name: &str,
    endian: RunTimeEndian,
) -> Cow<'data, [u8]> {
    let Some(section) = file.section_by_name(name) else {
        return Cow::Borrowed(&[]);
    };
    let data = section.uncompressed_data().unwrap_or(Cow::Borrowed(&[]));
    if section.relocations().next().is_none() {
        return data;
    }
    let mut buf = data.into_owned();
    for (offset, reloc) in section.relocations() {
        let symbol_value = match reloc.target() {
            RelocationTarget::Symbol(index) => match file.symbol_by_index(index) {
                Ok(sym) => sym.address(),
                Err(_) => continue,
            },
            RelocationTarget::Section(_) => 0,
            _ => continue,
        };
        let Ok(at) = usize::try_from(offset) else {
            continue;
        };
        let width = usize::from(reloc.size() / 8);
        let Some(bytes) = at.checked_add(width).and_then(|end| buf.get_mut(at..end)) else {
            continue;
        };
        let addend = if reloc.has_implicit_addend() {
            read_uint(bytes, endian) as i64
        } else {
            reloc.addend()
        };
        let value = symbol_value.wrapping_add_signed(addend);
        write_uint(bytes, value, endian);
    }
    Cow::Owned(buf)
}

fn read_uint(bytes: &[u8], endian: RunTimeEndian) -> u64 {
    let mut v = 0u64;
    let mut push = |b: u8| v = (v << 8) | u64::from(b);
    match endian {
        RunTimeEndian::Little => bytes.iter().rev().copied().for_each(&mut push),
        RunTimeEndian::Big => bytes.iter().copied().for_each(&mut push),
    }
    v
}

fn write_uint(bytes: &mut [u8], value: u64, endian: RunTimeEndian) {
    let n = bytes.len();
    for (i, b) in bytes.iter_mut().enumerate() {
        let shift = match endian {
            RunTimeEndian::Little => i,
            RunTimeEndian::Big => n - 1 - i,
        } * 8;
        *b = value.checked_shr(shift as u32).unwrap_or(0) as u8;
    }
}

/// Name-related attributes of one DIE, following one level of
/// `DW_AT_specification` / `DW_AT_abstract_origin` for missing ones.
#[derive(Default)]
struct Names {
    name: Option<String>,
    linkage: Option<String>,
    decl_file: Option<u64>,
    decl_line: Option<u64>,
}

struct Cx<'a, 'd> {
    dwarf: &'a gimli::Dwarf<R<'d>>,
    unit: &'a gimli::Unit<R<'d>>,
}

impl<'d> Cx<'_, 'd> {
    fn walk_unit(&self, info: &mut DebugInfo) -> gimli::Result<()> {
        let mut tree = self.unit.entries_tree(None)?;
        let root = tree.root()?;
        self.walk(root, None, 0, info)
    }

    fn walk(
        &self,
        node: gimli::EntriesTreeNode<'_, '_, R<'d>>,
        scope: Option<&str>,
        depth: usize,
        info: &mut DebugInfo,
    ) -> gimli::Result<()> {
        if depth > MAX_DEPTH {
            return Ok(());
        }
        let mut children = node.children();
        while let Some(child) = children.next()? {
            let entry = child.entry();
            match entry.tag() {
                gimli::DW_TAG_subprogram => {
                    if is_flag_set(entry.attr_value(gimli::DW_AT_declaration)) {
                        continue;
                    }
                    let names = self.names(entry);
                    let Some(key) = names.linkage.clone().or(names.name.clone()) else {
                        continue;
                    };
                    info.functions.entry(key.clone()).or_insert(FuncInfo {
                        source: self.location(&names),
                        size: high_pc_length(entry),
                        locals: Vec::new(),
                    });
                    self.walk(child, Some(&key), depth + 1, info)?;
                }
                gimli::DW_TAG_formal_parameter | gimli::DW_TAG_variable => {
                    let names = self.names(entry);
                    let Some(name) = names.name.clone() else {
                        continue;
                    };
                    let var = VarInfo {
                        name,
                        type_name: self.type_name(entry, 0),
                        source: self.location(&names),
                    };
                    match scope {
                        Some(f) => {
                            if let Some(func) = info.functions.get_mut(f) {
                                func.locals.push(var);
                            }
                        }
                        None => {
                            let key = names.linkage.unwrap_or_else(|| var.name.clone());
                            info.globals.entry(key).or_insert(var);
                        }
                    }
                }
                gimli::DW_TAG_lexical_block => self.walk(child, scope, depth + 1, info)?,
                gimli::DW_TAG_namespace | gimli::DW_TAG_module => {
                    self.walk(child, None, depth + 1, info)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn string(&self, value: AttributeValue<R<'d>>) -> Option<String> {
        let s = self.dwarf.attr_string(self.unit, value).ok()?;
        Some(String::from_utf8_lossy(s.slice()).into_owned())
    }

    fn names(&self, entry: &gimli::DebuggingInformationEntry<R<'d>>) -> Names {
        let mut names = self.own_names(entry);
        if names.name.is_none() && names.linkage.is_none() {
            for attr in [gimli::DW_AT_specification, gimli::DW_AT_abstract_origin] {
                if let Some(AttributeValue::UnitRef(offset)) = entry.attr_value(attr) {
                    if let Ok(origin) = self.unit.entry(offset) {
                        let o = self.own_names(&origin);
                        names.name = names.name.or(o.name);
                        names.linkage = names.linkage.or(o.linkage);
                        names.decl_file = names.decl_file.or(o.decl_file);
                        names.decl_line = names.decl_line.or(o.decl_line);
                        break;
                    }
                }
            }
        }
        names
    }

    fn own_names(&self, entry: &gimli::DebuggingInformationEntry<R<'d>>) -> Names {
        let get = |at| entry.attr_value(at);
        Names {
            name: get(gimli::DW_AT_name).and_then(|v| self.string(v)),
            linkage: get(gimli::DW_AT_linkage_name)
                .or_else(|| get(gimli::DW_AT_MIPS_linkage_name))
                .and_then(|v| self.string(v)),
            decl_file: get(gimli::DW_AT_decl_file).and_then(|v| match v {
                AttributeValue::FileIndex(i) => Some(i),
                other => other.udata_value(),
            }),
            decl_line: get(gimli::DW_AT_decl_line).and_then(|v| v.udata_value()),
        }
    }

    fn location(&self, names: &Names) -> Option<Location> {
        let line = u32::try_from(names.decl_line?).ok()?;
        let header = self.unit.line_program.as_ref()?.header();
        let entry = header.file(names.decl_file?)?;
        let file = self.string(entry.path_name())?;
        let dir = entry.directory(header).and_then(|d| self.string(d));
        Some(Location {
            file: join_path(dir.as_deref(), &file),
            line,
        })
    }

    /// A readable name for the DIE's `DW_AT_type`, e.g. `int`, `const char*`.
    fn type_name(
        &self,
        entry: &gimli::DebuggingInformationEntry<R<'d>>,
        depth: usize,
    ) -> Option<String> {
        if depth > MAX_TYPE_DEPTH {
            return None;
        }
        let AttributeValue::UnitRef(offset) = entry.attr_value(gimli::DW_AT_type)? else {
            return None;
        };
        let ty = self.unit.entry(offset).ok()?;
        // Many producers (rustc in particular) name derived types directly.
        if let Some(name) = ty
            .attr_value(gimli::DW_AT_name)
            .and_then(|v| self.string(v))
        {
            return Some(name);
        }
        let inner = || self.type_name(&ty, depth + 1);
        match ty.tag() {
            gimli::DW_TAG_pointer_type => Some(format!("{}*", inner().unwrap_or("void".into()))),
            gimli::DW_TAG_reference_type | gimli::DW_TAG_rvalue_reference_type => {
                inner().map(|t| format!("{t}&"))
            }
            gimli::DW_TAG_const_type => Some(format!("const {}", inner().unwrap_or("void".into()))),
            gimli::DW_TAG_volatile_type | gimli::DW_TAG_restrict_type => inner(),
            gimli::DW_TAG_array_type => inner().map(|t| format!("{t}[]")),
            gimli::DW_TAG_subroutine_type => Some("fn".into()),
            gimli::DW_TAG_structure_type
            | gimli::DW_TAG_class_type
            | gimli::DW_TAG_union_type
            | gimli::DW_TAG_enumeration_type => Some("<anonymous>".into()),
            _ => None,
        }
    }
}

fn is_flag_set(value: Option<AttributeValue<R<'_>>>) -> bool {
    matches!(value, Some(AttributeValue::Flag(true)))
}

/// Code length from `DW_AT_high_pc` when it is encoded as an offset.
fn high_pc_length(entry: &gimli::DebuggingInformationEntry<R<'_>>) -> Option<u64> {
    match entry.attr_value(gimli::DW_AT_high_pc)? {
        AttributeValue::Addr(_) => None,
        other => other.udata_value(),
    }
}

fn join_path(dir: Option<&str>, file: &str) -> String {
    let absolute =
        file.starts_with('/') || file.starts_with('\\') || file.as_bytes().get(1) == Some(&b':');
    match dir {
        Some(d) if !absolute && !d.is_empty() && d != "." => format!("{d}/{file}"),
        _ => file.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uint_round_trips_both_endians() {
        for endian in [RunTimeEndian::Little, RunTimeEndian::Big] {
            let mut b = [0u8; 4];
            write_uint(&mut b, 0x1234_5678, endian);
            assert_eq!(read_uint(&b, endian), 0x1234_5678);
            let mut b = [0u8; 8];
            write_uint(&mut b, u64::MAX - 1, endian);
            assert_eq!(read_uint(&b, endian), u64::MAX - 1);
        }
    }

    #[test]
    fn join_path_keeps_absolute_and_skips_dot() {
        assert_eq!(join_path(Some("."), "src/graph.c"), "src/graph.c");
        assert_eq!(join_path(Some("/home/x"), "a.c"), "/home/x/a.c");
        assert_eq!(join_path(Some("/home/x"), "C:\\a.c"), "C:\\a.c");
        assert_eq!(join_path(None, "a.c"), "a.c");
    }
}
