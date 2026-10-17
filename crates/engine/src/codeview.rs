//! Layer B for MSVC `/Z7` objects: CodeView in `.debug$S` (symbols, lines)
//! and `.debug$T` (types).
//!
//! The `pdb` crate reads PDB files only (its record parser is not public),
//! so the C13 subsections and the handful of records needed here are
//! decoded directly. Records are tied to symbols through the `.debug$S`
//! relocations (the `SECREL` on each record's code offset), because C++
//! procedure names in CodeView are undecorated while symbols are mangled.
//! Types are resolved to names only. `/Zi` objects keep their types and
//! symbols in an external PDB and are not read.

use std::collections::HashMap;

use crate::debug::{is_library_path, ClassInfo, DebugInfo, Location, VarInfo};
use object::{Object, ObjectSection, ObjectSymbol, RelocationTarget};

const CV_SIGNATURE_C13: u32 = 4;
const DEBUG_S_SYMBOLS: u32 = 0xF1;
const DEBUG_S_LINES: u32 = 0xF2;
const DEBUG_S_STRINGTABLE: u32 = 0xF3;
const DEBUG_S_FILECHKSMS: u32 = 0xF4;
/// Subsection kinds with this bit set are to be ignored.
const DEBUG_S_IGNORE: u32 = 0x8000_0000;

const S_BPREL32: u16 = 0x110b;
const S_LDATA32: u16 = 0x110c;
const S_GDATA32: u16 = 0x110d;
const S_REGREL32: u16 = 0x1111;
const S_LOCAL: u16 = 0x113e;

/// Procedure records (S_LPROC32, S_GPROC32 and their _ID/_DPC variants).
const PROC_KINDS: &[u16] = &[0x110f, 0x1110, 0x1146, 0x1147, 0x1155, 0x1156];
/// Other records that open a scope closed by S_END or a matching end record.
const SCOPE_KINDS: &[u16] = &[0x1102, 0x1103, 0x1132, 0x114d];
/// S_END, S_INLINESITE_END, S_PROC_ID_END.
const SCOPE_END_KINDS: &[u16] = &[0x0006, 0x114e, 0x114f];

/// Byte offset of the relocated code-offset field from the start of a
/// procedure record (including its length prefix).
const PROC_OFFSET_FIELD: u64 = 32;
/// Same for S_GDATA32/S_LDATA32.
const DATA_OFFSET_FIELD: u64 = 8;

pub(crate) fn read(file: &object::File<'_>, warnings: &mut Vec<String>) -> Option<DebugInfo> {
    let sections: Vec<_> = file
        .sections()
        .filter(|s| s.name().is_ok_and(|n| n == ".debug$S"))
        .collect();
    if sections.is_empty() {
        return None;
    }
    let types_data = file
        .sections()
        .find(|s| s.name().is_ok_and(|n| n == ".debug$T"))
        .and_then(|s| s.data().ok())
        .unwrap_or(&[]);
    let types = TypeTable::parse(types_data);

    let mut info = DebugInfo {
        classes: types.classes(),
        ..DebugInfo::default()
    };
    let mut strings: &[u8] = &[];
    let mut checksums: &[u8] = &[];
    let mut line_starts: Vec<(String, u32, u32)> = Vec::new();

    for section in sections {
        let Ok(data) = section.data() else {
            warnings.push("CodeView: unreadable .debug$S section".into());
            continue;
        };
        if read_u32(data, 0) != Some(CV_SIGNATURE_C13) {
            warnings.push("CodeView: .debug$S without a C13 signature".into());
            continue;
        }
        let relocs = relocation_targets(file, &section);
        let mut pos = 4usize;
        while let (Some(kind), Some(len)) = (read_u32(data, pos), read_u32(data, pos + 4)) {
            let start = pos + 8;
            let Some(body) = start
                .checked_add(len as usize)
                .and_then(|end| data.get(start..end))
            else {
                warnings.push("CodeView: truncated .debug$S subsection".into());
                break;
            };
            let base = start as u64;
            match kind & !DEBUG_S_IGNORE {
                _ if kind & DEBUG_S_IGNORE != 0 => {}
                DEBUG_S_SYMBOLS => {
                    read_symbols(body, base, &relocs, &types, &mut info);
                }
                DEBUG_S_LINES => read_lines(body, base, &relocs, &mut line_starts),
                DEBUG_S_STRINGTABLE => strings = body,
                DEBUG_S_FILECHKSMS => checksums = body,
                _ => {}
            }
            pos = (start + body.len() + 3) & !3;
        }
    }

    for (function, file_id, line) in line_starts {
        if let Some(path) = file_name(checksums, strings, file_id) {
            let func = info.functions.entry(function).or_default();
            if func.source.is_none() {
                let library = is_library_path(&path);
                func.source = Some(Location {
                    file: path,
                    line,
                    library,
                });
            }
        }
    }
    // Objects built without /Z7 still carry a .debug$S with only compiler
    // records; that is not debug info for our purposes.
    (!info.functions.is_empty() || !info.globals.is_empty()).then_some(info)
}

/// Section offset -> name of the symbol the relocation there points at.
fn relocation_targets(
    file: &object::File<'_>,
    section: &object::Section<'_, '_>,
) -> HashMap<u64, String> {
    section
        .relocations()
        .filter_map(|(offset, reloc)| match reloc.target() {
            RelocationTarget::Symbol(index) => {
                let sym = file.symbol_by_index(index).ok()?;
                Some((offset, sym.name().ok()?.to_owned()))
            }
            _ => None,
        })
        .collect()
}

fn read_symbols(
    body: &[u8],
    base: u64,
    relocs: &HashMap<u64, String>,
    types: &TypeTable<'_>,
    info: &mut DebugInfo,
) {
    // One entry per open scope: Some(function key) for procedures.
    let mut scopes: Vec<Option<String>> = Vec::new();
    let mut off = 0usize;
    while let Some(len) = read_u16(body, off) {
        let record_start = off;
        let Some(record) = body.get(off + 2..off + 2 + usize::from(len)) else {
            return;
        };
        off += 2 + usize::from(len);
        let Some(kind) = read_u16(record, 0) else {
            continue;
        };
        if SCOPE_END_KINDS.contains(&kind) {
            scopes.pop();
            continue;
        }
        // Record body after the kind field.
        let d = record.get(2..).unwrap_or(&[]);
        let at = |field: u64| base + record_start as u64 + field;
        if PROC_KINDS.contains(&kind) {
            // parent, end, next, len, dbg start, dbg end, type, offset,
            // segment (u16), flags (u8), name.
            let name = cstr(d, 35);
            let key = relocs
                .get(&at(PROC_OFFSET_FIELD))
                .cloned()
                .or_else(|| name.clone());
            if let (Some(key), Some(len)) = (&key, read_u32(d, 12)) {
                let func = info.functions.entry(key.clone()).or_default();
                func.size = Some(u64::from(len));
                // `Square::area`: ties methods to their class.
                func.qualified = name;
            }
            scopes.push(key);
            continue;
        }
        if SCOPE_KINDS.contains(&kind) {
            scopes.push(None);
            continue;
        }
        let current = scopes.iter().rev().find_map(Clone::clone);
        // (type index offset, name offset) within the record body.
        let local = match kind {
            S_LOCAL => Some((0, 6)),
            S_REGREL32 => Some((4, 10)),
            S_BPREL32 => Some((4, 8)),
            _ => None,
        };
        if let (Some((ty_at, name_at)), Some(function)) = (local, &current) {
            if let (Some(name), Some(func)) = (cstr(d, name_at), info.functions.get_mut(function)) {
                func.locals.push(VarInfo {
                    name,
                    type_name: read_u32(d, ty_at).and_then(|t| types.name(t, 0)),
                    source: None,
                });
            }
            continue;
        }
        if matches!(kind, S_GDATA32 | S_LDATA32) && current.is_none() {
            // type, offset, segment (u16), name.
            let Some(name) = cstr(d, 10) else {
                continue;
            };
            let key = relocs
                .get(&at(DATA_OFFSET_FIELD))
                .cloned()
                .unwrap_or_else(|| name.clone());
            info.globals.entry(key).or_insert(VarInfo {
                name,
                type_name: read_u32(d, 0).and_then(|t| types.name(t, 0)),
                source: None,
            });
        }
    }
}

/// Records the first line of a function's line block: (function, file id, line).
fn read_lines(
    body: &[u8],
    base: u64,
    relocs: &HashMap<u64, String>,
    out: &mut Vec<(String, u32, u32)>,
) {
    // Header: code offset (relocated), segment, flags, code size = 12 bytes.
    let Some(function) = relocs.get(&base) else {
        return;
    };
    let block = 12;
    let (Some(file_id), Some(count)) = (read_u32(body, block), read_u32(body, block + 4)) else {
        return;
    };
    // Block header is 12 bytes; each line entry is (offset, flags).
    let Some(flags) = read_u32(body, block + 12 + 4) else {
        return;
    };
    if count > 0 {
        out.push((function.clone(), file_id, flags & 0x00FF_FFFF));
    }
}

/// Resolves a file id (offset into the checksum subsection) to its name.
fn file_name(checksums: &[u8], strings: &[u8], file_id: u32) -> Option<String> {
    let name_offset = read_u32(checksums, usize::try_from(file_id).ok()?)?;
    let start = usize::try_from(name_offset).ok()?;
    cstr(strings, start)
}

/// `.debug$T` records, indexed from 0x1000.
#[derive(Default)]
struct TypeTable<'a> {
    records: Vec<(u16, &'a [u8])>,
}

impl<'a> TypeTable<'a> {
    fn parse(data: &'a [u8]) -> Self {
        let mut records = Vec::new();
        if read_u32(data, 0) == Some(CV_SIGNATURE_C13) {
            let mut pos = 4usize;
            while let Some(len) = read_u16(data, pos) {
                let Some(record) = data.get(pos + 2..pos + 2 + usize::from(len)) else {
                    break;
                };
                if let Some(kind) = read_u16(record, 0) {
                    records.push((kind, record.get(2..).unwrap_or(&[])));
                }
                pos += 2 + usize::from(len);
            }
        }
        Self { records }
    }

    fn name(&self, index: u32, depth: usize) -> Option<String> {
        if depth > 8 {
            return None;
        }
        if index < 0x1000 {
            return primitive_name(index);
        }
        let &(kind, d) = self.records.get(usize::try_from(index - 0x1000).ok()?)?;
        let inner = |field: usize| read_u32(d, field).and_then(|t| self.name(t, depth + 1));
        match kind {
            // LF_MODIFIER: type, attributes (bit 0 = const)
            0x1001 => {
                let t = inner(0)?;
                Some(if read_u16(d, 4)? & 1 != 0 {
                    format!("const {t}")
                } else {
                    t
                })
            }
            // LF_POINTER
            0x1002 => Some(format!("{}*", inner(0).unwrap_or_else(|| "void".into()))),
            // LF_PROCEDURE, LF_MFUNCTION
            0x1008 | 0x1009 => Some("fn".into()),
            // LF_ARRAY
            0x1503 => inner(0).map(|t| format!("{t}[]")),
            // LF_CLASS, LF_STRUCTURE: 16 fixed bytes, size leaf, name
            0x1504 | 0x1505 => cstr(d, skip_numeric(d, 16)?),
            // LF_UNION: 8 fixed bytes, size leaf, name
            0x1506 => cstr(d, skip_numeric(d, 8)?),
            // LF_ENUM: 12 fixed bytes, name
            0x1507 => cstr(d, 12),
            _ => None,
        }
    }

    fn record(&self, index: u32) -> Option<(u16, &'a [u8])> {
        let i = usize::try_from(index.checked_sub(0x1000)?).ok()?;
        self.records.get(i).copied()
    }

    /// Class, struct and union definitions (not forward references) with
    /// their data members and, from `LF_UDT_SRC_LINE`, where they are
    /// declared. Library types are left out.
    fn classes(&self) -> HashMap<String, ClassInfo> {
        let mut lines: HashMap<u32, Location> = HashMap::new();
        for &(kind, d) in &self.records {
            // LF_UDT_SRC_LINE: type, source file (an LF_STRING_ID), line.
            if kind != LF_UDT_SRC_LINE {
                continue;
            }
            let (Some(udt), Some(src), Some(line)) =
                (read_u32(d, 0), read_u32(d, 4), read_u32(d, 8))
            else {
                continue;
            };
            let file = match self.record(src) {
                Some((LF_STRING_ID, s)) => cstr(s, 4),
                _ => None,
            };
            if let Some(file) = file {
                let library = is_library_path(&file);
                lines.insert(
                    udt,
                    Location {
                        file,
                        line,
                        library,
                    },
                );
            }
        }
        let mut out = HashMap::new();
        for (i, &(kind, d)) in self.records.iter().enumerate() {
            // count, property, field list, then (class/struct) derived and
            // vshape, then the size leaf and the name.
            let fixed = match kind {
                0x1504 | 0x1505 => 16,
                0x1506 => 8,
                _ => continue,
            };
            let Some(property) = read_u16(d, 2) else {
                continue;
            };
            if property & PROP_FWDREF != 0 {
                continue;
            }
            let Some((size, name_at)) = read_numeric(d, fixed) else {
                continue;
            };
            let Some(name) = cstr(d, name_at).filter(|n| !n.starts_with('<')) else {
                continue;
            };
            let index = 0x1000 + i as u32;
            let source = lines.get(&index).cloned();
            if source.as_ref().is_some_and(|s| s.library) {
                continue;
            }
            let fields = read_u32(d, 4)
                .map(|list| self.fields(list))
                .unwrap_or_default();
            out.entry(name).or_insert(ClassInfo {
                source,
                size: Some(size),
                fields,
                methods: Vec::new(),
            });
        }
        out
    }

    /// Non-static data members (`LF_MEMBER`) of a field list, following
    /// `LF_INDEX` continuations.
    fn fields(&self, mut list: u32) -> Vec<VarInfo> {
        let mut out = Vec::new();
        for _ in 0..16 {
            let Some((LF_FIELDLIST, d)) = self.record(list) else {
                break;
            };
            let mut next = None;
            let mut pos = 0;
            while let Some(leaf) = read_u16(d, pos) {
                let body = pos + 2;
                // Offset just past this member, before padding.
                let end = match leaf {
                    // attributes, type, offset leaf, name
                    LF_MEMBER => {
                        let Some((_, name_at)) = read_numeric(d, body + 6) else {
                            break;
                        };
                        let Some(name) = cstr(d, name_at) else {
                            break;
                        };
                        let end = name_at + name.len() + 1;
                        out.push(VarInfo {
                            name,
                            type_name: read_u32(d, body + 2).and_then(|t| self.name(t, 0)),
                            source: None,
                        });
                        end
                    }
                    // attributes, base type, offset leaf
                    0x1400 => match read_numeric(d, body + 6) {
                        Some((_, end)) => end,
                        None => break,
                    },
                    // virtual bases: attributes, two types, two leaves
                    0x1401 | 0x1402 => {
                        match read_numeric(d, body + 10).and_then(|(_, p)| read_numeric(d, p)) {
                            Some((_, end)) => end,
                            None => break,
                        }
                    }
                    // LF_VFUNCTAB: pad, type
                    0x1409 => body + 6,
                    // LF_INDEX: pad, continuation list
                    0x1404 => {
                        next = read_u32(d, body + 2);
                        body + 6
                    }
                    // LF_ONEMETHOD: attributes, type, [vtable offset], name
                    0x1511 => {
                        let Some(attr) = read_u16(d, body) else {
                            break;
                        };
                        // Introducing virtual methods carry a vtable offset.
                        let intro = matches!((attr >> 2) & 7, 4 | 6);
                        let name_at = body + 6 + if intro { 4 } else { 0 };
                        match cstr(d, name_at) {
                            Some(n) => name_at + n.len() + 1,
                            None => break,
                        }
                    }
                    // LF_METHOD (count, list), LF_STMEMBER (attributes, type),
                    // LF_NESTTYPE (pad, type): then a name.
                    0x150e..=0x1510 => match cstr(d, body + 6) {
                        Some(n) => body + 6 + n.len() + 1,
                        None => break,
                    },
                    // LF_ENUMERATE: attributes, value leaf, name
                    0x1502 => match read_numeric(d, body + 2)
                        .and_then(|(_, p)| cstr(d, p).map(|n| p + n.len() + 1))
                    {
                        Some(end) => end,
                        None => break,
                    },
                    _ => break,
                };
                pos = end;
                // LF_PAD bytes (0xF0..=0xFF) align the next member.
                while d.get(pos).is_some_and(|&b| b >= 0xF0) {
                    pos += 1;
                }
            }
            match next {
                Some(n) => list = n,
                None => break,
            }
        }
        out
    }
}

const LF_FIELDLIST: u16 = 0x1203;
const LF_MEMBER: u16 = 0x150d;
const LF_STRING_ID: u16 = 0x1605;
const LF_UDT_SRC_LINE: u16 = 0x1606;
/// Class property bit: a forward reference, defined in another record.
const PROP_FWDREF: u16 = 0x0080;

/// Built-in CodeView types (index below 0x1000): low byte is the base type,
/// bits 8..12 the pointer mode.
fn primitive_name(index: u32) -> Option<String> {
    let base = match index & 0xff {
        0x03 => "void",
        0x08 => "HRESULT",
        0x10 => "signed char",
        0x11 => "short",
        0x12 => "long",
        0x13 => "__int64",
        0x20 => "unsigned char",
        0x21 => "unsigned short",
        0x22 => "unsigned long",
        0x23 => "unsigned __int64",
        0x30 => "bool",
        0x40 => "float",
        0x41 => "double",
        0x42 => "long double",
        0x68 => "int8_t",
        0x69 => "uint8_t",
        0x70 => "char",
        0x71 => "wchar_t",
        0x72 => "int16_t",
        0x73 => "uint16_t",
        0x74 => "int",
        0x75 => "unsigned int",
        0x76 => "int64_t",
        0x77 => "uint64_t",
        0x7a => "char16_t",
        0x7b => "char32_t",
        _ => return None,
    };
    Some(if (index >> 8) & 0xf != 0 {
        format!("{base}*")
    } else {
        base.to_owned()
    })
}

/// Skips a CodeView numeric leaf at `pos`, returning the following offset.
fn skip_numeric(d: &[u8], pos: usize) -> Option<usize> {
    read_numeric(d, pos).map(|(_, next)| next)
}

/// Reads a CodeView numeric leaf at `pos`: its value (signed kinds are
/// reinterpreted) and the offset after it.
fn read_numeric(d: &[u8], pos: usize) -> Option<(u64, usize)> {
    let leaf = read_u16(d, pos)?;
    let at = pos + 2;
    let le = |n: usize| -> Option<u64> {
        let bytes = d.get(at..at.checked_add(n)?)?;
        Some(
            bytes
                .iter()
                .rev()
                .fold(0u64, |v, &b| (v << 8) | u64::from(b)),
        )
    };
    let (value, extra) = match leaf {
        0..=0x7fff => (u64::from(leaf), 0),
        0x8000 => (le(1)?, 1),          // LF_CHAR
        0x8001 | 0x8002 => (le(2)?, 2), // LF_SHORT, LF_USHORT
        0x8003 | 0x8004 => (le(4)?, 4), // LF_LONG, LF_ULONG
        0x8009 | 0x800a => (le(8)?, 8), // LF_QUADWORD, LF_UQUADWORD
        _ => return None,
    };
    Some((value, at + extra))
}

fn cstr(d: &[u8], start: usize) -> Option<String> {
    let rest = d.get(start..)?;
    let end = rest.iter().position(|&b| b == 0)?;
    Some(String::from_utf8_lossy(&rest[..end]).into_owned())
}

fn read_u16(d: &[u8], at: usize) -> Option<u16> {
    let bytes = d.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes(bytes.try_into().ok()?))
}

fn read_u32(d: &[u8], at: usize) -> Option<u32> {
    let bytes = d.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitive_types_and_pointers() {
        assert_eq!(primitive_name(0x74).as_deref(), Some("int"));
        assert_eq!(primitive_name(0x0674).as_deref(), Some("int*"));
        assert_eq!(primitive_name(0xff), None);
    }

    #[test]
    fn numeric_leaves_and_strings_are_bounds_checked() {
        assert_eq!(skip_numeric(&[0x10, 0x00], 0), Some(2));
        assert_eq!(skip_numeric(&[0x04, 0x80, 1, 2, 3, 4], 0), Some(6));
        assert_eq!(skip_numeric(&[0x04], 0), None);
        assert_eq!(cstr(b"ab\0cd", 0).as_deref(), Some("ab"));
        assert_eq!(cstr(b"ab", 0), None);
        assert_eq!(read_u32(&[1, 2, 3], 0), None);
        assert_eq!(read_u32(&[1, 2, 3, 4], usize::MAX), None);
    }

    #[test]
    fn type_table_resolves_pointer_to_struct_and_const() {
        // 0x1000: LF_STRUCTURE "point" (size leaf 8)
        let mut t = vec![4, 0, 0, 0];
        let mut s = vec![0x05, 0x15];
        s.extend_from_slice(&[0; 16]);
        s.extend_from_slice(&[8, 0]);
        s.extend_from_slice(b"point\0");
        t.extend_from_slice(&(s.len() as u16).to_le_bytes());
        t.extend_from_slice(&s);
        // 0x1001: LF_POINTER -> 0x1000
        let p = [0x02, 0x10, 0x00, 0x10, 0, 0, 0, 0, 0, 0];
        t.extend_from_slice(&(p.len() as u16).to_le_bytes());
        t.extend_from_slice(&p);
        // 0x1002: LF_MODIFIER const -> 0x1001
        let m = [0x01, 0x10, 0x01, 0x10, 0, 0, 1, 0];
        t.extend_from_slice(&(m.len() as u16).to_le_bytes());
        t.extend_from_slice(&m);

        let table = TypeTable::parse(&t);
        assert_eq!(table.name(0x1000, 0).as_deref(), Some("point"));
        assert_eq!(table.name(0x1001, 0).as_deref(), Some("point*"));
        assert_eq!(table.name(0x1002, 0).as_deref(), Some("const point*"));
        assert_eq!(table.name(0x2000, 0), None);
    }
}
