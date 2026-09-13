//! The web process must never mail anything.
//!
//! `send_epubs` is called from exactly one place: `generate_epubs_for_source`
//! in src/epub.rs, which mails volumes and chapters to every configured
//! destination. A download handler that reached that function would mail
//! hundreds of chapters to every configured recipient, and there is no recall.
//!
//! This is a source-level check rather than a runtime one because the failure
//! is irreversible: by the time a runtime test observed it, the mail is sent.

use std::fs;
use std::path::{Path, PathBuf};

/// Every `.rs` file under `dir`, at any depth.
///
/// Recursive on purpose. A flat `read_dir` scan covers `src/web/*.rs` and
/// silently covers nothing the day `src/web/download.rs` becomes
/// `src/web/download/mod.rs` — the directory entry has no `.rs` extension, so
/// a flat scan skips it and the guard keeps passing over code it never read.
/// For a check whose whole justification is that the failure cannot be undone,
/// "passes because it looked at nothing" is the worst failure mode.
fn rs_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {}", dir.display(), e))
    {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(rs_files(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            found.push(path);
        }
    }
    found.sort();
    found
}

/// Lines in `files` that name the mail path, outside comments.
fn mail_references(files: &[PathBuf]) -> Vec<String> {
    let mut offenders = Vec::new();
    for path in files {
        let source = fs::read_to_string(path).unwrap();
        for (n, line) in source.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if code.contains("send_epubs")
                || code.contains("generate_epubs_for_source")
                || code.contains("crate::mail")
            {
                offenders.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
            }
        }
    }
    offenders
}

#[test]
fn web_module_never_references_mail() {
    let files = rs_files(Path::new("src/web"));
    assert!(
        !files.is_empty(),
        "found no source files under src/web; the scan is broken, not the code"
    );

    let offenders = mail_references(&files);

    assert!(
        offenders.is_empty(),
        "the web module must not reach the mail path:\n{}",
        offenders.join("\n")
    );
}

/// The guard has to see into subdirectories, and this proves it does rather
/// than asserting it in a comment. `src/web` is flat today, so a real nested
/// offender would be invisible to the check above if the walk stopped at the
/// top level — and nothing about the passing result would look different.
#[test]
fn the_scan_would_catch_a_nested_offender() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("handlers").join("deep");
    fs::create_dir_all(&nested).unwrap();
    fs::write(dir.path().join("ok.rs"), "fn handler() {}\n").unwrap();
    fs::write(
        nested.join("offender.rs"),
        "fn handler() {\n    crate::mail::send_epubs();\n}\n",
    )
    .unwrap();

    let files = rs_files(dir.path());
    assert_eq!(files.len(), 2, "the walk must reach nested files: {:?}", files);

    let offenders = mail_references(&files);
    assert_eq!(offenders.len(), 1, "expected one offender, got {:?}", offenders);
    assert!(
        offenders[0].contains("offender.rs"),
        "the nested file must be the one reported: {}",
        offenders[0]
    );
}

#[test]
fn send_epubs_still_has_exactly_one_call_site() {
    // If this count changes, the guarantee above needs rechecking: a second
    // call site may be reachable from somewhere the first was not.
    let epub_rs = fs::read_to_string("src/epub.rs").unwrap();
    let calls = epub_rs
        .lines()
        .filter(|l| {
            let code = l.split("//").next().unwrap_or("");
            code.contains("send_epubs(")
        })
        .count();

    assert_eq!(
        calls, 1,
        "expected exactly one send_epubs call site in src/epub.rs, found {}", calls
    );
}
