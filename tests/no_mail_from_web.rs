//! The web process must never reach the path that mails every volume and
//! chapter to every configured destination, and only named files may reach
//! the manual send that replaces it.
//!
//! `send_epubs` (which calls `send_epub`) is called from exactly one place,
//! `generate_epubs_for_source` in src/epub.rs, which mails volumes and
//! chapters to every configured destination. A download handler that reached
//! that function would mail hundreds of chapters to every configured
//! recipient, and there is no recall. This is a source-level check rather
//! than a runtime one because the failure is irreversible: by the time a
//! runtime test observed it, the mail is sent.
//!
//! The web process's only mail is a manual send through `crate::mail::Mailer`,
//! to destinations chosen one by one rather than every configured
//! destination at once. Each rule below is a line scan of code text (what
//! comes before `//` on a line), against an allowlist of exact paths:
//!
//! - Nothing under `src/web` names `send_epub` (so `send_epubs` too),
//!   `generate_epubs_for_source`, or the `mail_send` crate.
//! - Across `src`, `generate_epubs_for_source(` appears only in `main.rs`
//!   (its call) and `epub.rs` (its definition).
//! - Under `src/web`, only `send.rs`, `send_jobs.rs` and `app.rs` name
//!   `Mailer`, `SmtpMailer` or `SendError` as whole words.
//! - Under `src/web`, only `send.rs` reads the `mailer` field (`.mailer`, as
//!   in `(state.mailer)(mail)`): the one place a mailer is made. `app.rs`
//!   declares and fills the field as `mailer:`, which this does not match.
//! - Under `src/web`, only `send.rs` (the one spawn) and `send_jobs.rs` (the
//!   definition and its tests) name `run_job`.
//! - Under `src/web`, only `send_plan.rs` (which builds recipients from the
//!   configuration) names `Recipient`, plus the tests module of
//!   `send_jobs.rs`. A whole-word match rather than `Recipient {`, so an
//!   alias or a constructor, which must name the type too, is caught.
//!
//! The last three exist because Rust calls a method on a `dyn Mailer` without
//! the trait in scope: a handler could send through the factory, the runner
//! or a hand-built recipient without naming any word in the third rule.

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

/// How much of an allowed file may match.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    Whole,
    /// Only inside the file's tests block: from a line that is exactly
    /// `mod tests {` right after a line that is exactly `#[cfg(test)]`
    /// (blank lines between allowed), to the next line that is exactly `}`
    /// at column 0. Anything else, code after the block included, is
    /// scanned.
    Tests,
}

/// Lines in `files` (rooted at `root`) whose code text, before `//`,
/// satisfies `matches`, except in the parts of files that `allowed` names by
/// their exact path relative to `root`.
fn offenders(
    root: &Path,
    files: &[PathBuf],
    allowed: &[(&str, Part)],
    matches: fn(&str) -> bool,
) -> Vec<String> {
    let mut found = Vec::new();
    for path in files {
        let relative = path.strip_prefix(root).unwrap_or(path);
        let part = allowed
            .iter()
            .find(|(name, _)| relative == Path::new(name))
            .map(|(_, part)| *part);
        if part == Some(Part::Whole) {
            continue;
        }
        let source = fs::read_to_string(path).unwrap();
        let mut in_tests = false;
        let mut previous = "";
        for (n, line) in source.lines().enumerate() {
            if !in_tests && line.trim() == "mod tests {" && previous == "#[cfg(test)]" {
                in_tests = true;
            }
            if !line.trim().is_empty() {
                previous = line.trim();
            }
            if in_tests {
                if line == "}" {
                    in_tests = false;
                }
                if part == Some(Part::Tests) {
                    continue;
                }
            }
            let code = line.split("//").next().unwrap_or("");
            if matches(code) {
                found.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
            }
        }
    }
    found
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

/// `send_epub` (which also matches `send_epubs`) or
/// `generate_epubs_for_source`.
fn reaches_send_everything(code: &str) -> bool {
    code.contains("send_epub") || code.contains("generate_epubs_for_source")
}

fn names_mail_send(code: &str) -> bool {
    contains_word(code, "mail_send")
}

fn calls_generate_epubs(code: &str) -> bool {
    code.contains("generate_epubs_for_source(")
}

fn names_mailer(code: &str) -> bool {
    ["Mailer", "SmtpMailer", "SendError"]
        .iter()
        .any(|id| contains_word(code, id))
}

/// A read of a field named `mailer`: `.mailer` not followed by a word
/// character, so `.mailer_count` would not match.
fn reads_mailer_field(code: &str) -> bool {
    code.match_indices(".mailer").any(|(i, m)| {
        !code[i + m.len()..].chars().next().is_some_and(is_word_char)
    })
}

fn names_run_job(code: &str) -> bool {
    contains_word(code, "run_job")
}

fn names_recipient(code: &str) -> bool {
    contains_word(code, "Recipient")
}

/// Files, relative to `src/web`, allowed to name the manual mailer.
const MAILER_ALLOWED: [(&str, Part); 3] =
    [("send.rs", Part::Whole), ("send_jobs.rs", Part::Whole), ("app.rs", Part::Whole)];

/// Files, relative to `src/web`, allowed to read the mailer factory.
const MAILER_FIELD_ALLOWED: [(&str, Part); 1] = [("send.rs", Part::Whole)];

/// Files, relative to `src/web`, allowed to name the job runner.
const RUN_JOB_ALLOWED: [(&str, Part); 2] = [("send.rs", Part::Whole), ("send_jobs.rs", Part::Whole)];

/// Files, relative to `src/web`, allowed to name `Recipient`.
const RECIPIENT_ALLOWED: [(&str, Part); 2] =
    [("send_plan.rs", Part::Whole), ("send_jobs.rs", Part::Tests)];

/// Files, relative to `src`, allowed to name `generate_epubs_for_source(`:
/// its one call site (`main.rs`) and its definition (`epub.rs`).
const GENERATE_EPUBS_ALLOWED: [(&str, Part); 2] = [("main.rs", Part::Whole), ("epub.rs", Part::Whole)];

/// A scratch directory holding `files` (relative path, content), and every
/// `.rs` file under it.
fn scratch(files: &[(&str, &str)]) -> (tempfile::TempDir, Vec<PathBuf>) {
    let dir = tempfile::tempdir().unwrap();
    for (name, content) in files {
        let path = dir.path().join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    let found = rs_files(dir.path());
    (dir, found)
}

#[test]
fn web_module_never_reaches_the_send_everything_path() {
    let root = Path::new("src/web");
    let files = rs_files(root);
    assert!(
        !files.is_empty(),
        "found no source files under src/web; the scan is broken, not the code"
    );

    let offenders = offenders(root, &files, &[], reaches_send_everything);

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

    let offenders = offenders(root, &files, &MAILER_ALLOWED, names_mailer);

    assert!(
        offenders.is_empty(),
        "only send.rs, send_jobs.rs and app.rs may name the mailer:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn web_module_never_names_the_mail_send_crate() {
    let root = Path::new("src/web");
    let files = rs_files(root);

    let offenders = offenders(root, &files, &[], names_mail_send);

    assert!(
        offenders.is_empty(),
        "the web module must go through crate::mail::Mailer, not mail_send directly:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn only_send_rs_makes_a_mailer() {
    let root = Path::new("src/web");
    let files = rs_files(root);

    let offenders = offenders(root, &files, &MAILER_FIELD_ALLOWED, reads_mailer_field);

    assert!(
        offenders.is_empty(),
        "only send.rs may call the mailer factory:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn only_send_rs_starts_a_job() {
    let root = Path::new("src/web");
    let files = rs_files(root);

    let offenders = offenders(root, &files, &RUN_JOB_ALLOWED, names_run_job);

    assert!(
        offenders.is_empty(),
        "only send.rs and send_jobs.rs may name run_job:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn only_send_plan_rs_builds_a_recipient() {
    let root = Path::new("src/web");
    let files = rs_files(root);

    let offenders = offenders(root, &files, &RECIPIENT_ALLOWED, names_recipient);

    assert!(
        offenders.is_empty(),
        "only send_plan.rs and send_jobs.rs's tests may name Recipient:\n{}",
        offenders.join("\n")
    );
}

/// Proves the `mail_send` check reaches subdirectories and matches whole
/// words, the same way `the_scan_would_catch_a_nested_offender` does for the
/// send-everything check.
#[test]
fn the_mail_send_check_would_catch_a_nested_offender() {
    let (dir, files) = scratch(&[
        ("ok.rs", "use crate::mail::Mailer;\n"),
        ("nested/offender.rs", "use mail_send::SmtpClientBuilder;\n"),
    ]);

    let offenders = offenders(dir.path(), &files, &[], names_mail_send);

    assert_eq!(offenders.len(), 1, "expected one offender, got {:?}", offenders);
    assert!(
        offenders[0].contains("offender.rs"),
        "the nested file must be the one reported: {}",
        offenders[0]
    );
}

/// The guard has to see into subdirectories, and this proves it does rather
/// than asserting it in a comment. `src/web` is flat today, so a real nested
/// offender would be invisible to the check above if the walk stopped at the
/// top level — and nothing about the passing result would look different.
#[test]
fn the_scan_would_catch_a_nested_offender() {
    let (dir, files) = scratch(&[
        ("ok.rs", "fn handler() {}\n"),
        ("handlers/deep/offender.rs", "fn handler() {\n    crate::mail::send_epubs();\n}\n"),
    ]);
    assert_eq!(files.len(), 2, "the walk must reach nested files: {:?}", files);

    let offenders = offenders(dir.path(), &files, &[], reaches_send_everything);
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
    let check = |files: &[(&str, &str)]| {
        let (dir, found) = scratch(files);
        offenders(dir.path(), &found, &MAILER_ALLOWED, names_mailer)
    };

    // send.rs directly under the scanned root is allowed to name the mailer.
    let found = check(&[("send.rs", "fn f() -> Mailer { todo!() }\n")]);
    assert!(found.is_empty(), "send.rs should be allowed: {:?}", found);

    // nested/send.rs is a different path from send.rs: the allowlist is exact.
    let found = check(&[("nested/send.rs", "fn f() -> Mailer { todo!() }\n")]);
    assert_eq!(found.len(), 1, "nested/send.rs must not be allowed: {:?}", found);

    // A disallowed file naming SmtpMailer is caught.
    let found = check(&[("other.rs", "fn f() -> SmtpMailer { todo!() }\n")]);
    assert_eq!(found.len(), 1, "SmtpMailer must be caught: {:?}", found);

    // RecordingMailer is not a whole-word match for Mailer.
    let found = check(&[("other.rs", "fn f() -> RecordingMailer { todo!() }\n")]);
    assert!(found.is_empty(), "RecordingMailer must not match Mailer: {:?}", found);

    // Mailer named only in a comment does not count.
    let found = check(&[("other.rs", "fn f() {} // Mailer\n")]);
    assert!(found.is_empty(), "a comment-only mention must not count: {:?}", found);
}

/// The factory call is caught outside send.rs however it is spelled, a
/// field declaration is not, and a longer field name is not a match.
#[test]
fn the_mailer_factory_check_catches_a_call_elsewhere() {
    let (dir, files) = scratch(&[
        ("send.rs", "let m = (state.mailer)(mail);\n"),
        ("app.rs", "pub mailer: Factory,\nmailer: Arc::new(make),\n"),
        ("handler.rs", "(state.mailer)(mail).send(&to, &att).await;\n"),
        ("nested/send.rs", "let f = Arc::clone(&s.mailer);\n"),
        ("other.rs", "let n = state.mailer_count;\n"),
    ]);

    let mut found = offenders(dir.path(), &files, &MAILER_FIELD_ALLOWED, reads_mailer_field);
    found.sort();

    assert_eq!(found.len(), 2, "expected handler.rs and nested/send.rs: {:?}", found);
    assert!(found[0].contains("handler.rs"), "{:?}", found);
    assert!(found[1].contains("send.rs"), "{:?}", found);
}

#[test]
fn the_run_job_check_catches_a_call_elsewhere() {
    let (dir, files) = scratch(&[
        ("send.rs", "tokio::spawn(run_job(jobs));\n"),
        ("send_jobs.rs", "pub(crate) async fn run_job() {}\n"),
        ("handler.rs", "tokio::spawn(send_jobs::run_job(jobs));\n"),
        ("other.rs", "fn f() {} // run_job\nfn rerun_jobs() {}\n"),
    ]);

    let found = offenders(dir.path(), &files, &RUN_JOB_ALLOWED, names_run_job);

    assert_eq!(found.len(), 1, "expected only handler.rs: {:?}", found);
    assert!(found[0].contains("handler.rs"), "{:?}", found);
}

/// Fails closed: the exemption covers only a `#[cfg(test)] mod tests { }`
/// block, not code after it, and not a look-alike line such as
/// `mod tests_support;`.
#[test]
fn the_tests_exemption_ends_at_the_block() {
    let check = |content: &str| {
        let (dir, files) = scratch(&[("send_jobs.rs", content)]);
        offenders(dir.path(), &files, &RECIPIENT_ALLOWED, names_recipient)
    };

    let inside = "fn f() {}\n\n#[cfg(test)]\nmod tests {\n    fn t() { let r = Recipient { name, email }; }\n}\n";
    let found = check(inside);
    assert!(found.is_empty(), "inside the tests block is allowed: {:?}", found);

    let after = format!("{}\nfn g() {{ let r = Recipient {{ name, email }}; }}\n", inside);
    let found = check(&after);
    assert_eq!(found.len(), 1, "code after the tests block is scanned: {:?}", found);
    assert!(found[0].contains("send_jobs.rs:8:"), "{:?}", found);

    let look_alike = "mod tests_support;\nfn g() { let r = Recipient { name, email }; }\n";
    let found = check(look_alike);
    assert_eq!(found.len(), 1, "mod tests_support; starts no exemption: {:?}", found);

    let no_cfg = "mod tests {\n    fn t() { let r = Recipient { name, email }; }\n}\n";
    let found = check(no_cfg);
    assert_eq!(found.len(), 1, "mod tests without #[cfg(test)] starts no exemption: {:?}", found);
}

/// send_jobs.rs may name `Recipient` in its tests module only, and
/// send_plan.rs anywhere.
#[test]
fn the_recipient_check_catches_one_built_elsewhere() {
    let (dir, files) = scratch(&[
        ("send_plan.rs", "to: Recipient { name, email },\n"),
        (
            "send_jobs.rs",
            "fn f() { let r = Recipient { name, email }; }\n\
             #[cfg(test)]\nmod tests {\n    use crate::mail::Recipient;\n}\n",
        ),
        ("handler.rs", "use crate::mail::Recipient as R;\n"),
    ]);

    let mut found = offenders(dir.path(), &files, &RECIPIENT_ALLOWED, names_recipient);
    found.sort();

    assert_eq!(found.len(), 2, "expected handler.rs and send_jobs.rs line 1: {:?}", found);
    assert!(found[0].contains("handler.rs"), "{:?}", found);
    assert!(found[1].contains("send_jobs.rs:1:"), "{:?}", found);
}

/// `web_module_never_reaches_the_send_everything_path` only scans
/// `src/web`. A library wrapper around `generate_epubs_for_source` placed
/// anywhere else — say `src/epub_wrappers.rs` — would let web code reach
/// the mail-everything path through that wrapper's name instead, which the
/// web-only scan never looks for. Pinning every call of
/// `generate_epubs_for_source` across all of `src` to its one real call
/// site and its definition closes that gap: a new wrapper anywhere would
/// show up here before it could be reached from `src/web`.
#[test]
fn generate_epubs_for_source_has_only_its_call_and_definition() {
    let root = Path::new("src");
    let files = rs_files(root);

    let offenders = offenders(root, &files, &GENERATE_EPUBS_ALLOWED, calls_generate_epubs);

    assert!(
        offenders.is_empty(),
        "generate_epubs_for_source must appear only in main.rs (the call) and epub.rs (the definition):\n{}",
        offenders.join("\n")
    );
}

/// Proves the allowlist above is exact and the walk reaches every file, the
/// same way `the_scan_would_catch_a_nested_offender` does for the
/// send-everything check.
#[test]
fn the_generate_epubs_allowlist_catches_an_offender_elsewhere() {
    let (dir, files) = scratch(&[
        ("main.rs", "fn main() { generate_epubs_for_source(); }\n"),
        ("epub.rs", "pub async fn generate_epubs_for_source() {}\n"),
        ("wrapper.rs", "async fn also_send() { generate_epubs_for_source(); }\n"),
    ]);

    let offenders = offenders(dir.path(), &files, &GENERATE_EPUBS_ALLOWED, calls_generate_epubs);

    assert_eq!(offenders.len(), 1, "expected only wrapper.rs flagged: {:?}", offenders);
    assert!(
        offenders[0].contains("wrapper.rs"),
        "the wrapper file must be the one reported: {:?}",
        offenders
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
