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

#[test]
fn web_module_never_references_mail() {
    let mut offenders = Vec::new();

    for entry in fs::read_dir("src/web").expect("src/web must exist") {
        let path = entry.unwrap().path();
        if path.extension().map_or(true, |e| e != "rs") {
            continue;
        }
        let source = fs::read_to_string(&path).unwrap();
        for (n, line) in source.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if code.contains("send_epubs")
                || code.contains("generate_epubs_for_source")
                || code.contains("crate::mail")
                || code.contains("use crate::mail")
            {
                offenders.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "the web module must not reach the mail path:\n{}",
        offenders.join("\n")
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
