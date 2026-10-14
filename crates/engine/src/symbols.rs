//! Layer A, part 1: symbols and sections become nodes, and each analysed
//! section gets sorted address spans so offsets can be mapped to the symbol
//! that owns them.

use std::collections::HashMap;

use object::{
    Object, ObjectSection, ObjectSymbol, SectionIndex, SectionKind, SymbolIndex, SymbolKind,
    SymbolSection,
};

use crate::graph::{Builder, EdgeKind, NodeId, NodeKind};

/// Section-name prefixes that carry metadata rather than program content
/// (unwind tables, linker directives, debug info, compiler notes).
const SKIPPED_PREFIXES: &[&str] = &[
    ".pdata",
    ".xdata",
    ".rdata$zzz",
    ".gfids",
    ".giats",
    ".gljmp",
    ".gehcont",
    ".sxdata",
    ".eh_frame",
    ".llvm",
    ".note",
    ".comment",
    ".debug",
    ".drectve",
    ".chks64",
    // MinGW auto-import indirection cells (see `REFPTR`).
    ".rdata$.refptr",
];

/// MinGW emits `.refptr.X`, a pointer cell holding `&X`, for symbols that
/// may come from a DLL. Code references the cell, so the cell is resolved
/// to `X` instead of becoming a node.
const REFPTR: &str = ".refptr.";

/// One symbol's byte range inside its section.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Span {
    pub(crate) start: u64,
    pub(crate) end: u64,
    pub(crate) node: NodeId,
}

#[derive(Default)]
pub(crate) struct LayerA {
    pub(crate) symbols: HashMap<SymbolIndex, NodeId>,
    pub(crate) spans: HashMap<SectionIndex, Vec<Span>>,
    sections: HashMap<SectionIndex, Option<NodeId>>,
}

/// Whether a section holds code or data worth graphing.
pub(crate) fn is_analysed_section(section: &object::Section<'_, '_>) -> bool {
    let Ok(name) = section.name() else {
        return false;
    };
    if SKIPPED_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return false;
    }
    matches!(
        section.kind(),
        SectionKind::Text
            | SectionKind::Data
            | SectionKind::ReadOnlyData
            | SectionKind::ReadOnlyDataWithRel
            | SectionKind::ReadOnlyString
            | SectionKind::UninitializedData
            | SectionKind::Tls
            | SectionKind::UninitializedTls
    )
}

/// Compiler-internal names: MSVC `$LN3`/`$unwind$f`, `@comp.id`, ELF `.L`
/// labels, and exception-handling funclets (`?dtor$7@?0?f@4HA`), which are
/// cleanup code of their parent function. Skipping a funclet folds its code
/// range, and the calls in it, into the function before it in the section.
fn is_noise_symbol(name: &str) -> bool {
    const FUNCLETS: &[&str] = &[
        "?dtor$",
        "?catch$",
        "?fin$",
        "?filt$",
        "__catch$",
        "__unwindfunclet$",
    ];
    name.is_empty()
        || name.starts_with('$')
        || name.starts_with('@')
        || name.starts_with(".L")
        || FUNCLETS.iter().any(|p| name.starts_with(p))
        // MSVC /ZI names cleanup funclets after their parent: `main$dtor$0`.
        || name.contains("$dtor$")
}

impl LayerA {
    /// The `Section` node for `index`, created on first use. `None` for
    /// sections that are not analysed.
    pub(crate) fn section_node(
        &mut self,
        b: &mut Builder,
        file: &object::File<'_>,
        index: SectionIndex,
    ) -> Option<NodeId> {
        if let Some(cached) = self.sections.get(&index) {
            return *cached;
        }
        let node = file
            .section_by_index(index)
            .ok()
            .filter(is_analysed_section)
            .and_then(|s| {
                let name = s.name().ok()?;
                // Section names repeat (one `.text$mn` per function), so the
                // index is part of the identity.
                let linkage = format!("{name}#{}", index.0);
                Some(b.add_node(NodeKind::Section, &linkage, Some(name)))
            });
        self.sections.insert(index, node);
        node
    }

    /// The symbol whose span contains `offset` in `section`.
    pub(crate) fn owner(&self, section: SectionIndex, offset: u64) -> Option<NodeId> {
        let spans = self.spans.get(&section)?;
        let i = spans.partition_point(|s| s.start <= offset);
        let span = spans.get(i.checked_sub(1)?)?;
        (offset < span.end).then_some(span.node)
    }
}

pub(crate) fn collect(file: &object::File<'_>, b: &mut Builder) -> LayerA {
    let mut a = LayerA::default();
    let mut placed: HashMap<SectionIndex, Vec<(u64, u64, NodeId)>> = HashMap::new();
    let mut refptrs: Vec<(SymbolIndex, String)> = Vec::new();

    for sym in file.symbols() {
        let Ok(name) = sym.name() else {
            continue;
        };
        if let Some(target) = name.strip_prefix(REFPTR) {
            refptrs.push((sym.index(), target.to_owned()));
            continue;
        }
        if is_noise_symbol(name)
            || matches!(
                sym.kind(),
                SymbolKind::Section | SymbolKind::File | SymbolKind::Label
            )
        {
            continue;
        }
        match sym.section() {
            SymbolSection::Undefined => {
                let id = b.add_node(NodeKind::External, name, None);
                a.symbols.insert(sym.index(), id);
            }
            SymbolSection::Common => {
                let id = b.add_node(NodeKind::Variable, name, None);
                b.set_size(id, sym.size());
                a.symbols.insert(sym.index(), id);
            }
            SymbolSection::Section(index) => {
                let Ok(section) = file.section_by_index(index) else {
                    continue;
                };
                if !is_analysed_section(&section) {
                    continue;
                }
                let kind = if sym.kind() == SymbolKind::Text || section.kind() == SectionKind::Text
                {
                    NodeKind::Function
                } else {
                    NodeKind::Variable
                };
                let id = b.add_node(kind, name, None);
                a.symbols.insert(sym.index(), id);
                if let Some(section_node) = a.section_node(b, file, index) {
                    b.add_edge(id, section_node, EdgeKind::DefinedIn);
                }
                placed
                    .entry(index)
                    .or_default()
                    .push((sym.address(), sym.size(), id));
            }
            _ => {}
        }
    }

    for (index, target) in refptrs {
        let existing = [NodeKind::Function, NodeKind::Variable, NodeKind::External]
            .into_iter()
            .map(|kind| b.id_for(kind, &target))
            .find(|&id| b.contains(id));
        let id = existing.unwrap_or_else(|| b.add_node(NodeKind::External, &target, None));
        a.symbols.insert(index, id);
    }

    for (index, mut syms) in placed {
        let section_size = file.section_by_index(index).map_or(0, |s| s.size());
        syms.sort_by_key(|&(addr, _, _)| addr);
        let mut spans = Vec::with_capacity(syms.len());
        for (i, &(start, size, node)) in syms.iter().enumerate() {
            // COFF symbols have no size: use the gap to the next symbol.
            let next = syms[i + 1..]
                .iter()
                .map(|&(addr, _, _)| addr)
                .find(|&addr| addr > start)
                .unwrap_or(section_size);
            let end = if size > 0 {
                start.saturating_add(size)
            } else {
                next.max(start)
            };
            b.set_size(node, end - start);
            spans.push(Span { start, end, node });
        }
        a.spans.insert(index, spans);
    }
    a
}
