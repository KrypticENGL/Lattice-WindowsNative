//! Finding `gdb.exe`.

use std::path::{Path, PathBuf};

/// The GDB to run: `LATTICE_GDB` if set, else `gdb` on `PATH`, else the
/// usual MinGW-w64 and MSYS2 install folders.
pub fn find_gdb() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("LATTICE_GDB") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let exe = if cfg!(windows) { "gdb.exe" } else { "gdb" };
    if let Some(path) = std::env::var_os("PATH") {
        if let Some(p) = std::env::split_paths(&path)
            .map(|dir| dir.join(exe))
            .find(|p| p.is_file())
        {
            return Some(p);
        }
    }
    [
        r"C:\msys64\ucrt64\bin",
        r"C:\msys64\mingw64\bin",
        r"C:\msys64\clang64\bin",
        r"C:\ProgramData\mingw64\mingw64\bin",
        r"C:\mingw64\bin",
        r"C:\MinGW\bin",
    ]
    .iter()
    .map(|dir| Path::new(dir).join(exe))
    .find(|p| p.is_file())
}
