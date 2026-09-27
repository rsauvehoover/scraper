//! Runs the config panel's logic tests (`tests/js/config_panel.test.js`)
//! under node, so `cargo test` covers the JavaScript that decides what a form
//! edit does to the document.
//!
//! Skipped, with a note, where node is not installed: nothing else in the
//! suite needs it. That makes this the same kind of gate as the tests that
//! need `db/`, and like them it passes silently when skipped, so check its
//! output if node's absence matters.

use std::process::Command;

#[test]
fn config_panel_logic_passes_under_node() {
    let has_node = Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !has_node {
        eprintln!("skipping config panel logic tests: node is not installed");
        return;
    }

    let out = Command::new("node")
        .args(["--test", "tests/js/config_panel.test.js"])
        .output()
        .expect("node ran once already");
    assert!(
        out.status.success(),
        "config panel logic tests failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
