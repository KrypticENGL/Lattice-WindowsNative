//! Malformed input must produce an error, never a panic. Add a case here for
//! every parse bug found, so it stays fixed.

mod common;

use std::path::Path;

use common::{fixture, ALL_FIXTURES};
use engine::ErrorKind;

fn kind_of(bytes: &[u8]) -> Result<(), ErrorKind> {
    engine::load_bytes(bytes, Path::new("input.o"))
        .map(|_| ())
        .map_err(|e| e.kind)
}

#[test]
fn non_object_inputs_are_rejected() {
    assert_eq!(kind_of(b""), Err(ErrorKind::NotAnObject));
    assert_eq!(
        kind_of(b"fn main() { println!(\"not an object\"); }\n"),
        Err(ErrorKind::NotAnObject)
    );
    assert_eq!(kind_of(&[0u8; 64]), Err(ErrorKind::NotAnObject));
}

#[test]
fn missing_file_is_an_io_error() {
    let err = engine::load(&fixture("does-not-exist.obj")).expect_err("missing");
    assert_eq!(err.kind, ErrorKind::Io);
    assert!(err.to_string().contains("does-not-exist.obj"));
}

#[test]
fn elf_executable_is_unsupported() {
    let mut bytes = std::fs::read(fixture("elf_dwarf.o")).expect("fixture");
    // e_type at offset 16: ET_REL (1) -> ET_EXEC (2).
    bytes[16] = 2;
    assert_eq!(kind_of(&bytes), Err(ErrorKind::Unsupported));
}

#[test]
fn truncated_headers_are_errors() {
    for name in ALL_FIXTURES {
        let bytes = std::fs::read(fixture(name)).expect("fixture");
        for len in [1, 4, 16, 20, 40, 63] {
            assert!(
                kind_of(&bytes[..len.min(bytes.len())]).is_err(),
                "{name} cut to {len} bytes loaded"
            );
        }
    }
}

/// Every prefix length up to 256 bytes, then ~300 evenly spaced cuts.
#[test]
fn truncation_sweep_never_panics() {
    for name in ALL_FIXTURES {
        let bytes = std::fs::read(fixture(name)).expect("fixture");
        let step = (bytes.len() / 300).max(1);
        let lens = (0..bytes.len().min(256)).chain((256..bytes.len()).step_by(step));
        for len in lens {
            // Ok or Err are both fine; reaching the next iteration is the test.
            let _ = kind_of(&bytes[..len]);
        }
    }
}

/// Deterministic pseudo-random byte corruption (LCG, fixed seed).
#[test]
fn byte_flips_never_panic() {
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as usize
    };
    for name in ALL_FIXTURES {
        let original = std::fs::read(fixture(name)).expect("fixture");
        for _ in 0..200 {
            let mut bytes = original.clone();
            for _ in 0..(1 + next() % 8) {
                let at = next() % bytes.len();
                bytes[at] = (next() & 0xff) as u8;
            }
            let _ = kind_of(&bytes);
        }
    }
}
