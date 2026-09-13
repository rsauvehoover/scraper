//! Runs the sanitiser over every stored chapter. Skipped when db/ is absent.

use std::path::Path;

use wandering_inn_scraper::web::sanitize::sanitize_chapter;

#[test]
fn no_active_content_survives_sanitisation() {
    if !Path::new("db").is_dir() {
        eprintln!("SKIP: db/ absent (expected in CI)");
        return;
    }

    let mut checked = 0usize;
    let mut had_active = 0usize;

    let mut paths: Vec<_> = std::fs::read_dir("db")
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension().map_or(false, |e| e == "db")
                && !p.file_name().unwrap().to_string_lossy().ends_with(".bak.db")
        })
        .collect();
    paths.sort();

    for path in paths {
        let conn = rusqlite::Connection::open(&path).unwrap();
        let mut stmt = conn.prepare("SELECT data FROM raw_data").unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, Option<String>>(0)).unwrap();

        for html in rows.filter_map(|r| r.ok()).flatten() {
            let lower = html.to_lowercase();
            if lower.contains("<script") || lower.contains("<iframe") {
                had_active += 1;
            }
            let clean = sanitize_chapter(&html).to_lowercase();
            assert!(!clean.contains("<script"), "script survived in {}", path.display());
            assert!(!clean.contains("<iframe"), "iframe survived in {}", path.display());
            assert!(!clean.contains("javascript:"), "js url survived in {}", path.display());
            checked += 1;
        }
    }

    println!(
        "sanitised {} chapters; {} contained active content",
        checked, had_active
    );
    assert!(checked > 0, "no chapters were checked");
    assert!(
        had_active > 0,
        "expected some chapters to contain script/iframe — if this is 0 the corpus changed \
         and this test is no longer proving anything"
    );
}
