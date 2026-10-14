//! Object-file engine: loads compiled object files and builds the code graph
//! consumed by the IDE and renderer.
//!
//! Stage 1 skeleton; parsing and graph types arrive in stage 3. This crate
//! must stay portable and free of any Windows dependency.
#![forbid(unsafe_code)]
