//! Layer A, part 2: relocations become edges from the symbol containing the
//! relocation site to the symbol it targets.

use object::{
    Architecture, Object, ObjectSection, ObjectSymbol, Relocation, RelocationKind,
    RelocationTarget, SectionKind, SymbolSection,
};

use crate::calls::CallSites;
use crate::graph::{Builder, EdgeKind};
use crate::symbols::{is_analysed_section, LayerA};

pub(crate) fn collect(file: &object::File<'_>, a: &mut LayerA, b: &mut Builder) {
    let x86_64 = file.architecture() == Architecture::X86_64;
    for section in file.sections() {
        if !is_analysed_section(&section) {
            continue;
        }
        let mut relocations = section.relocations().peekable();
        if relocations.peek().is_none() {
            continue;
        }
        let index = section.index();
        let data = section.data().unwrap_or(&[]);
        let calls = (x86_64 && section.kind() == SectionKind::Text).then(|| {
            let starts: Vec<u64> = a
                .spans
                .get(&index)
                .map(|spans| spans.iter().map(|s| s.start).collect())
                .unwrap_or_default();
            CallSites::analyze(data, &starts)
        });

        for (offset, reloc) in relocations {
            let Some(to) = target(file, a, b, &reloc, offset, data) else {
                continue;
            };
            let from = match a.owner(index, offset) {
                Some(owner) => owner,
                None => match a.section_node(b, file, index) {
                    Some(node) => node,
                    None => continue,
                },
            };
            let kind = if calls.as_ref().is_some_and(|c| c.contains(offset)) {
                EdgeKind::Calls
            } else {
                EdgeKind::References
            };
            // Self references are jump tables and the like; recursion is kept.
            if from == to && kind == EdgeKind::References {
                continue;
            }
            b.add_edge(from, to, kind);
        }
    }
}

/// Resolves a relocation to the node it points at. References through a
/// section symbol (or an unnamed local label) are mapped to the symbol that
/// owns the referenced offset, else to the section itself.
fn target(
    file: &object::File<'_>,
    a: &mut LayerA,
    b: &mut Builder,
    reloc: &Relocation,
    offset: u64,
    data: &[u8],
) -> Option<crate::graph::NodeId> {
    let (section, base) = match reloc.target() {
        RelocationTarget::Symbol(index) => {
            if let Some(&node) = a.symbols.get(&index) {
                return Some(node);
            }
            let sym = file.symbol_by_index(index).ok()?;
            match sym.section() {
                SymbolSection::Section(section) => (section, sym.address()),
                _ => return None,
            }
        }
        RelocationTarget::Section(section) => (section, 0),
        _ => return None,
    };
    let at = base.wrapping_add_signed(target_offset(reloc, offset, data));
    a.owner(section, at)
        .or_else(|| a.section_node(b, file, section))
}

/// Offset into the target the relocation refers to.
fn target_offset(reloc: &Relocation, offset: u64, data: &[u8]) -> i64 {
    if reloc.has_implicit_addend() {
        // COFF stores the addend in the instruction or data being patched.
        return read_implicit(data, offset, reloc.size());
    }
    let addend = reloc.addend();
    // ELF PC-relative addends include the -4 for the rest of the instruction.
    if matches!(
        reloc.kind(),
        RelocationKind::Relative | RelocationKind::PltRelative
    ) && reloc.size() == 32
    {
        addend.wrapping_add(4)
    } else {
        addend
    }
}

fn read_implicit(data: &[u8], offset: u64, bits: u8) -> i64 {
    let Ok(at) = usize::try_from(offset) else {
        return 0;
    };
    match bits {
        32 => data
            .get(at..at.saturating_add(4))
            .and_then(|b| b.try_into().ok())
            .map_or(0, |b| i64::from(i32::from_le_bytes(b))),
        64 => data
            .get(at..at.saturating_add(8))
            .and_then(|b| b.try_into().ok())
            .map_or(0, i64::from_le_bytes),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implicit_addend_reads_are_bounds_checked() {
        let data = [0x10, 0, 0, 0, 0xff];
        assert_eq!(read_implicit(&data, 0, 32), 0x10);
        assert_eq!(read_implicit(&data, 2, 32), 0);
        assert_eq!(read_implicit(&data, u64::MAX, 64), 0);
        assert_eq!(read_implicit(&data, 0, 16), 0);
    }
}
