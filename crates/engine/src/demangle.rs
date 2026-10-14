//! Display names. Linkage names stay the identity; this is cosmetic only.

/// Demangles Rust (legacy and v0), MSVC C++ and Itanium C++ names; anything
/// else is returned unchanged.
pub fn demangle(name: &str) -> String {
    if let Ok(d) = rustc_demangle::try_demangle(name) {
        // `{:#}` drops the trailing hash of legacy Rust symbols.
        return format!("{d:#}");
    }
    if name.starts_with('?') {
        if let Ok(d) = msvc_demangler::demangle(name, msvc_demangler::DemangleFlags::NAME_ONLY) {
            return d;
        }
    }
    if name.starts_with("_Z") {
        if let Ok(sym) = cpp_demangle::Symbol::new(name) {
            if let Ok(d) = sym.demangle() {
                return d;
            }
        }
    }
    name.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_legacy_and_v0() {
        assert_eq!(
            demangle("_ZN3lib15mangled_rust_fn17hcbce8c5a191a4b28E"),
            "lib::mangled_rust_fn"
        );
        assert_eq!(
            demangle("_RNvNtCsgEmfK2I1SDS_4core9panicking18panic_bounds_check"),
            "core::panicking::panic_bounds_check"
        );
    }

    #[test]
    fn msvc_cpp() {
        assert_eq!(demangle("?use_comdat@@YAHH@Z"), "use_comdat");
        assert_eq!(demangle("??$twice@H@@YAHH@Z"), "twice<int>");
    }

    #[test]
    fn itanium_cpp() {
        assert_eq!(demangle("_Z3fooi"), "foo(int)");
    }

    #[test]
    fn plain_c_and_garbage_pass_through() {
        assert_eq!(demangle("callee"), "callee");
        assert_eq!(demangle("?"), "?");
        assert_eq!(demangle("_Z"), "_Z");
    }
}
