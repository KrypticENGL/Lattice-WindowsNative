//! `cargo test -p engine --test dump -- --ignored --nocapture` prints every
//! fixture's graph. Useful when regenerating fixtures.

mod common;

#[test]
#[ignore = "diagnostic output only"]
fn dump_all_fixtures() {
    for name in common::ALL_FIXTURES {
        println!("{}\n", common::describe(&common::load(name)));
    }
}
