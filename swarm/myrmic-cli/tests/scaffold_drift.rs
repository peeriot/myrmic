//! Pins the four copies of the scaffold cell against each other: the template
//! `myrmic new` renders, the `examples/counter` crate, and the listings in the
//! Quickstart and the cells guide. A reader who follows any of them has to end
//! up with the same cell.
//!
//! Like `myrmic-sdk`'s own doc-example guard, this reads files outside the
//! package directory. That works because the workspace is `publish = false` and
//! `myrmic-cli` is not in the published crate closure; if either changes, the
//! paths below stop resolving in a packaged build.

use std::path::{Path, PathBuf};

const COUNT_SIGNATURE: &str =
    "fn count(md: Metadata, callback: Option<Callback<JsonValue>>) -> myrmic_sdk::Result {";

const TEMPLATE: &str = "swarm/myrmic-cli/templates/new/src/lib.rs.tmpl";

const EXAMPLE: &str = "examples/counter/src/lib.rs";

#[test]
fn the_scaffold_copies_are_identical() {
    let template = read(TEMPLATE);

    for (path, copy) in [
        (EXAMPLE, read(EXAMPLE)),
        (
            "doc/chapters/01_quickstart.md",
            first_rust_fence("doc/chapters/01_quickstart.md"),
        ),
        (
            "doc/chapters/05_guides/01_cells.md",
            first_rust_fence("doc/chapters/05_guides/01_cells.md"),
        ),
    ] {
        assert_eq!(
            copy, template,
            "{path} has drifted from {TEMPLATE}; copy the template's current \
             contents into it"
        );
    }
}

#[test]
fn the_template_count_command_accepts_a_missing_callback() {
    assert!(
        read(TEMPLATE).contains(COUNT_SIGNATURE),
        "{TEMPLATE} must declare `{COUNT_SIGNATURE}`; a mandatory `Callback` \
         payload cannot be decoded from a command sent without one, so the \
         handler body never runs for `myrmic send <cell> count`"
    );
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);

    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{relative} must be readable: {e}"))
}

/// The body of a markdown file's first top-level rust code fence, with the
/// trailing newline the closing fence consumed put back. The fence marker is
/// named in prose rather than quoted, so this comment does not open one.
fn first_rust_fence(relative: &str) -> String {
    let text = read(relative);
    let (_, after_open) = text
        .split_once("\n```rust\n")
        .unwrap_or_else(|| panic!("{relative} has no top-level ```rust fence"));
    let (body, _) = after_open
        .split_once("\n```\n")
        .unwrap_or_else(|| panic!("{relative}'s first ```rust fence is not closed"));

    format!("{body}\n")
}
