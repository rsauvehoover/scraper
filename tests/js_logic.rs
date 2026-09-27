//! Runs the JavaScript logic tests in `tests/js/` under node, so `cargo test`
//! covers the code that decides what the config panel does to the document
//! and which table-of-contents volumes open.
//!
//! Skipped, with a note, where node is not installed: nothing else in the
//! suite needs it. That makes this the same kind of gate as the tests that
//! need `db/`, and like them it passes silently when skipped, so check its
//! output if node's absence matters.

use std::process::Command;

#[test]
fn javascript_logic_passes_under_node() {
    let has_node = Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !has_node {
        eprintln!("skipping JavaScript logic tests: node is not installed");
        return;
    }

    // Listed explicitly rather than handing node the directory, which older
    // node versions do not search the same way.
    let mut files: Vec<_> = std::fs::read_dir("tests/js")
        .expect("tests/js exists")
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with(".test.js"))
        .collect();
    files.sort();
    assert!(files.len() >= 2, "expected the panel and folding tests, found {:?}", files);

    let out = Command::new("node")
        .arg("--test")
        .args(&files)
        .output()
        .expect("node ran once already");
    assert!(
        out.status.success(),
        "JavaScript logic tests failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
