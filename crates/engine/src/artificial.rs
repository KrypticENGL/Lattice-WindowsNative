//! Classifies symbols so consumers can keep them out of the way of user
//! code. They stay in the graph; nodes are only flagged.
//!
//! - Compiler-generated: RTTI, vtables, string literals, runtime-check data,
//!   static initialisers, EH helpers, synthesized deleting destructors.
//! - Library: code from the C++ or Rust standard library (by namespace),
//!   such as template instantiations and implicit members, which often have
//!   no source location to classify them by.

/// MSVC and Itanium prefixes of compiler-generated linkage names.
const PREFIXES: &[&str] = &[
    // MSVC: string literals, RTTI, vftables/vbtables, dynamic initialisers
    // and atexit destructors, floating-point/SIMD constants.
    "??_C@",
    "??_R",
    "??_7",
    "??_8",
    "??__E",
    "??__F",
    // MSVC scalar/vector deleting destructors (synthesized).
    "??_G",
    "??_E",
    "__real@",
    "__xmm@",
    "__ymm@",
    "__zmm@",
    "__mask@",
    // MSVC runtime checks, security cookie, EH and JMC helpers.
    "_RTC_",
    "__RTC_",
    "__security_",
    "__GSHandlerCheck",
    "__CxxFrameHandler",
    "__C_specific_handler",
    "__chkstk",
    "__CheckForDebuggerJustMyCode",
    "__JustMyCode",
    "_Init_thread_",
    // Itanium C++: vtables, VTTs, typeinfo and names, guard variables,
    // thunks; GCC static initialisers and EH personality.
    "_ZTV",
    "_ZTT",
    "_ZTI",
    "_ZTS",
    "_ZGV",
    "_ZTh",
    "_ZTv",
    "_ZTc",
    "_GLOBAL__sub_I_",
    "_GLOBAL__sub_D_",
    "__tcf_",
    "__gxx_personality",
    "__cxa_",
    "_Unwind_",
    "__dso_handle",
    "__stack_chk_",
];

/// Substrings that mark generated names anywhere in the symbol.
const INFIXES: &[&str] = &[
    // MSVC /RTC frame descriptors: `main$rtcFrameData`, `f$rtcName$0`.
    "$rtc",
    // MSVC function-local static used by __LINE__ in STL headers.
    "?__LINE__Var@",
    // GCC's per-TU static initialisation function and <iostream>'s object.
    "__static_initialization_and_destruction",
    // MSVC's sized-delete stub, emitted mangled: `?__empty_global_delete@@...`.
    "__empty_global_delete",
    "__ioinit",
];

/// Whole names that are compiler-inserted calls (MinGW's `__main` runs
/// global constructors at the start of `main`).
const EXACT: &[&str] = &["__main"];

pub(crate) fn is_artificial(linkage: &str) -> bool {
    EXACT.contains(&linkage)
        || PREFIXES.iter().any(|p| linkage.starts_with(p))
        || INFIXES.iter().any(|i| linkage.contains(i))
        || is_jmc_flag(linkage)
}

/// Namespaces of the C++ and Rust standard libraries, as display names
/// start (after an optional `<` for Rust trait impls).
const LIBRARY_NAMESPACES: &[&str] = &[
    "std::",
    "stdext::",
    "__gnu_cxx::",
    "__cxxabiv1::",
    "core::",
    "alloc::",
];

/// Whether a symbol belongs to the C++ or Rust standard library.
pub(crate) fn is_library_symbol(linkage: &str, display: &str) -> bool {
    let display = display.strip_prefix('<').unwrap_or(display);
    LIBRARY_NAMESPACES.iter().any(|ns| display.starts_with(ns)) || is_itanium_std(linkage)
}

/// Itanium names in `std` (`_ZSt...`, `_ZNKSt...`) or one of its
/// abbreviations (`Sa` allocator, `Sb` basic_string, `Ss` string, `Si`/`So`/
/// `Sd` streams), or in `__gnu_cxx`. Checked on the mangled name because
/// demangled template functions start with their return type.
fn is_itanium_std(linkage: &str) -> bool {
    let Some(rest) = linkage.strip_prefix("_Z") else {
        return false;
    };
    // `Z` introduces a local entity (`_ZZ<function>E<local>`): classify it
    // by its enclosing function.
    let rest = rest.strip_prefix('Z').unwrap_or(rest);
    let rest = rest.strip_prefix('N').unwrap_or(rest);
    let rest = rest.trim_start_matches(['K', 'V', 'r']);
    let bytes = rest.as_bytes();
    (bytes.first() == Some(&b'S') && bytes.get(1).is_some_and(|c| b"tabsiod".contains(c)))
        || rest.starts_with("9__gnu_cxx")
}

/// MSVC `/JMC` emits one flag byte per source file: `__5973A5D5_main@cpp`.
fn is_jmc_flag(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() > 11
        && name.starts_with("__")
        && bytes[2..10]
            .iter()
            .all(|b| matches!(b, b'0'..=b'9' | b'A'..=b'F'))
        && bytes[10] == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msvc_generated_names() {
        for name in [
            "??_C@_05CJBACGMB@hello@",
            "??_R4exception@std@@6B@",
            "??_7bad_alloc@std@@6B@",
            "??__Eglobal@@YAXXZ",
            "__real@4014000000000000",
            "main$rtcFrameData",
            "?max_size@?$basic_string@D@std@@QEBA_KXZ$rtcName$0",
            "__5973A5D5_main@cpp",
            "__FC61B0F5_cstdint",
            "?__LINE__Var@?0??_Adjust_manually_vector_aligned@std@@YAXAEAPEAXAEA_K@Z@4JA",
            "__CxxFrameHandler4",
            "?__empty_global_delete@@YAXPEAX_K@Z",
            "_RTC_InitBase.rtc$IMZ",
        ] {
            assert!(is_artificial(name), "{name}");
        }
    }

    #[test]
    fn itanium_generated_names() {
        for name in [
            "_ZTV6Square",
            "_ZTI5Shape",
            "_ZTS5Shape",
            "_ZGVZ4mainE1x",
            "_GLOBAL__sub_I_total",
            "_Z41__static_initialization_and_destruction_0v",
            "_ZStL8__ioinit",
            "__gxx_personality_seh0",
        ] {
            assert!(is_artificial(name), "{name}");
        }
    }

    #[test]
    fn standard_library_symbols() {
        for (linkage, display) in [
            ("??1bad_alloc@std@@UEAA@XZ", "std::bad_alloc::~bad_alloc"),
            ("__imp_?cout@std@@3V?$basic_ostream@D@1@A", "std::cout"),
            (
                "_ZSt4endlIcSt11char_traitsIcEERSt13basic_ostreamIT_T0_ES6_",
                "x",
            ),
            (
                "_ZNSt7__cxx1112basic_stringIcSt11char_traitsIcESaIcEE7_M_dataEv",
                "x",
            ),
            ("_ZNKSt9exception4whatEv", "x"),
            ("_ZNSolsEi", "x"),
            ("_ZN9__gnu_cxx13new_allocatorIcED2Ev", "x"),
            (
                "_ZZNSt7__cxx1112basic_stringIcSt11char_traitsIcESaIcEE12_M_constructIPKcEEvT_S8_St20forward_iterator_tagEN6_GuardD1Ev",
                "x",
            ),
            (
                "_ZN4core9panicking5panic17h0123456789abcdefE",
                "core::panicking::panic",
            ),
            ("x", "<alloc::vec::Vec<T> as core::ops::drop::Drop>::drop"),
        ] {
            assert!(is_library_symbol(linkage, display), "{linkage}");
        }
        for (linkage, display) in [
            ("?add@@YAHHH@Z", "add"),
            ("?greet@@YAXAEBV?$basic_string@D@std@@@Z", "greet"),
            ("_Z7measureRK5Shape", "measure(Shape const&)"),
            ("_Z3useSt6vectorIiSaIiEE", "use(std::vector<int>)"),
            ("main", "main"),
            ("x", "<lib::Foo as core::fmt::Debug>::fmt"),
        ] {
            assert!(!is_library_symbol(linkage, display), "{linkage}");
        }
    }

    #[test]
    fn user_symbols_are_not_artificial() {
        for name in [
            "main",
            "global_counter",
            "?add@@YAHHH@Z",
            "?greeting@@3PEBDEB",
            "_Z7measureRK5Shape",
            "_ZL5ratio",
            "__imp_?cout@std@@3V?$basic_ostream@DU?$char_traits@D@std@@@1@A",
            "_ZSt4cout",
            "__init_counter",
            "__ABCDEFGH_x",
        ] {
            assert!(!is_artificial(name), "{name}");
        }
    }
}
