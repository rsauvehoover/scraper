//! The web process must never reach the path that mails every volume and
//! chapter to every configured destination, and only named files may reach
//! the manual send that replaces it.
//!
//! `send_epubs`/`send_epub` are called from exactly one place,
//! `generate_epubs_for_source` in src/epub.rs, which mails volumes and
//! chapters to every configured destination. A download handler that reached
//! that function would mail hundreds of chapters to every configured
//! recipient, and there is no recall. This is a source-level check rather
//! than a runtime one because the failure is irreversible: by the time a
//! runtime test observed it, the mail is sent.
//!
//! The web process's only mail is a manual send through `crate::mail::Mailer`,
//! to destinations chosen one by one rather than every configured
//! destination at once. Only `src/web/send.rs`, `src/web/send_jobs.rs` and
//! `src/web/app.rs` may name `Mailer`, `SmtpMailer` or `SendError`; every
//! other file under `src/web` is checked against both rules.

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

/// Lines in `files` that reach the send-everything path: `send_epub` (which
/// also matches `send_epubs`) or `generate_epubs_for_source`, outside
/// comments.
fn send_everything_references(files: &[PathBuf]) -> Vec<String> {
    let mut offenders = Vec::new();
    for path in files {
        let source = fs::read_to_string(path).unwrap();
        for (n, line) in source.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if code.contains("send_epub") || code.contains("generate_epubs_for_source") {
                offenders.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
            }
        }
    }
    offenders
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// True if `identifier` occurs in `code` as a whole word: bounded on each
/// side by a character outside `[A-Za-z0-9_]`, or by the start/end of the
/// string. `Mailer` is therefore not a match inside `SmtpMailer` or
/// `RecordingMailer`.
fn contains_word(code: &str, identifier: &str) -> bool {
    let mut start = 0;
    while let Some(pos) = code[start..].find(identifier) {
        let idx = start + pos;
        let before_ok = idx == 0 || !is_word_char(code[..idx].chars().next_back().unwrap());
        let end = idx + identifier.len();
        let after_ok = end == code.len() || !is_word_char(code[end..].chars().next().unwrap());
        if before_ok && after_ok {
            return true;
        }
        start = idx + 1;
    }
    false
}

/// Files, relative to `src/web`, allowed to name the manual mailer.
const MAILER_ALLOWED: [&str; 3] = ["send.rs", "send_jobs.rs", "app.rs"];

/// Lines in `files` (rooted at `root`) that name `Mailer`, `SmtpMailer` or
/// `SendError` as whole words, outside comments, in a file whose path
/// relative to `root` is not exactly one of `allowed`.
fn mailer_references_outside(root: &Path, files: &[PathBuf], allowed: &[&str]) -> Vec<String> {
    let mut offenders = Vec::new();
    for path in files {
        let relative = path.strip_prefix(root).unwrap_or(path);
        if allowed.iter().any(|name| relative == Path::new(name)) {
            continue;
        }
        let source = fs::read_to_string(path).unwrap();
        for (n, line) in source.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if ["Mailer", "SmtpMailer", "SendError"]
                .iter()
                .any(|id| contains_word(code, id))
            {
                offenders.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
            }
        }
    }
    offenders
}

#[test]
fn web_module_never_reaches_the_send_everything_path() {
    let files = rs_files(Path::new("src/web"));
    assert!(
        !files.is_empty(),
        "found no source files under src/web; the scan is broken, not the code"
    );

    let offenders = send_everything_references(&files);

    assert!(
        offenders.is_empty(),
        "the web module must not reach the mail-everything path:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn only_the_send_files_name_the_mailer() {
    let root = Path::new("src/web");
    let files = rs_files(root);

    let offenders = mailer_references_outside(root, &files, &MAILER_ALLOWED);

    assert!(
        offenders.is_empty(),
        "only send.rs, send_jobs.rs and app.rs may name the mailer:\n{}",
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

    let offenders = send_everything_references(&files);
    assert_eq!(offenders.len(), 1, "expected one offender, got {:?}", offenders);
    assert!(
        offenders[0].contains("offender.rs"),
        "the nested file must be the one reported: {}",
        offenders[0]
    );
}

/// `only_the_send_files_name_the_mailer` allows exact paths, not filenames:
/// a nested `send.rs` is not the allowed `send.rs`. And the whole-word match
/// must neither be fooled by a longer identifier that contains one of the
/// three as a substring, nor by one that appears only in a comment.
#[test]
fn the_allowlist_is_exact() {
    // send.rs directly under the scanned root is allowed to name the mailer.
    {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("send.rs"), "fn f() -> Mailer { todo!() }\n").unwrap();
        let files = rs_files(dir.path());
        let offenders = mailer_references_outside(dir.path(), &files, &MAILER_ALLOWED);
        assert!(offenders.is_empty(), "send.rs should be allowed: {:?}", offenders);
    }

    // nested/send.rs is a different path from send.rs: the allowlist is exact.
    {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("send.rs"), "fn f() -> Mailer { todo!() }\n").unwrap();
        let files = rs_files(dir.path());
        let offenders = mailer_references_outside(dir.path(), &files, &MAILER_ALLOWED);
        assert_eq!(offenders.len(), 1, "nested/send.rs must not be allowed: {:?}", offenders);
    }

    // A disallowed file naming SmtpMailer is caught.
    {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("other.rs"), "fn f() -> SmtpMailer { todo!() }\n").unwrap();
        let files = rs_files(dir.path());
        let offenders = mailer_references_outside(dir.path(), &files, &MAILER_ALLOWED);
        assert_eq!(offenders.len(), 1, "SmtpMailer must be caught: {:?}", offenders);
    }

    // RecordingMailer is not a whole-word match for Mailer.
    {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("other.rs"), "fn f() -> RecordingMailer { todo!() }\n").unwrap();
        let files = rs_files(dir.path());
        let offenders = mailer_references_outside(dir.path(), &files, &MAILER_ALLOWED);
        assert!(offenders.is_empty(), "RecordingMailer must not match Mailer: {:?}", offenders);
    }

    // Mailer named only in a comment does not count.
    {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("other.rs"), "fn f() {} // Mailer\n").unwrap();
        let files = rs_files(dir.path());
        let offenders = mailer_references_outside(dir.path(), &files, &MAILER_ALLOWED);
        assert!(offenders.is_empty(), "a comment-only mention must not count: {:?}", offenders);
    }
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
