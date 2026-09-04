//! Pins the in-package copy of the crate-root doc example
//! (`src/doc_examples/counter.rs`, included verbatim into `lib.rs`'s doc
//! comment) against the real, compiled fixture crate
//! `tests/fixtures/cell-docs-counter`, which CI compiles for wasm32. A
//! doc-comment `include_str!` cannot reach the fixture directly: `cargo
//! package` extracts this crate into a standalone directory where anything
//! outside it no longer exists, so the doc comment includes the in-package
//! copy instead. This test is what keeps that copy from drifting out of sync
//! with the fixture it was cloned from.

use std::path::Path;

#[test]
fn the_in_package_doc_example_matches_the_compiled_fixture() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let copy = std::fs::read_to_string(crate_root.join("src/doc_examples/counter.rs"))
        .expect("src/doc_examples/counter.rs must exist");
    let fixture = std::fs::read_to_string(
        crate_root.join("../../tests/fixtures/cell-docs-counter/src/lib.rs"),
    )
    .expect("tests/fixtures/cell-docs-counter/src/lib.rs must exist");

    assert_eq!(
        copy, fixture,
        "src/doc_examples/counter.rs (included verbatim into the crate-root doc \
         comment) has drifted from tests/fixtures/cell-docs-counter/src/lib.rs \
         (the real cell CI compiles for wasm32); copy the fixture's current \
         contents into src/doc_examples/counter.rs to fix this"
    );
}
