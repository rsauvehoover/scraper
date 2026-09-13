//! Per-source statistics, computed once and cached until the database changes.
//!
//! The frontend never writes, so there is no `word_count` column to read: every
//! figure here is derived from `raw_data.data` on demand and memoised. A full
//! recompute of the largest source takes a few seconds, and the scraper only
//! touches a source when it has new chapters, so a cache keyed on the database
//! file's mtime is enough.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

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
}

#[derive(Debug, Clone)]
pub struct VolumeStat {
    pub id: isize,
    pub name: String,
    pub chapters: Vec<ChapterStat>,
    pub words: usize,
}

#[derive(Debug, Clone)]
pub struct SourceStat {
    pub source_id: String,
    pub name: String,
    pub volumes: Vec<VolumeStat>,
    pub total_words: usize,
    pub total_chapters: usize,
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
        let mut ch_stmt = conn
            .prepare(
                "SELECT c.id, c.name, c.uri, rd.data
                 FROM chapters c
                 LEFT JOIN raw_data rd ON rd.chapter_id = c.id
                 WHERE c.volumeid = ?1
                 ORDER BY c.id",
            )
            .expect("chapters query");

        let chapters: Vec<ChapterStat> = ch_stmt
            .query_map([vol_id], |r| {
                let data: Option<String> = r.get(3)?;
                let uri: String = r.get(2)?;
                Ok(ChapterStat {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    published: published_from_uri(&uri),
                    uri,
                    words: data.as_deref().map(count_words).unwrap_or(0),
                })
            })
            .expect("chapters query")
            .filter_map(|r| r.ok())
            .collect();

        let words = chapters.iter().map(|c| c.words).sum();
        volumes.push(VolumeStat {
            id: vol_id,
            name: vol_name,
            chapters,
            words,
        });
    }

    let total_words: usize = volumes.iter().map(|v| v.words).sum();
    let total_chapters: usize = volumes.iter().map(|v| v.chapters.len()).sum();
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
        mean_chapter_words: mean_words(total_words, total_chapters),
        latest_published,
        volumes,
    }
}

struct Cached {
    mtime: Option<SystemTime>,
    stat: Arc<SourceStat>,
}

/// Memoises `SourceStat` per source, invalidating when the database file's
/// mtime changes. The scraper runs hourly and usually touches one or two
/// sources, so most refreshes are free.
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
        let mtime = std::fs::metadata(entry.db().db_path())
            .and_then(|m| m.modified())
            .ok();

        let mut guard = self.inner.lock().expect("stats cache poisoned");
        if let Some(cached) = guard.get(&entry.config.id) {
            if cached.mtime == mtime {
                return Arc::clone(&cached.stat);
            }
        }

        let stat = Arc::new(compute(entry));
        guard.insert(
            entry.config.id.clone(),
            Cached {
                mtime,
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
}
