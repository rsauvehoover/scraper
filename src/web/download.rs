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
        let db = crate::db::SourceDatabase::open_query_only(&db_source_id)
            .map_err(|e| e.to_string())?;

        let volume_name = db
            .get_volume_name(volume_id as isize)
            .map_err(|_| "Unknown volume".to_string())?;

        let chapters = db
            .get_chapters_by_volume(volume_id as isize)
            .map_err(|e| e.to_string())?;
        if chapters.is_empty() {
            return Err("Volume has no chapters".to_string());
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
        build_volume_epub(&db, &volume, &chapters, &ctx, strip_colour).map_err(|e| e.to_string())
    })
    .await;

    match result {
        Ok(Ok(attachment)) => epub_response(&attachment.filename, attachment.bytes),
        Ok(Err(e)) if e == "Unknown volume" || e == "Volume has no chapters" => {
            (StatusCode::NOT_FOUND, e).into_response()
        }
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("build panicked: {}", e),
        )
            .into_response(),
    }
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
        let db = crate::db::SourceDatabase::open_query_only(&db_source_id)
            .map_err(|e| e.to_string())?;

        let row: Result<(String, String, isize), _> = db.connection().query_row(
            "SELECT name, uri, volumeid FROM chapters WHERE id = ?1",
            [chapter_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        );
        let (name, uri, volume_id) = row.map_err(|_| "Unknown chapter".to_string())?;

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
        build_chapter_epub(&db, &chapter, &ctx, strip_colour).map_err(|e| e.to_string())
    })
    .await;

    match result {
        Ok(Ok(attachment)) => epub_response(&attachment.filename, attachment.bytes),
        Ok(Err(e)) if e == "Unknown chapter" => (StatusCode::NOT_FOUND, e).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("build panicked: {}", e),
        )
            .into_response(),
    }
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
                AxumPath((hostile.to_string(), 1)),
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
                AxumPath((hostile.to_string(), 1)),
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
}
