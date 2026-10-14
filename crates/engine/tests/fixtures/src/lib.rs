//! Fixture for the ELF object (built for x86_64-unknown-linux-gnu): the same
//! shapes as `graph.c`. The bounds check in `caller` produces an unresolved
//! external (`core::panicking::panic_bounds_check`).
#![no_std]

use core::sync::atomic::{AtomicI32, Ordering};

pub static COUNTER: AtomicI32 = AtomicI32::new(0);

#[inline(never)]
#[no_mangle]
pub extern "C" fn helper(x: i32) -> i32 {
    x.wrapping_mul(3)
        .wrapping_add(COUNTER.load(Ordering::Relaxed))
}

#[inline(never)]
#[no_mangle]
pub extern "C" fn callee(a: i32, b: i32) -> i32 {
    let sum = a.wrapping_add(b);
    COUNTER.fetch_add(sum, Ordering::Relaxed);
    helper(sum)
}

#[no_mangle]
pub fn caller(values: &[i32], n: usize) -> i32 {
    let mut total: i32 = 0;
    for i in 0..n {
        total = total.wrapping_add(callee(values[i], i as i32));
    }
    total
}

#[no_mangle]
pub extern "C" fn read_counter() -> i32 {
    COUNTER.load(Ordering::Relaxed)
}

/// Keeps a Rust-mangled symbol in the object for the demangling test.
#[inline(never)]
pub fn mangled_rust_fn(x: i32) -> i32 {
    helper(x).wrapping_add(1)
}
