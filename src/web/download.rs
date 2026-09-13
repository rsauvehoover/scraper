//! On-demand EPUB generation.
//!
//! Nothing here reads `build/`. That directory only holds artefacts whose
//! `regenerate_epub` flag was set at the time of a scrape and cleared after,
//! so it is a cache of recent output rather than an archive — typically a
//! handful of files against many volumes. A volume rename also leaves the old
//! file behind, so what is there can be wrong as well as missing. Generating
//! from the database is the only correct source.
//!
//! No filesystem path is constructed in this module. The source ID resolves
//! through `SourceRegistry`, and volume and chapter IDs are integers parsed by
//! the extractor, so a traversal attempt fails before any handler runs.

use std::sync::Arc;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use serde::Deserialize;

use crate::epub::{build_chapter_epub, build_volume_epub, EpubContext};
use crate::postprocess::ProcessorRegistry;
use crate::web::app::AppState;

#[derive(Deserialize, Default)]
pub struct DownloadQuery {
    /// `?stripped=1` serves the colour-stripped variant.
    #[serde(default)]
    stripped: Option<String>,
}

impl DownloadQuery {
    fn strip_colour(&self) -> bool {
        matches!(self.stripped.as_deref(), Some("1") | Some("true"))
    }
}

/// RFC 5987 `attr-char`: unreserved marks need not be percent-encoded.
/// `NON_ALPHANUMERIC` alone would also escape `.`, `-`, `_` and `~`, which
/// still produces a spec-valid value but turns every filename's extension
/// into `%2Eepub` for no safety benefit.
const RFC5987_ATTR_CHAR: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Build a `Content-Disposition` value that survives spaces, colons and
/// non-ASCII, and cannot inject a header.
pub fn content_disposition(filename: &str) -> String {
    let ascii: String = filename
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' ' | '(' | ')') {
                c
            } else {
                '_'
            }
        })
        .collect();

    let encoded = utf8_percent_encode(filename, RFC5987_ATTR_CHAR).to_string();

    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{}", encoded)
}

fn epub_response(filename: &str, bytes: Vec<u8>) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/epub+zip".to_string()),
            (header::CONTENT_DISPOSITION, content_disposition(filename)),
        ],
        bytes,
    )
        .into_response()
}

/// A generic message shown to the client whenever the cause is a database or
/// build fault rather than "this row does not exist". The real cause is
/// never put in the response body — it can carry schema names, file paths or
/// driver detail — so it is `eprintln!`ed at the point it is known instead.
const INTERNAL_ERROR_MARKER: &str = "internal_error";
const INTERNAL_ERROR_MESSAGE: &str = "Internal server error";

/// Turn the closure's `Result<(filename, bytes), String>` (as returned by
/// `spawn_blocking`, the build's attachment already broken into its two
/// fields so this stays independent of where that type is defined) into a
/// response. The error string is either `"not_found:<message safe to show
/// the client>"` or the fixed `INTERNAL_ERROR_MARKER` — never a raw driver or
/// build error, which is logged server-side at the point it occurred
/// instead.
fn build_task_response(
    result: Result<Result<(String, Vec<u8>), String>, tokio::task::JoinError>,
) -> Response {
    match result {
        Ok(Ok((filename, bytes))) => epub_response(&filename, bytes),
        Ok(Err(marker)) => match marker.strip_prefix("not_found:") {
            Some(msg) => (StatusCode::NOT_FOUND, msg.to_string()).into_response(),
            None => (StatusCode::INTERNAL_SERVER_ERROR, INTERNAL_ERROR_MESSAGE).into_response(),
        },
        Err(e) => {
            eprintln!("epub build task panicked: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, INTERNAL_ERROR_MESSAGE).into_response()
        }
    }
}

pub async fn volume_epub(
    State(state): State<Arc<AppState>>,
    AxumPath((source_id, volume_id)): AxumPath<(String, i64)>,
    Query(query): Query<DownloadQuery>,
) -> Response {
    let Some(entry) = state.registry.get(&source_id) else {
        return (StatusCode::NOT_FOUND, "Unknown source").into_response();
    };
    let source = entry.config.clone();
    let strip_colour = query.strip_colour();

    // Generation is CPU-bound and each build holds several megabytes, so a
    // burst of requests must not be allowed to exhaust memory.
    let permit = Arc::clone(&state.epub_permits).acquire_owned().await;
    let Ok(_permit) = permit else {
        return (StatusCode::SERVICE_UNAVAILABLE, "Shutting down").into_response();
    };

    // Everything that touches the database — including the metadata lookups
    // that decide whether the volume exists at all — runs inside the blocking
    // task. `open_query_only` sets a 5-second `busy_timeout`, and a reader
    // that actually waits out that timeout must not do it on an async
    // executor thread, where it would stall every other request scheduled
    // there for the duration.
    let db_source_id = source_id.clone();
    let result = tokio::task::spawn_blocking(move || {
        // A fresh query-only handle: rusqlite Connections are not Sync, so the
        // blocking task opens its own rather than borrowing the registry's.
        let db = match crate::db::SourceDatabase::open_query_only(&db_source_id) {
            Ok(db) => db,
            Err(e) => {
                eprintln!("opening database for {} failed: {}", db_source_id, e);
                return Err(INTERNAL_ERROR_MARKER.to_string());
            }
        };

        // `QueryReturnedNoRows` means the volume genuinely does not exist —
        // any other error is a real backend fault (locked file, corruption,
        // I/O) and must not be reported to the client as "not found", which
        // would send whoever debugs it looking for a missing row instead of
        // the actual failure.
        let volume_name = match db.get_volume_name(volume_id as isize) {
            Ok(name) => name,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Err("not_found:Unknown volume".to_string())
            }
            Err(e) => {
                eprintln!(
                    "volume lookup failed for {}/{}: {}",
                    db_source_id, volume_id, e
                );
                return Err(INTERNAL_ERROR_MARKER.to_string());
            }
        };

        let chapters = match db.get_chapters_by_volume(volume_id as isize) {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "chapter list failed for {}/{}: {}",
                    db_source_id, volume_id, e
                );
                return Err(INTERNAL_ERROR_MARKER.to_string());
            }
        };
        if chapters.is_empty() {
            return Err("not_found:Volume has no chapters".to_string());
        }

        let volume = crate::db::Volume {
            id: volume_id as isize,
            name: volume_name,
        };
        let registry = ProcessorRegistry::new();
        let ctx = EpubContext {
            source: &source,
            processor_registry: &registry,
        };
        build_volume_epub(&db, &volume, &chapters, &ctx, strip_colour)
            .map(|a| (a.filename, a.bytes))
            .map_err(|e| {
                eprintln!(
                    "building volume epub failed for {}/{}: {}",
                    db_source_id, volume_id, e
                );
                INTERNAL_ERROR_MARKER.to_string()
            })
    })
    .await;

    build_task_response(result)
}

pub async fn chapter_epub(
    State(state): State<Arc<AppState>>,
    AxumPath((source_id, chapter_id)): AxumPath<(String, i64)>,
    Query(query): Query<DownloadQuery>,
) -> Response {
    let Some(entry) = state.registry.get(&source_id) else {
        return (StatusCode::NOT_FOUND, "Unknown source").into_response();
    };
    let source = entry.config.clone();
    let strip_colour = query.strip_colour();

    let permit = Arc::clone(&state.epub_permits).acquire_owned().await;
    let Ok(_permit) = permit else {
        return (StatusCode::SERVICE_UNAVAILABLE, "Shutting down").into_response();
    };

    // See volume_epub: the chapter lookup is database work too, so it stays
    // inside the blocking task alongside the build.
    let db_source_id = source_id.clone();
    let result = tokio::task::spawn_blocking(move || {
        let db = match crate::db::SourceDatabase::open_query_only(&db_source_id) {
            Ok(db) => db,
            Err(e) => {
                eprintln!("opening database for {} failed: {}", db_source_id, e);
                return Err(INTERNAL_ERROR_MARKER.to_string());
            }
        };

        // A LEFT JOIN, not a second query: a chapter the TOC lists but the
        // scraper has not downloaded yet has no `raw_data` row. That must be
        // known here — before `build_chapter_epub` is ever called — or the
        // missing row surfaces from inside the build as an opaque 500
        // instead of the 404 a pending chapter actually is.
        let row: Result<(String, String, isize, bool), _> = db.connection().query_row(
            "SELECT c.name, c.uri, c.volumeid, rd.data IS NOT NULL
             FROM chapters c
             LEFT JOIN raw_data rd ON rd.chapter_id = c.id
             WHERE c.id = ?1",
            [chapter_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        );
        let (name, uri, volume_id, downloaded) = match row {
            Ok(row) => row,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Err("not_found:Unknown chapter".to_string())
            }
            Err(e) => {
                eprintln!(
                    "chapter lookup failed for {}/{}: {}",
                    db_source_id, chapter_id, e
                );
                return Err(INTERNAL_ERROR_MARKER.to_string());
            }
        };
        if !downloaded {
            return Err("not_found:Chapter has not been downloaded yet".to_string());
        }

        let chapter = crate::db::Chapter {
            id: chapter_id as isize,
            name,
            uri,
            volume_id,
            data_id: None,
        };
        let registry = ProcessorRegistry::new();
        let ctx = EpubContext {
            source: &source,
            processor_registry: &registry,
        };
        build_chapter_epub(&db, &chapter, &ctx, strip_colour)
            .map(|a| (a.filename, a.bytes))
            .map_err(|e| {
                eprintln!(
                    "building chapter epub failed for {}/{}: {}",
                    db_source_id, chapter_id, e
                );
                INTERNAL_ERROR_MARKER.to_string()
            })
    })
    .await;

    build_task_response(result)
}

#[cfg(test)]
mod tests {
    use super::content_disposition;

    #[test]
    fn encodes_spaces_and_colons() {
        // This is a real filename from the build directory.
        let header = content_disposition("The Years of Apocalypse: Book 5.epub");

        assert!(header.starts_with("attachment; "), "{}", header);
        // RFC 5987 form carries the exact name.
        assert!(
            header.contains("filename*=UTF-8''The%20Years%20of%20Apocalypse%3A%20Book%205.epub"),
            "{}", header
        );
        // The ASCII fallback must be quoted and free of raw quotes.
        assert!(header.contains(r#"filename=""#), "{}", header);
    }

    #[test]
    fn ascii_fallback_contains_no_quotes_or_control_characters() {
        let header = content_disposition("weird \"quoted\" \r\nname.epub");
        let fallback_start = header.find("filename=\"").unwrap() + 10;
        let fallback_end = header[fallback_start..].find('"').unwrap() + fallback_start;
        let fallback = &header[fallback_start..fallback_end];

        assert!(!fallback.contains('"'));
        assert!(!fallback.contains('\r'));
        assert!(!fallback.contains('\n'));
    }

    #[test]
    fn header_is_a_single_line() {
        // A newline here is header injection.
        let header = content_disposition("a\r\nX-Evil: yes.epub");
        assert!(!header.contains('\r'));
        assert!(!header.contains('\n'));
    }

    #[test]
    fn handles_unicode_names() {
        let header = content_disposition("Café Volume 1.epub");
        assert!(header.contains("filename*=UTF-8''"));
        assert!(header.contains("Caf%C3%A9"));
    }

    /// `source_id` is the only handler input that could reach a filesystem
    /// path (`open_query_only` joins it onto `db/`). Volume and chapter ids
    /// are `i64`, so a traversal string cannot even parse into one — this
    /// targets the string field instead.
    ///
    /// The handlers never build a path themselves; `SourceRegistry::get`
    /// resolves `source_id` by exact match against the configured list
    /// first, so a hostile value must be rejected with 404 before
    /// `open_query_only` is ever called.
    #[tokio::test]
    async fn rejects_path_traversal_in_source_id() {
        use super::{chapter_epub, volume_epub, DownloadQuery};
        use crate::web::app::AppState;
        use axum::extract::{Path as AxumPath, Query, State};
        use axum::http::StatusCode;
        use std::sync::Arc;

        let state = Arc::new(AppState::for_test());

        for hostile in [
            "../../etc/passwd",
            "..",
            ".",
            "",
            "/etc/passwd",
            "wandering-inn/../../../etc/passwd",
        ] {
            let response = volume_epub(
                State(Arc::clone(&state)),
                AxumPath((hostile.to_owned(), 1)),
                Query(DownloadQuery::default()),
            )
            .await;
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "volume_epub must reject source_id {:?}, got {}",
                hostile,
                response.status()
            );

            let response = chapter_epub(
                State(Arc::clone(&state)),
                AxumPath((hostile.to_owned(), 1)),
                Query(DownloadQuery::default()),
            )
            .await;
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "chapter_epub must reject source_id {:?}, got {}",
                hostile,
                response.status()
            );
        }
    }

    /// Restores the process cwd on drop, including on unwind from a panic.
    /// `open_query_only` resolves its path relative to the cwd, so a test
    /// that needs a real file-backed database (the registry's in-memory
    /// fixture and the handler's own `open_query_only` call are two
    /// different connections and cannot share state otherwise) must point
    /// the cwd at a scratch directory for its duration. Mirrors the same
    /// guard in `src/db/connection.rs`.
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

    /// A chapter the TOC lists but the scraper has not downloaded yet has no
    /// `raw_data` row. Hitting its EPUB link directly must 404 with an
    /// explanation, not 500 with `build_chapter_epub`'s internal
    /// `QueryReturnedNoRows` propagating as an opaque failure.
    #[tokio::test]
    #[serial_test::serial]
    async fn pending_chapter_download_is_404_not_500() {
        use super::{chapter_epub, DownloadQuery};
        use crate::web::app::AppState;
        use axum::extract::{Path as AxumPath, Query, State};
        use axum::http::StatusCode;
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::change_to(dir.path());
        std::fs::create_dir_all("db").unwrap();

        let chapter_id = {
            // A real, file-backed database: `open_query_only` inside the
            // handler opens its own connection to this same file, which an
            // in-memory fixture cannot be reached through.
            let db = crate::db::SourceDatabase::open("test-source").unwrap();
            let vol = db.add_volume("Volume 1").unwrap();
            db.add_chapter("Pending Chapter", "https://example.com/c1", vol)
                .unwrap();
            // Deliberately no `add_chapter_data` call — this chapter is
            // listed but not downloaded.
            db.get_chapters_by_volume(vol).unwrap()[0].id
        };

        let state = Arc::new(AppState::for_test());

        let response = chapter_epub(
            State(Arc::clone(&state)),
            AxumPath(("test-source".to_string(), chapter_id as i64)),
            Query(DownloadQuery::default()),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "a pending chapter's EPUB must 404, not 500"
        );

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(
            text.contains("not been downloaded"),
            "the body must explain why, not show a raw database error: {}",
            text
        );
    }
}
