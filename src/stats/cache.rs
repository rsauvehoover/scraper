//! Per-source statistics, computed once and cached until the database changes.
//!
//! The frontend never writes, so there is no `word_count` column to read: every
//! figure here is derived from `raw_data.data` on demand and memoised. A full
//! recompute of the largest source takes a few seconds, and the scraper only
//! touches a source when it has new chapters, so a cache keyed on a cheap
//! change-detection token (see `data_version` below — deliberately not the
//! database file's mtime) is enough.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::db::SourceEntry;
use crate::stats::wordcount::count_words;

#[derive(Debug, Clone)]
pub struct ChapterStat {
    pub id: isize,
    pub name: String,
    pub uri: String,
    pub words: usize,
    /// `YYYY-MM-DD`, only where the source's URIs carry one. `None` for
    /// Royal Road, which has no date anywhere in the stored data.
    pub published: Option<String>,
    /// Whether this chapter has a `raw_data` row yet. `false` for a chapter
    /// the TOC lists but the scraper has not downloaded — that case also has
    /// `words: 0`, so callers must check this field rather than `words == 0`
    /// to tell "not downloaded" apart from a genuinely empty chapter.
    pub downloaded: bool,
}

#[derive(Debug, Clone)]
pub struct VolumeStat {
    pub id: isize,
    pub name: String,
    pub chapters: Vec<ChapterStat>,
    pub words: usize,
    /// Chapters in `chapters` that the TOC lists but the scraper has not
    /// downloaded yet (no `raw_data` row). See `SourceStat::pending_chapters`.
    pub pending_chapters: usize,
}

#[derive(Debug, Clone)]
pub struct SourceStat {
    pub source_id: String,
    pub name: String,
    pub volumes: Vec<VolumeStat>,
    pub total_words: usize,
    pub total_chapters: usize,
    /// Chapters the TOC lists but the scraper has not downloaded yet (no
    /// `raw_data` row). Between `update_index` and `download_all_chapters`,
    /// and for any chapter whose download fails, such rows exist. They are
    /// kept in `VolumeStat::chapters` (with `words: 0`) so the TOC listing
    /// stays complete, but excluded from `total_words`, `total_chapters`,
    /// and `mean_chapter_words` — folding a pending chapter in as a
    /// zero-word chapter would make these statistics disagree with
    /// `wordcount.py`, which inner-joins `raw_data` and never sees it.
    pub pending_chapters: usize,
    pub mean_chapter_words: usize,
    pub latest_published: Option<String>,
}

/// Extract a `YYYY/MM/DD` path segment triple as `YYYY-MM-DD`.
///
/// WordPress sources (The Wandering Inn) put the publication date in the URI.
/// Royal Road does not, and nothing else in the database does either, so this
/// returns `None` there rather than guessing.
fn published_from_uri(uri: &str) -> Option<String> {
    let segments: Vec<&str> = uri
        .split('/')
        .filter(|s| !s.is_empty() && !s.contains(':'))
        .collect();

    for window in segments.windows(3) {
        let (y, m, d) = (window[0], window[1], window[2]);
        if y.len() != 4 || m.len() != 2 || d.len() != 2 {
            continue;
        }
        let (yn, mn, dn) = (
            y.parse::<u32>().ok()?,
            m.parse::<u32>().ok()?,
            d.parse::<u32>().ok()?,
        );
        if (1990..=2100).contains(&yn) && (1..=12).contains(&mn) && (1..=31).contains(&dn) {
            return Some(format!("{y}-{m}-{d}"));
        }
    }
    None
}

fn mean_words(total_words: usize, chapters: usize) -> usize {
    if chapters == 0 {
        0
    } else {
        total_words / chapters
    }
}

fn compute(entry: &SourceEntry) -> SourceStat {
    let db = entry.db();
    let conn = db.connection();
    let mut volumes: Vec<VolumeStat> = Vec::new();

    let mut vol_stmt = conn
        .prepare("SELECT id, name FROM volumes ORDER BY id")
        .expect("volumes query");
    let vol_rows: Vec<(isize, String)> = vol_stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("volumes query")
        .filter_map(|r| r.ok())
        .collect();

    for (vol_id, vol_name) in vol_rows {
        // LEFT JOIN, not INNER: a chapter the TOC lists but the scraper has
        // not downloaded yet still needs a row here, for the TOC listing.
        // It is kept out of the word/chapter totals below instead.
        let mut ch_stmt = conn
            .prepare(
                "SELECT c.id, c.name, c.uri, rd.data
                 FROM chapters c
                 LEFT JOIN raw_data rd ON rd.chapter_id = c.id
                 WHERE c.volumeid = ?1
                 ORDER BY c.id",
            )
            .expect("chapters query");

        let rows: Vec<(isize, String, String, Option<String>)> = ch_stmt
            .query_map([vol_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .expect("chapters query")
            .filter_map(|r| r.ok())
            .collect();

        let mut chapters: Vec<ChapterStat> = Vec::with_capacity(rows.len());
        let mut volume_words = 0usize;
        let mut volume_pending = 0usize;

        for (id, name, uri, data) in rows {
            let published = published_from_uri(&uri);
            let downloaded = data.is_some();
            let words = match &data {
                Some(html) => count_words(html),
                None => {
                    volume_pending += 1;
                    0
                }
            };
            volume_words += words;
            chapters.push(ChapterStat {
                id,
                name,
                uri,
                words,
                published,
                downloaded,
            });
        }

        volumes.push(VolumeStat {
            id: vol_id,
            name: vol_name,
            words: volume_words,
            pending_chapters: volume_pending,
            chapters,
        });
    }

    let total_words: usize = volumes.iter().map(|v| v.words).sum();
    let pending_chapters: usize = volumes.iter().map(|v| v.pending_chapters).sum();
    // Downloaded chapters only, matching wordcount.py's inner join on
    // raw_data — a pending chapter is listed (see `chapters` above) but
    // does not count toward totals until it has content.
    let total_chapters: usize = volumes
        .iter()
        .map(|v| v.chapters.len() - v.pending_chapters)
        .sum();
    let latest_published = volumes
        .iter()
        .flat_map(|v| v.chapters.iter())
        .filter_map(|c| c.published.clone())
        .max();

    SourceStat {
        source_id: entry.config.id.clone(),
        name: entry.config.name.clone(),
        total_words,
        total_chapters,
        pending_chapters,
        mean_chapter_words: mean_words(total_words, total_chapters),
        latest_published,
        volumes,
    }
}

/// Cheap change-detection token for a source's database.
///
/// NOT the file mtime: under WAL, commits land in the `-wal` sidecar and the
/// main database file is only touched at checkpoint. This process holds a
/// long-lived reader per source, so the "checkpoint on last close" path never
/// runs, and a routine scrape will not cross the auto-checkpoint threshold —
/// an mtime-keyed cache would serve pre-scrape numbers indefinitely, with no
/// error. `data_version` changes whenever another connection commits, which is
/// exactly the scraper-writes-while-we-read case.
fn data_version(entry: &SourceEntry) -> Option<i64> {
    entry
        .db()
        .connection()
        .query_row("PRAGMA data_version", [], |row| row.get(0))
        .ok()
}

struct Cached {
    version: Option<i64>,
    stat: Arc<SourceStat>,
}

/// Memoises `SourceStat` per source, invalidating when `PRAGMA data_version`
/// changes for that source's database. The scraper runs hourly and usually
/// touches one or two sources, so most refreshes are free.
pub struct StatsCache {
    inner: Mutex<HashMap<String, Cached>>,
}

impl StatsCache {
    pub fn new() -> Self {
        StatsCache {
            inner: Mutex::new(HashMap::new()),
        }
    }

    pub fn get(&self, entry: &SourceEntry) -> Arc<SourceStat> {
        let version = data_version(entry);

        // Check under the lock, then release it before the scan: a cache
        // miss for one source recomputing a full table scan must not block
        // every other source's statistics request for the duration.
        {
            let guard = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(cached) = guard.get(&entry.config.id) {
                if cached.version == version {
                    return Arc::clone(&cached.stat);
                }
            }
        }

        // Computed WITHOUT the map lock held. Two callers racing a miss on
        // the same source may each compute once; that is idempotent and far
        // cheaper than serialising every request behind one scan.
        let stat = Arc::new(compute(entry));

        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.insert(
            entry.config.id.clone(),
            Cached {
                version,
                stat: Arc::clone(&stat),
            },
        );
        stat
    }
}

impl Default for StatsCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Restores the process cwd on drop, including on unwind from a panic.
    /// Mirrors `db::connection`'s test helper of the same name; duplicated
    /// here rather than shared across modules since it is a handful of
    /// lines and not worth exposing from a non-test build.
    struct CwdGuard {
        original: std::path::PathBuf,
    }

    impl CwdGuard {
        fn change_to(dir: &std::path::Path) -> Self {
            let original = std::env::current_dir().unwrap();
            std::env::set_current_dir(dir).unwrap();
            CwdGuard { original }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.original);
        }
    }

    #[test]
    fn parses_wandering_inn_date_from_uri() {
        assert_eq!(
            published_from_uri("https://wanderinginn.com/2026/09/02/10-75-pt-2/"),
            Some("2026-09-02".to_string())
        );
    }

    #[test]
    fn returns_none_for_royal_road_uri() {
        // Royal Road URIs carry no date. Nothing may be invented for them.
        assert_eq!(
            published_from_uri(
                "https://www.royalroad.com/fiction/65058/pale-lights/chapter/3804525/epilogue"
            ),
            None
        );
    }

    #[test]
    fn rejects_implausible_date_components() {
        assert_eq!(published_from_uri("https://example.com/9999/99/99/x/"), None);
        assert_eq!(published_from_uri("https://example.com/2026/13/01/x/"), None);
        assert_eq!(published_from_uri("https://example.com/2026/02/32/x/"), None);
        assert_eq!(published_from_uri("https://example.com/chapter/12345/x"), None);
    }

    #[test]
    fn mean_chapter_words_is_zero_for_empty_source() {
        assert_eq!(mean_words(0, 0), 0);
        assert_eq!(mean_words(10, 0), 0);
        assert_eq!(mean_words(100, 4), 25);
    }

    #[test]
    fn pending_chapter_is_excluded_from_totals_but_listed() {
        use crate::config::{Config, SourceConfig};
        use crate::db::SourceRegistry;

        let mut config = Config::default();
        config.sources = vec![SourceConfig {
            id: "test-pending".to_string(),
            name: "Test Pending".to_string(),
            enabled: true,
            ..SourceConfig::default()
        }];
        let registry = SourceRegistry::from_config_for_test(&config);
        let entry = registry.get("test-pending").expect("configured source resolves");

        {
            let db = entry.db();
            let vol_id = db.add_volume("Volume 1").unwrap();
            db.add_chapter("Chapter 1", "https://example.com/c1", vol_id)
                .unwrap();
            db.add_chapter("Chapter 2", "https://example.com/c2", vol_id)
                .unwrap();
            let chapters = db.get_chapters_by_volume(vol_id).unwrap();
            let downloaded = chapters
                .iter()
                .find(|c| c.name == "Chapter 1")
                .expect("chapter 1 exists");
            // Chapter 2 is left without a `raw_data` row: a chapter the TOC
            // has listed but the scraper has not downloaded yet.
            db.add_chapter_data(downloaded.id, "<p>one two three</p>")
                .unwrap();
        }

        let stat = compute(entry);

        assert_eq!(stat.total_chapters, 1, "pending chapter must not count toward the total");
        assert_eq!(stat.pending_chapters, 1);
        assert_eq!(stat.total_words, 3);
        assert_eq!(
            stat.volumes[0].chapters.len(),
            2,
            "the pending chapter must still appear in the TOC listing"
        );
        assert_eq!(
            stat.mean_chapter_words, 3,
            "mean must divide by downloaded chapters, not all listed chapters"
        );
    }

    /// What this proves: with a real, file-backed database and TWO real
    /// connections -- a writer (`SourceDatabase::open`, which enables WAL)
    /// and a separate, long-lived reader (`SourceDatabase::open_query_only`,
    /// exactly what `SourceRegistry` holds for the process's lifetime) -- a
    /// commit through the writer is visible to a `StatsCache::get` call
    /// through the reader on the very next call, with no checkpoint and no
    /// process restart. That is the actual mechanism `data_version` is
    /// chosen for: it is the scraper's connection committing while the web
    /// process's reader looks on, under the same WAL journal mode this
    /// project always runs.
    ///
    /// What this does NOT prove: that an mtime-keyed cache would have
    /// served the stale answer here. That would need asserting the main
    /// database file's mtime is unchanged across the writer's commit, which
    /// is true in this project's WAL setup but is a filesystem-timing
    /// claim this test does not make (mtime resolution and OS behaviour
    /// vary, and the point stands without asserting it: this test would
    /// pass or fail identically regardless of what mtime does, because it
    /// never reads mtime at all). The reasoning for why mtime doesn't move
    /// under WAL is documented on `data_version` above and is not
    /// re-verified here.
    #[test]
    #[serial]
    fn stats_cache_reflects_writer_commit_under_wal() {
        use crate::config::SourceConfig;
        use crate::db::{SourceDatabase, SourceEntry};

        let dir = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::change_to(dir.path());
        std::fs::create_dir_all("db").unwrap();

        let writer = SourceDatabase::open("t").unwrap();
        let vol = writer.add_volume("Volume 1").unwrap();
        writer
            .add_chapter("Chapter 1", "https://example.com/c1", vol)
            .unwrap();
        let ch1 = writer.get_chapters_by_volume(vol).unwrap()[0].id;
        writer
            .add_chapter_data(ch1, "<p>one two three</p>")
            .unwrap();

        let reader = SourceDatabase::open_query_only("t").unwrap();
        let entry = SourceEntry::for_test(
            SourceConfig {
                id: "t".to_string(),
                name: "T".to_string(),
                ..SourceConfig::default()
            },
            reader,
        );

        let cache = StatsCache::new();
        let first = cache.get(&entry);
        assert_eq!(first.total_chapters, 1);
        assert_eq!(first.total_words, 3);

        // Written through the WRITER connection, not the one the cache
        // reads through -- the same shape as the scraper's own connection
        // writing while the web process's long-lived reader looks on.
        writer
            .add_chapter("Chapter 2", "https://example.com/c2", vol)
            .unwrap();
        let ch2 = writer
            .get_chapters_by_volume(vol)
            .unwrap()
            .into_iter()
            .find(|c| c.name == "Chapter 2")
            .unwrap()
            .id;
        writer.add_chapter_data(ch2, "<p>four five</p>").unwrap();

        let second = cache.get(&entry);
        assert_eq!(
            second.total_chapters, 2,
            "cache must observe the writer's commit, not serve stale data"
        );
        assert_eq!(second.total_words, 5);
    }
}
