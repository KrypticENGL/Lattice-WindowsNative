//! End to end: build `fixtures/shapes.cpp` with g++, stop it under GDB and
//! check the snapshot. Skipped (passes trivially) when g++ or GDB is
//! missing, so the suite needs no toolchain.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command as Process;
use std::sync::Arc;
use std::time::{Duration, Instant};

use debugger::{Cell, Command, DebugEvent, ObjectKind, Session, Snapshot, StartConfig};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shapes.cpp")
}

fn build(dir: &Path) -> Option<PathBuf> {
    let exe = dir.join("shapes.exe");
    let ok = Process::new("g++")
        .args(["-g", "-O0", "-o"])
        .arg(&exe)
        .arg(fixture())
        .status()
        .ok()?
        .success();
    ok.then_some(exe)
}

fn line_of(needle: &str) -> u32 {
    let src = std::fs::read_to_string(fixture()).unwrap();
    src.lines().position(|l| l.contains(needle)).unwrap() as u32 + 1
}

/// Events not yet consumed: one poll can return several.
struct Events<'a> {
    session: &'a Session,
    pending: VecDeque<DebugEvent>,
}

impl Events<'_> {
    fn wait_for<T>(&mut self, mut f: impl FnMut(DebugEvent) -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            self.pending.extend(self.session.poll());
            while let Some(e) = self.pending.pop_front() {
                match &e {
                    DebugEvent::Snapshot(_) => eprintln!("event: snapshot"),
                    other => eprintln!("event: {other:?}"),
                }
                if let Some(t) = f(e) {
                    return t;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("timed out waiting for GDB");
    }

    fn snapshot(&mut self) -> Snapshot {
        self.wait_for(|e| match e {
            DebugEvent::Snapshot(r) => Some(r.expect("snapshot parses")),
            _ => None,
        })
    }
}

fn target<'a>(snap: &'a Snapshot, cell: &Cell) -> &'a debugger::Object {
    &snap.objects[cell.target().expect("points somewhere")]
}

#[test]
fn stops_and_reads_shapes() {
    let Some(gdb) = debugger::find_gdb() else {
        eprintln!("skipped: no gdb");
        return;
    };
    let dir = std::env::temp_dir().join(format!("lattice-dbg-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(exe) = build(&dir) else {
        eprintln!("skipped: no g++");
        return;
    };
    let line = line_of("return total");
    let s = Session::start(
        StartConfig {
            gdb,
            program: exe,
            cwd: None,
            breakpoints: vec![(fixture(), line)],
        },
        Arc::new(|| {}),
    )
    .expect("gdb starts");

    let mut events = Events {
        session: &s,
        pending: VecDeque::new(),
    };
    let (func, at) = events.wait_for(|e| match e {
        DebugEvent::Stopped { func, line, .. } => Some((func, line)),
        DebugEvent::Exited(c) => panic!("exited early: {c:?}"),
        _ => None,
    });
    assert_eq!((func.as_str(), at), ("main", Some(line)));
    let snap = events.snapshot();
    let main = &snap.frames[0];
    let var = |name: &str| &main.vars.iter().find(|v| v.name == name).unwrap().value;

    // The BST: 50 with 30 on the left and 70 on the right.
    let root = target(&snap, var("root"));
    assert_eq!(root.kind, ObjectKind::Struct);
    let key = |o: &debugger::Object| match &o.fields[0].value {
        Cell::Scalar { v } => v.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(key(root), "50");
    assert_eq!(key(target(&snap, &root.fields[1].value)), "30");
    assert_eq!(key(target(&snap, &root.fields[2].value)), "70");

    // A vector is an array of its elements.
    let v = target(&snap, var("v"));
    assert_eq!((v.kind, v.fields.len()), (ObjectKind::Array, 6));
    // unique_ptr is drawn as the pointer it holds.
    assert!(matches!(var("tree"), Cell::Pointer { to: Some(_), .. }));
    // The cycle closes back on itself.
    let c0 = var("cyc").target().unwrap().to_string();
    let c1 = target(&snap, var("cyc"));
    let back = target(&snap, &c1.fields[1].value);
    assert_eq!(back.fields[1].value.target(), Some(c0.as_str()));

    // Stepping produces another stop and snapshot.
    s.send(Command::Next).unwrap();
    events.wait_for(|e| match e {
        DebugEvent::Stopped { .. } | DebugEvent::Exited(_) => Some(()),
        _ => None,
    });
    drop(events);
    s.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
