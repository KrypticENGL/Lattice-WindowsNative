//! Object-file engine: loads compiled object files (COFF `.obj`, ELF `.o`)
//! and builds the [`CodeGraph`] that the IDE and renderer consume.
//!
//! - Layer A (always): symbols, sections and relocations become function,
//!   variable, external and section nodes with `Calls`/`References`/`DefinedIn`
//!   edges. Calls are told apart from other references by decoding the
//!   x86-64 instruction at each relocation.
//! - Layer B (when present): DWARF or CodeView (`/Z7`) debug info adds source
//!   locations, sizes, parameters/locals (`Contains`) and types (`HasType`).
//!
//! Portable and UI-free: no Windows dependency, no threads, no `unsafe`.
//! Every function here is safe to call off the UI thread, and malformed
//! input yields an [`EngineError`], never a panic.
#![forbid(unsafe_code)]

mod artificial;
mod calls;
mod codeview;
mod debug;
mod demangle;
mod dwarf;
mod error;
mod graph;
mod id;
mod relocs;
mod symbols;

use std::path::Path;

use object::{BinaryFormat, FileKind, Object, ObjectKind};

pub use demangle::demangle;
pub use error::{EngineError, ErrorKind};
pub use graph::{
    CodeGraph, DebugFormat, Edge, EdgeKind, Node, NodeId, NodeKind, ObjectFormat, SourceLoc,
};

/// Reads and analyses the object file at `path`. Call again to reload;
/// node ids are stable, so callers can match old and new graphs by id.
pub fn load(path: &Path) -> Result<CodeGraph, EngineError> {
    let data =
        std::fs::read(path).map_err(|e| EngineError::new(ErrorKind::Io, path, e.to_string()))?;
    load_bytes(&data, path)
}

/// Analyses an object file already in memory. `path` is used for identity
/// (node ids) and error messages only.
pub fn load_bytes(data: &[u8], path: &Path) -> Result<CodeGraph, EngineError> {
    let file_kind = FileKind::parse(data)
        .map_err(|_| EngineError::new(ErrorKind::NotAnObject, path, "unrecognised file format"))?;
    match file_kind {
        FileKind::Coff | FileKind::CoffBig | FileKind::Elf32 | FileKind::Elf64 => {}
        other => {
            return Err(EngineError::new(
                ErrorKind::Unsupported,
                path,
                format!("{other:?} files are not supported"),
            ))
        }
    }
    let file = object::File::parse(data)
        .map_err(|e| EngineError::new(ErrorKind::Malformed, path, e.to_string()))?;
    if file.kind() != ObjectKind::Relocatable {
        return Err(EngineError::new(
            ErrorKind::Unsupported,
            path,
            format!(
                "{:?} file, not a relocatable object; only .obj/.o files are supported",
                file.kind()
            ),
        ));
    }
    let format = match file.format() {
        BinaryFormat::Elf => ObjectFormat::Elf,
        _ => ObjectFormat::Coff,
    };

    let mut b = graph::Builder::new(path.to_string_lossy().into_owned());
    let mut layer_a = symbols::collect(&file, &mut b);
    relocs::collect(&file, &mut layer_a, &mut b);

    let mut debug = DebugFormat::None;
    let mut warnings = Vec::new();
    if let Some(info) = dwarf::read(&file, &mut warnings) {
        info.apply(&mut b);
        debug = DebugFormat::Dwarf;
    } else if let Some(info) = codeview::read(&file, &mut warnings) {
        info.apply(&mut b);
        debug = DebugFormat::CodeView;
    }
    b.warnings.extend(warnings);

    let architecture = format!("{:?}", file.architecture());
    Ok(b.finish(path.to_path_buf(), format, architecture, debug))
}
