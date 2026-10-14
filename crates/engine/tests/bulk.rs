//! Loads every `.o`/`.obj` under `$LATTICE_BULK_DIR` (e.g. a build's
//! `target` folder) and reports failures and timing. Real compiler output
//! catches cases the small fixtures do not.
//!
//! `LATTICE_BULK_DIR=target cargo test -p engine --release --test bulk -- --ignored --nocapture`

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use engine::{DebugFormat, EdgeKind, NodeKind};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("o") || e.eq_ignore_ascii_case("obj"))
        {
            out.push(path);
        }
    }
}

#[test]
#[ignore = "needs LATTICE_BULK_DIR"]
fn bulk_load() {
    let Some(dir) = std::env::var_os("LATTICE_BULK_DIR") else {
        println!("LATTICE_BULK_DIR not set; nothing to do");
        return;
    };
    let mut files = Vec::new();
    collect(Path::new(&dir), &mut files);

    let (mut ok, mut failed, mut calls, mut refs, mut with_debug) = (0, 0, 0usize, 0usize, 0);
    let (mut functions, mut with_source) = (0usize, 0usize);
    let mut slowest = (Duration::ZERO, PathBuf::new());
    let mut total_bytes = 0u64;
    let started = Instant::now();
    for path in &files {
        total_bytes += std::fs::metadata(path).map_or(0, |m| m.len());
        let t = Instant::now();
        let result = engine::load(path);
        let elapsed = t.elapsed();
        if elapsed > slowest.0 {
            slowest = (elapsed, path.clone());
        }
        match result {
            Ok(g) => {
                ok += 1;
                if g.debug != DebugFormat::None {
                    with_debug += 1;
                    for f in g.nodes_of(NodeKind::Function) {
                        functions += 1;
                        with_source += usize::from(f.source.is_some());
                    }
                }
                calls += g
                    .edges()
                    .iter()
                    .filter(|e| e.kind == EdgeKind::Calls)
                    .count();
                refs += g
                    .edges()
                    .iter()
                    .filter(|e| e.kind == EdgeKind::References)
                    .count();
                assert!(
                    g.nodes_of(NodeKind::Function).count() > 0
                        || g.nodes_of(NodeKind::Variable).count() > 0
                        || g.nodes().is_empty()
                        || g.nodes_of(NodeKind::External).count() > 0,
                    "{}",
                    path.display()
                );
            }
            Err(e) => {
                failed += 1;
                println!("FAIL {e}");
            }
        }
    }
    println!(
        "{} files ({:.1} MB): {ok} ok, {failed} failed, {with_debug} with debug info, \
         {calls} call edges, {refs} reference edges, {with_source}/{functions} functions \
         with source in {:.2?}; slowest {:.2?} {}",
        files.len(),
        total_bytes as f64 / 1e6,
        started.elapsed(),
        slowest.0,
        slowest.1.display()
    );
}
