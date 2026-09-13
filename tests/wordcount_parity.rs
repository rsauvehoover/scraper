//! Proves the Rust and Python word counters agree on real data.
//!
//! Skipped when `db/` is absent, which is the case in CI: the databases
//! are gitignored. This test is a local verification step, and the
//! synthetic-fixture tests in src/stats/wordcount.rs are what CI runs.

use std::path::Path;
use std::process::Command;

use wandering_inn_scraper::stats::wordcount::count_words;

/// Total words across every non-backup database, using the Rust counter.
fn rust_total() -> (usize, usize) {
    let mut words = 0usize;
    let mut chapters = 0usize;

    let mut paths: Vec<_> = std::fs::read_dir("db")
        .expect("db/ must exist — guarded by the caller")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension().map_or(false, |e| e == "db")
                && !p.file_name().unwrap().to_string_lossy().ends_with(".bak.db")
        })
        .collect();
    paths.sort();

    for path in paths {
        let conn = rusqlite::Connection::open(&path).unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT rd.data FROM chapters c
                 JOIN volumes v ON v.id = c.volumeid
                 JOIN raw_data rd ON rd.chapter_id = c.id
                 ORDER BY v.id, c.id",
            )
            .unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        for html in rows {
            words += count_words(&html.unwrap());
            chapters += 1;
        }
    }
    (chapters, words)
}

#[test]
fn rust_and_python_counters_agree_on_real_data() {
    if !Path::new("db").is_dir() {
        eprintln!("SKIP: db/ absent (expected in CI — databases are gitignored)");
        return;
    }

    let output = Command::new("python3").arg("wordcount.py").output();
    let output = match output {
        Ok(o) if o.status.success() => o,
        Ok(o) => panic!("wordcount.py failed: {}", String::from_utf8_lossy(&o.stderr)),
        Err(e) => {
            eprintln!("SKIP: python3 unavailable ({})", e);
            return;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .find(|l| l.starts_with("ALL SOURCES"))
        .expect("wordcount.py must print an ALL SOURCES line");

    // "ALL SOURCES  2162  26,451,980"
    let nums: Vec<usize> = line
        .split_whitespace()
        .filter_map(|t| t.replace(',', "").parse::<usize>().ok())
        .collect();
    assert_eq!(nums.len(), 2, "unexpected ALL SOURCES format: {line}");
    let (py_chapters, py_words) = (nums[0], nums[1]);

    let (rs_chapters, rs_words) = rust_total();

    assert_eq!(rs_chapters, py_chapters, "chapter counts differ");
    assert_eq!(
        rs_words, py_words,
        "word counts differ: rust={rs_words} python={py_words}"
    );
}
