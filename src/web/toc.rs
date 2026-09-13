//! Per-source table of contents and chapter views.

use std::sync::Arc;

use axum::extract::{Path as AxumPath, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use maud::{html, PreEscaped};

use crate::web::app::AppState;
use crate::web::sanitize::sanitize_chapter;
use crate::web::views::page;

pub async fn source_toc(
    State(state): State<Arc<AppState>>,
    AxumPath(source_id): AxumPath<String>,
) -> Response {
    let Some(entry) = state.registry.get(&source_id) else {
        return (StatusCode::NOT_FOUND, "Unknown source").into_response();
    };
    let stat = state.stats.get(entry);

    page(
        &stat.name,
        html! {
            p class="summary" {
                (stat.total_chapters) " chapters, "
                (format_thousands(stat.total_words)) " words"
            }
            @for volume in &stat.volumes {
                section class="volume" {
                    h2 { (volume.name) }
                    p class="summary" {
                        (volume.chapters.len()) " chapters, "
                        (format_thousands(volume.words)) " words"
                        " · "
                        a href={ "/source/" (source_id) "/volume/" (volume.id) "/epub" } { "EPUB" }
                        " · "
                        a href={ "/source/" (source_id) "/volume/" (volume.id) "/epub?stripped=1" } {
                            "EPUB (no colour)"
                        }
                    }
                    ol class="chapters" {
                        @for chapter in &volume.chapters {
                            li {
                                a href={ "/source/" (source_id) "/chapter/" (chapter.id) } {
                                    (chapter.name)
                                }
                                // A pending chapter has `words: 0`, same as a
                                // genuinely empty one — `downloaded` is what
                                // tells them apart, so the TOC must not
                                // render both as "0 words".
                                @if chapter.downloaded {
                                    span class="words" { (format_thousands(chapter.words)) " words" }
                                } @else {
                                    span class="words pending" { "pending" }
                                }
                                @if let Some(published) = &chapter.published {
                                    span class="date" { (published) }
                                }
                                // A pending chapter has no `raw_data` row, so
                                // its EPUB link would 404 (`chapter_epub`
                                // rejects it deliberately) — don't offer a
                                // link that cannot work.
                                @if chapter.downloaded {
                                    a class="dl" href={
                                        "/source/" (source_id) "/chapter/" (chapter.id) "/epub"
                                    } { "EPUB" }
                                }
                            }
                        }
                    }
                }
            }
        },
    )
    .into_response()
}

pub async fn chapter_page(
    State(state): State<Arc<AppState>>,
    AxumPath((source_id, chapter_id)): AxumPath<(String, i64)>,
) -> Response {
    let Some(entry) = state.registry.get(&source_id) else {
        return (StatusCode::NOT_FOUND, "Unknown source").into_response();
    };

    // LEFT JOIN, not a second query: a chapter the TOC lists but the scraper
    // has not downloaded yet has no `raw_data` row, and that must be known
    // before deciding whether to embed the reader iframe (its `/raw` source
    // would otherwise 404 with no explanation).
    let row: Result<(String, bool), _> = entry.db().connection().query_row(
        "SELECT c.name, rd.data IS NOT NULL
         FROM chapters c
         LEFT JOIN raw_data rd ON rd.chapter_id = c.id
         WHERE c.id = ?1",
        [chapter_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    );
    let Ok((name, downloaded)) = row else {
        return (StatusCode::NOT_FOUND, "Unknown chapter").into_response();
    };

    page(
        &name,
        html! {
            p class="summary" {
                a href={ "/source/" (source_id) "/chapter/" (chapter_id) "/epub" } { "Download EPUB" }
                " · "
                a href={ "/source/" (source_id) } { "Back to contents" }
            }
            @if downloaded {
                // The chapter body is upstream-authored. It renders in a sandbox
                // without allow-scripts or allow-same-origin, so even if the
                // sanitiser missed something it cannot reach this origin.
                iframe
                    class="reader"
                    sandbox=""
                    src={ "/source/" (source_id) "/chapter/" (chapter_id) "/raw" } {}
            } @else {
                p class="pending" {
                    "This chapter is listed in the table of contents but has not "
                    "been downloaded yet. Run the scraper, then reload this page."
                }
            }
        },
    )
    .into_response()
}

/// The sanitised chapter body, served for the sandboxed iframe only.
pub async fn chapter_raw(
    State(state): State<Arc<AppState>>,
    AxumPath((source_id, chapter_id)): AxumPath<(String, i64)>,
) -> Response {
    let Some(entry) = state.registry.get(&source_id) else {
        return (StatusCode::NOT_FOUND, "Unknown source").into_response();
    };

    let raw: Result<String, _> = entry.db().connection().query_row(
        "SELECT data FROM raw_data WHERE chapter_id = ?1",
        [chapter_id],
        |r| r.get(0),
    );
    let Ok(raw) = raw else {
        return (StatusCode::NOT_FOUND, "Chapter not downloaded").into_response();
    };

    let body = sanitize_chapter(&raw);
    let document = html! {
        (PreEscaped("<!DOCTYPE html>"))
        html {
            head {
                meta charset="utf-8";
                style { (PreEscaped(READER_CSS)) }
            }
            body { (PreEscaped(body)) }
        }
    }
    .into_string();

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            // img-src data: blocks the ~70 external image references in the
            // corpus. That is not a regression: the EPUB generator has no
            // add_resource call, so those images are not in the EPUBs
            // either. Do not "fix" this by allowing remote images — doing
            // so would leak the reader's address to upstream hosts on
            // every chapter view.
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; style-src 'unsafe-inline'; img-src data:",
            ),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::X_FRAME_OPTIONS, "SAMEORIGIN"),
        ],
        document,
    )
        .into_response()
}

const READER_CSS: &str = r#"
body { font-family: Georgia, 'Times New Roman', serif; line-height: 1.7;
       max-width: 40rem; margin: 0 auto; padding: 2rem 1rem; color: #1c1c1c; }
img { max-width: 100%; height: auto; }
"#;

/// 1234567 -> "1,234,567".
pub fn format_thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::format_thousands;

    #[test]
    fn formats_thousands_separators() {
        assert_eq!(format_thousands(0), "0");
        assert_eq!(format_thousands(999), "999");
        assert_eq!(format_thousands(1000), "1,000");
        assert_eq!(format_thousands(26451980), "26,451,980");
    }
}
