//! Per-source table of contents and chapter views.

use std::sync::Arc;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use maud::{html, PreEscaped};
use serde::Deserialize;

use crate::web::app::AppState;
use crate::web::sanitize::sanitize_chapter;
use crate::web::views::page;

/// Shown to the client whenever the cause is a database fault rather than a
/// missing row. The real cause carries schema names and file paths, so it is
/// logged at the point it is known instead of returned. Mirrors the constant
/// of the same name in `web::download`.
const INTERNAL_ERROR_MESSAGE: &str = "Internal server error";

pub async fn source_toc(
    State(state): State<Arc<AppState>>,
    AxumPath(source_id): AxumPath<String>,
) -> Response {
    let Some(entry) = state.registry.get(&source_id) else {
        return (StatusCode::NOT_FOUND, "Unknown source").into_response();
    };
    // This page is one source, so there is nothing to degrade to: a scan that
    // fails is a 500, with the cause logged rather than shown (it can carry
    // schema names and file paths).
    let stat = match state.stats.get(entry) {
        Ok(stat) => stat,
        Err(e) => {
            eprintln!("statistics scan failed for {}: {}", source_id, e);
            return (StatusCode::INTERNAL_SERVER_ERROR, INTERNAL_ERROR_MESSAGE).into_response();
        }
    };

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
                        // `volume.chapters` includes pending chapters so the
                        // listing below stays complete, but `stat.total_chapters`
                        // in the header counts downloaded chapters only. Using
                        // the raw length here made one page report two different
                        // chapter counts for the same data; pending chapters get
                        // their own figure instead. Same expression as `views.rs`.
                        @let downloaded_chapters =
                            volume.chapters.len() - volume.pending_chapters;
                        (downloaded_chapters) " chapters, "
                        (format_thousands(volume.words)) " words"
                        @if volume.pending_chapters > 0 {
                            ", " (volume.pending_chapters) " pending"
                        }
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
    // `QueryReturnedNoRows` means the chapter genuinely does not exist. Any
    // other error is a backend fault (locked file, corruption, missing schema)
    // and reporting it as 404 sends whoever debugs it looking for a missing row
    // instead of the real failure. Same policy as `web::download`.
    let (name, downloaded) = match row {
        Ok(row) => row,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return (StatusCode::NOT_FOUND, "Unknown chapter").into_response()
        }
        Err(e) => {
            eprintln!(
                "chapter lookup failed for {}/{}: {}",
                source_id, chapter_id, e
            );
            return (StatusCode::INTERNAL_SERVER_ERROR, INTERNAL_ERROR_MESSAGE).into_response();
        }
    };

    page(
        &name,
        html! {
            p class="summary" {
                // `chapter_epub` rejects a chapter with no `raw_data` row, so
                // offering the link for one would be offering a 404. The TOC
                // listing suppresses it on the same basis.
                @if downloaded {
                    a href={ "/source/" (source_id) "/chapter/" (chapter_id) "/epub" } {
                        "Download EPUB"
                    }
                    " · "
                }
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
///
/// "For the iframe only" is an intention, not an enforced property: the route
/// is an ordinary authenticated GET, so a signed-in operator who navigates
/// straight to `/raw` gets the same document as a TOP-LEVEL page, outside the
/// `sandbox=""` attribute that constrains it when embedded. On that path the
/// response's own CSP (`default-src 'none'`, no `script-src`) is the only
/// thing between upstream-authored markup and this origin, so the two
/// defences stop being independent. Left as is: it takes a CSP bypass plus a
/// sanitiser miss plus a deliberate navigation to reach, and the alternatives
/// (a separate origin, or a one-shot token per embed) cost more than that is
/// worth here. Do not weaken the CSP below without revisiting this.
pub async fn chapter_raw(
    State(state): State<Arc<AppState>>,
    AxumPath((source_id, chapter_id)): AxumPath<(String, i64)>,
    Query(query): Query<ReaderQuery>,
) -> Response {
    let Some(entry) = state.registry.get(&source_id) else {
        return (StatusCode::NOT_FOUND, "Unknown source").into_response();
    };

    let raw: Result<String, _> = entry.db().connection().query_row(
        "SELECT data FROM raw_data WHERE chapter_id = ?1",
        [chapter_id],
        |r| r.get(0),
    );
    // As in `chapter_page`: no row means the chapter has not been downloaded;
    // anything else is a fault and must not be dressed up as a 404.
    let raw = match raw {
        Ok(raw) => raw,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return (StatusCode::NOT_FOUND, "Chapter not downloaded").into_response()
        }
        Err(e) => {
            eprintln!(
                "chapter body lookup failed for {}/{}: {}",
                source_id, chapter_id, e
            );
            return (StatusCode::INTERNAL_SERVER_ERROR, INTERNAL_ERROR_MESSAGE).into_response();
        }
    };

    let body = sanitize_chapter(&raw);
    let document = html! {
        (PreEscaped("<!DOCTYPE html>"))
        html {
            head {
                meta charset="utf-8";
                style { (PreEscaped(reader_css(query.theme()))) }
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

/// `?theme=` on the reader URL.
///
/// The frame is served into `sandbox=""` with no `allow-same-origin`, so it
/// cannot read the parent's `localStorage`, and the parent's stylesheet does
/// not cross into it either. The URL is the only channel left. It is also
/// attacker-influenced in the ordinary way a query string is, and the reader's
/// CSP permits inline `<style>` — so the value is matched against a closed set
/// and never reaches the document. `reader_css` returns one of three fixed
/// strings; nothing from the request is interpolated into CSS or markup.
#[derive(Deserialize, Default)]
pub struct ReaderQuery {
    #[serde(default)]
    theme: Option<String>,
}

impl ReaderQuery {
    fn theme(&self) -> ReaderTheme {
        match self.theme.as_deref() {
            Some("light") => ReaderTheme::Light,
            Some("dark") => ReaderTheme::Dark,
            // Absent or unrecognised. Anything the parent did not send is
            // treated as "no preference expressed" rather than an error: the
            // frame still has to render a readable chapter.
            _ => ReaderTheme::Auto,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ReaderTheme {
    Auto,
    Light,
    Dark,
}

/// The reader stylesheet for one theme.
///
/// The light palette is always emitted first, so no colour is defined only
/// inside a media query or only in an override block — same rule as the
/// parent's `PALETTE`. `Auto` adds the `prefers-color-scheme` block; `Dark`
/// adds the same values unconditionally, because an explicit choice must beat
/// the system preference in both directions.
fn reader_css(theme: ReaderTheme) -> String {
    let palette = match theme {
        ReaderTheme::Auto => READER_AUTO_DARK,
        ReaderTheme::Light => "",
        ReaderTheme::Dark => READER_DARK,
    };
    format!("{}{}{}", READER_LIGHT, palette, READER_BASE)
}

const READER_LIGHT: &str = r#"
:root { --reader-fg:#1c1c1c; --reader-bg:#ffffff; --reader-accent:#3a5a8c; }
"#;

const READER_DARK: &str = r#"
:root { --reader-fg:#e4e4e2; --reader-bg:#1f1f24; --reader-accent:#8ab0e4; }
"#;

const READER_AUTO_DARK: &str = r#"
@media (prefers-color-scheme: dark) {
  :root { --reader-fg:#e4e4e2; --reader-bg:#1f1f24; --reader-accent:#8ab0e4; }
}
"#;

const READER_BASE: &str = r#"
body { font-family: Georgia, 'Times New Roman', serif; line-height: 1.7;
       max-width: 40rem; margin: 0 auto; padding: 2rem 1rem;
       color: var(--reader-fg); background: var(--reader-bg); }
img { max-width: 100%; height: auto; }
a { color: var(--reader-accent); }
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

    /// Seed the in-memory fixture database with one downloaded and one
    /// pending chapter, and return the two chapter ids in that order.
    #[cfg(test)]
    fn seed_one_downloaded_one_pending(state: &crate::web::app::AppState) -> (i64, i64) {
        let entry = state.registry.get("test-source").expect("fixture source");
        let db = entry.db();
        let vol = db.add_volume("Volume 1").unwrap();
        db.add_chapter("Chapter 1", "https://example.com/c1", vol).unwrap();
        db.add_chapter("Chapter 2", "https://example.com/c2", vol).unwrap();
        let chapters = db.get_chapters_by_volume(vol).unwrap();
        let downloaded = chapters.iter().find(|c| c.name == "Chapter 1").unwrap().id;
        let pending = chapters.iter().find(|c| c.name == "Chapter 2").unwrap().id;
        db.add_chapter_data(downloaded, "<p>one two three</p>").unwrap();
        (downloaded as i64, pending as i64)
    }

    async fn body_of(response: axum::response::Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    /// The page header counts downloaded chapters (matching every other page
    /// and `wordcount.py`); the per-volume line used to count the listing,
    /// which includes pending chapters. One page, two different answers for
    /// the same data. Both must now say "1 chapters", with the undownloaded
    /// one reported as pending rather than folded in or dropped.
    #[tokio::test]
    async fn volume_and_page_chapter_counts_agree() {
        use crate::web::app::AppState;
        use axum::extract::{Path as AxumPath, State};
        use std::sync::Arc;

        let state = Arc::new(AppState::for_test());
        seed_one_downloaded_one_pending(&state);

        let body = body_of(
            super::source_toc(State(Arc::clone(&state)), AxumPath("test-source".to_string())).await,
        )
        .await;

        assert_eq!(
            body.matches("1 chapters").count(),
            2,
            "the page header and the volume line must report the same count: {}",
            body
        );
        assert!(
            !body.contains("2 chapters"),
            "the pending chapter must not be counted as downloaded: {}",
            body
        );
        assert!(body.contains("1 pending"), "pending chapters must be stated: {}", body);
    }

    /// `chapter_epub` 404s a chapter with no `raw_data` row, and the table of
    /// contents already suppresses its EPUB link on that basis. The chapter
    /// page offered one unconditionally.
    #[tokio::test]
    async fn pending_chapter_page_offers_no_epub_link() {
        use crate::web::app::AppState;
        use axum::extract::{Path as AxumPath, State};
        use std::sync::Arc;

        let state = Arc::new(AppState::for_test());
        let (downloaded, pending) = seed_one_downloaded_one_pending(&state);

        let body = body_of(
            super::chapter_page(
                State(Arc::clone(&state)),
                AxumPath(("test-source".to_string(), pending)),
            )
            .await,
        )
        .await;
        assert!(
            !body.contains("Download EPUB"),
            "a pending chapter must not offer a link that 404s: {}",
            body
        );

        let body = body_of(
            super::chapter_page(
                State(Arc::clone(&state)),
                AxumPath(("test-source".to_string(), downloaded)),
            )
            .await,
        )
        .await;
        assert!(
            body.contains("Download EPUB"),
            "a downloaded chapter must still offer its EPUB: {}",
            body
        );
    }

    /// The sandboxed reader cannot read the parent's storage, so the theme
    /// arrives in the URL. An explicit choice has to beat the frame's own
    /// `prefers-color-scheme`, in both directions.
    #[tokio::test]
    async fn reader_honours_an_explicit_theme() {
        use crate::web::app::AppState;
        use axum::extract::{Path as AxumPath, Query, State};
        use std::sync::Arc;

        let state = Arc::new(AppState::for_test());
        let (downloaded, _) = seed_one_downloaded_one_pending(&state);

        let dark = body_of(
            super::chapter_raw(
                State(Arc::clone(&state)),
                AxumPath(("test-source".to_string(), downloaded)),
                Query(super::ReaderQuery { theme: Some("dark".to_string()) }),
            )
            .await,
        )
        .await;
        assert!(
            !dark.contains("prefers-color-scheme"),
            "an explicit choice must not be left to the system preference: {}",
            dark
        );
        assert!(
            dark.contains("--reader-bg:#1f1f24"),
            "?theme=dark must serve the dark palette: {}",
            dark
        );

        let light = body_of(
            super::chapter_raw(
                State(Arc::clone(&state)),
                AxumPath(("test-source".to_string(), downloaded)),
                Query(super::ReaderQuery { theme: Some("light".to_string()) }),
            )
            .await,
        )
        .await;
        assert!(
            !light.contains("prefers-color-scheme") && !light.contains("--reader-bg:#1f1f24"),
            "?theme=light must stay light on a dark desktop: {}",
            light
        );
    }

    /// The reader's CSP allows inline `<style>`, so a query value reaching the
    /// stylesheet would be a live injection. Nothing from the request is
    /// interpolated: an unrecognised value is simply not a choice, and the
    /// frame falls back to `prefers-color-scheme` exactly as when the
    /// parameter is absent.
    #[tokio::test]
    async fn reader_ignores_an_unrecognised_theme() {
        use crate::web::app::AppState;
        use axum::extract::{Path as AxumPath, Query, State};
        use std::sync::Arc;

        let state = Arc::new(AppState::for_test());
        let (downloaded, _) = seed_one_downloaded_one_pending(&state);

        let hostile = "purple} body{display:none} :root{";
        let rejected = body_of(
            super::chapter_raw(
                State(Arc::clone(&state)),
                AxumPath(("test-source".to_string(), downloaded)),
                Query(super::ReaderQuery { theme: Some(hostile.to_string()) }),
            )
            .await,
        )
        .await;
        let absent = body_of(
            super::chapter_raw(
                State(Arc::clone(&state)),
                AxumPath(("test-source".to_string(), downloaded)),
                Query(super::ReaderQuery::default()),
            )
            .await,
        )
        .await;

        assert!(
            !rejected.contains("purple") && !rejected.contains("display:none"),
            "a query value must never reach the document: {}",
            rejected
        );
        assert_eq!(
            rejected, absent,
            "an unrecognised theme must behave exactly as no theme at all"
        );
        assert!(
            absent.contains("prefers-color-scheme"),
            "with no choice expressed the frame follows the system preference: {}",
            absent
        );
    }

    /// The reader's whole defence is that CSP. `img-src data:` also keeps the
    /// reader's address off upstream hosts. Adding a theme parameter must not
    /// have loosened either, and `no-referrer` keeps the chapter id out of any
    /// request the document manages to make.
    #[tokio::test]
    async fn reader_keeps_its_content_security_policy() {
        use crate::web::app::AppState;
        use axum::extract::{Path as AxumPath, Query, State};
        use std::sync::Arc;

        let state = Arc::new(AppState::for_test());
        let (downloaded, _) = seed_one_downloaded_one_pending(&state);

        let response = super::chapter_raw(
            State(Arc::clone(&state)),
            AxumPath(("test-source".to_string(), downloaded)),
            Query(super::ReaderQuery { theme: Some("dark".to_string()) }),
        )
        .await;

        assert_eq!(
            response.headers().get("content-security-policy").unwrap(),
            "default-src 'none'; style-src 'unsafe-inline'; img-src data:",
            "the reader's CSP must not be weakened"
        );
        assert_eq!(response.headers().get("referrer-policy").unwrap(), "no-referrer");
        assert_eq!(response.headers().get("x-frame-options").unwrap(), "SAMEORIGIN");
    }

    #[test]
    fn formats_thousands_separators() {
        assert_eq!(format_thousands(0), "0");
        assert_eq!(format_thousands(999), "999");
        assert_eq!(format_thousands(1000), "1,000");
        assert_eq!(format_thousands(26451980), "26,451,980");
    }
}
