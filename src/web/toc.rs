//! Per-source table of contents and chapter views.

use std::sync::Arc;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use maud::{html, Markup, PreEscaped};
use serde::Deserialize;

use crate::stats::cache::{ChapterStat, VolumeStat};
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
    // One snapshot for the whole request; see `LiveRegistry::snapshot`.
    let registry = state.registry.snapshot();
    let Some(entry) = registry.get(&source_id) else {
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
    // A manga source without `VolumeEnds` keeps every chapter in one volume
    // that is never built or sent, so its page is a flat chapter list: no
    // fold, no volume checkbox, no volume EPUB link. With `VolumeEnds` it
    // folds like a serial, `Volume N` first and the series volume last; a
    // volume EPUB exists only when Kindle Comic Converter builds it.
    let manga = entry.config.is_manga();
    let has_volumes = manga && entry.config.has_volumes();
    let volume_files = has_volumes && registry.kcc_builds();
    let ordered: Vec<&VolumeStat> = if manga { manga_volume_order(&stat.volumes, &stat.name) } else { stat.volumes.iter().collect() };
    // The newest volume starts open: the last one with any chapter listed,
    // since a manga series volume sits last and may be empty.
    let open_index = ordered.iter().rposition(|v| !v.chapters.is_empty()).unwrap_or(ordered.len().saturating_sub(1));

    page(
        &stat.name,
        html! {
            div class="page-bar" {
                p class="summary" {
                    (stat.total_chapters) " chapters, "
                    (format_thousands(stat.total_words)) " words"
                    // Every EPUB the page offers, in one archive: chapters
                    // for manga, volumes for a serial. Streamed, so it is
                    // offered even for a series of gigabytes.
                    @if stat.total_chapters > 0 {
                        " · "
                        a href={ "/source/" (source_id) "/epubs.zip" } { "All EPUBs (zip)" }
                        @if !manga {
                            " · "
                            a href={ "/source/" (source_id) "/epubs.zip?stripped=1" } { "All EPUBs (zip, no colour)" }
                        }
                    }
                }
                @if !manga || has_volumes {
                    // Hidden until toc_folds.js runs, so a reader without
                    // JavaScript is not offered a button that does nothing.
                    button type="button" id="toggle-volumes" class="toggle-all"
                           aria-controls="volumes" hidden { "Collapse all" }
                }
            }
            form id="send-pick" method="get" action={ "/source/" (source_id) "/send" } {
            @if manga && !has_volumes {
                ol class="chapters" {
                    @for volume in &ordered {
                        @for chapter in &volume.chapters {
                            (chapter_row(&source_id, chapter))
                        }
                    }
                }
            } @else {
            // Each volume folds natively. Its heading and its counts and
            // download links are the summary, so they stay visible and usable
            // with the volume folded. The newest volume starts open; the script
            // then applies whatever this reader left open last time, stored
            // under this series' id.
            div id="volumes" data-storage-key={ "toc-open:" (source_id) } {
            @for (i, volume) in ordered.iter().enumerate() {
                @let newest = i == open_index;
                details class="volume" data-volume=(volume.id) open[newest] {
                    summary {
                    // `volume.chapters` includes pending chapters so the
                    // listing below stays complete, but `stat.total_chapters`
                    // in the header counts downloaded chapters only. Using
                    // the raw length here made one page report two different
                    // chapter counts for the same data; pending chapters get
                    // their own figure instead. Same expression as `views.rs`.
                    @let downloaded_chapters =
                        volume.chapters.len() - volume.pending_chapters;
                    // A volume with nothing downloaded has nothing to send;
                    // a manga volume is never sent.
                    @if downloaded_chapters > 0 && !manga {
                        input type="checkbox" class="pick" name="v" value=(volume.id)
                              aria-label={ "Select " (volume.name) };
                    }
                    h2 { (volume.name) }
                    p class="summary" {
                        (downloaded_chapters) " chapters, "
                        (format_thousands(volume.words)) " words"
                        @if volume.pending_chapters > 0 {
                            ", " (volume.pending_chapters) " pending"
                        }
                        // The series volume of a manga source is never built,
                        // nor is a volume with nothing downloaded, and a manga
                        // volume file exists only with Kindle Comic Converter.
                        @if !manga || (volume_files && volume.name != stat.name && downloaded_chapters > 0) {
                            " · "
                            a href={ "/source/" (source_id) "/volume/" (volume.id) "/epub" } { "EPUB" }
                        }
                        @if !manga {
                            " · "
                            a href={ "/source/" (source_id) "/volume/" (volume.id) "/epub?stripped=1" } {
                                "EPUB (no colour)"
                            }
                        }
                    }
                    }
                    ol class="chapters" {
                        @for chapter in &volume.chapters {
                            (chapter_row(&source_id, chapter))
                        }
                    }
                }
            }
            }
            }
            // The selection is the query string of this form, so sending
            // needs no script. With script, send_select.js hides the bar
            // until something is ticked and says what.
            div class="send-bar" id="send-bar" {
                span class="count" id="send-count" {
                    @if manga { "Tick chapters to send them." } @else { "Tick volumes or chapters to send them." }
                }
                button type="button" id="send-clear" class="toggle-all" hidden { "Clear" }
                button type="submit" id="send-open" { "Send selected" }
            }
            }
            script { (PreEscaped(TOC_SCRIPT)) }
            script { (PreEscaped(crate::web::send::SEND_SCRIPT)) }
        },
    )
    .into_response()
}

/// Collapse/expand-all and remembered folds. See the file's header.
const TOC_SCRIPT: &str = include_str!("toc_folds.js");

pub async fn chapter_page(
    State(state): State<Arc<AppState>>,
    AxumPath((source_id, chapter_id)): AxumPath<(String, i64)>,
) -> Response {
    // One snapshot for the whole request; see `LiveRegistry::snapshot`.
    let registry = state.registry.snapshot();
    let Some(entry) = registry.get(&source_id) else {
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
    // One snapshot for the whole request; see `LiveRegistry::snapshot`.
    let registry = state.registry.snapshot();
    let Some(entry) = registry.get(&source_id) else {
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

    // Stored images are inlined after sanitising: the sandboxed frame has an
    // opaque origin, so an image route behind the session cookie would get
    // no cookie, and the sanitiser drops data: URLs it does not know.
    let images = match entry.db().get_chapter_images(chapter_id as isize) {
        Ok(images) => images,
        Err(e) => {
            eprintln!("chapter image lookup failed for {}/{}: {}", source_id, chapter_id, e);
            return (StatusCode::INTERNAL_SERVER_ERROR, INTERNAL_ERROR_MESSAGE).into_response();
        }
    };
    let body = inline_images(&sanitize_chapter(&raw), &images);
    let document = html! {
        (PreEscaped("<!DOCTYPE html>"))
        html {
            head {
                meta charset="utf-8";
                style { (PreEscaped(reader_css(query.theme(), entry.config.is_manga()))) }
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
fn reader_css(theme: ReaderTheme, manga: bool) -> String {
    let palette = match theme {
        ReaderTheme::Auto => READER_AUTO_DARK,
        ReaderTheme::Light => "",
        ReaderTheme::Dark => READER_DARK,
    };
    let layout = if manga { READER_MANGA } else { "" };
    format!("{}{}{}{}", READER_LIGHT, palette, READER_BASE, layout)
}

const READER_LIGHT: &str = r#"
:root { color-scheme: light; --reader-fg:#1c1c1c; --reader-bg:#ffffff; --reader-accent:#3a5a8c; }
"#;

const READER_DARK: &str = r#"
:root { color-scheme: dark; --reader-fg:#e4e4e2; --reader-bg:#1f1f24; --reader-accent:#8ab0e4; }
"#;

const READER_AUTO_DARK: &str = r#"
@media (prefers-color-scheme: dark) {
  :root { color-scheme: dark; --reader-fg:#e4e4e2; --reader-bg:#1f1f24; --reader-accent:#8ab0e4; }
}
"#;

const READER_BASE: &str = r#"
body { font-family: Georgia, 'Times New Roman', serif; line-height: 1.7;
       max-width: 40rem; margin: 0 auto; padding: 2rem 1rem;
       color: var(--reader-fg); background: var(--reader-bg); }
img { max-width: 100%; height: auto; }
a { color: var(--reader-accent); }
"#;

/// Manga pages want the frame's full width and no prose column.
const READER_MANGA: &str = r#"
body { max-width: none; padding: 0; }
img { display: block; margin: 0 auto; }
"#;

/// Replace each stored image's `src` in sanitised chapter HTML with a data:
/// URL of its bytes. Only the exact paths the scraper assigned are touched;
/// any other `src` stays as it is and is blocked by the reader's CSP.
fn inline_images(html: &str, images: &[crate::db::ChapterImage]) -> String {
    use base64::Engine;
    let mut out = html.to_string();
    for image in images {
        let needle = format!("src=\"{}\"", image.path);
        if !out.contains(&needle) {
            continue;
        }
        let data = base64::engine::general_purpose::STANDARD.encode(&image.data);
        out = out.replace(&needle, &format!("src=\"data:{};base64,{}\"", image.mime, data));
    }
    out
}

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

/// A manga source's volumes for display: `Volume N` ascending, then the
/// series volume and anything else, last.
fn manga_volume_order<'a>(volumes: &'a [VolumeStat], series: &str) -> Vec<&'a VolumeStat> {
    let number = |v: &VolumeStat| v.name.strip_prefix("Volume ").and_then(|n| n.parse::<usize>().ok());
    let mut numbered: Vec<(usize, &VolumeStat)> = volumes.iter().filter_map(|v| number(v).map(|n| (n, v))).collect();
    numbered.sort_by_key(|(n, _)| *n);
    let mut ordered: Vec<&VolumeStat> = numbered.into_iter().map(|(_, v)| v).collect();
    ordered.extend(volumes.iter().filter(|v| number(v).is_none() || v.name == series));
    ordered.dedup_by(|a, b| std::ptr::eq(*a, *b));
    ordered
}

/// One chapter's line in a listing: checkbox, link, size or "pending", date,
/// and an EPUB link, the last two only once the chapter is downloaded.
fn chapter_row(source_id: &str, chapter: &ChapterStat) -> Markup {
    html! {
        li {
            @if chapter.downloaded {
                input type="checkbox" class="pick" name="c" value=(chapter.id)
                      aria-label={ "Select " (chapter.name) };
            }
            a href={ "/source/" (source_id) "/chapter/" (chapter.id) } {
                (chapter.name)
            }
            // A pending chapter has `words: 0`, same as a genuinely empty
            // one — `downloaded` is what tells them apart, so the TOC must
            // not render both as "0 words".
            @if chapter.downloaded {
                span class="words" { (format_thousands(chapter.words)) " words" }
            } @else {
                span class="words pending" { "pending" }
            }
            @if let Some(published) = &chapter.published {
                span class="date" { (published) }
            }
            // A pending chapter has no `raw_data` row, so its EPUB link
            // would 404 (`chapter_epub` rejects it deliberately) — don't
            // offer a link that cannot work.
            @if chapter.downloaded {
                a class="dl" href={
                    "/source/" (source_id) "/chapter/" (chapter.id) "/epub"
                } { "EPUB" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::format_thousands;

    /// Seed the in-memory fixture database with one downloaded and one
    /// pending chapter, and return the two chapter ids in that order.
    #[cfg(test)]
    fn seed_one_downloaded_one_pending(state: &crate::web::app::AppState) -> (i64, i64) {
        let registry = state.registry.snapshot();
        let entry = registry.get("test-source").expect("fixture source");
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

    /// A stored image reaches the sandboxed reader as a data: URL, which is
    /// what its CSP allows; a reference the scraper did not store stays a
    /// plain path and is blocked.
    #[tokio::test]
    async fn reader_inlines_stored_images_as_data_urls() {
        use crate::config::{SourceConfig, SourceKind};
        use crate::db::{ChapterImage, RegenerateFlags};
        use crate::web::app::AppState;
        use axum::extract::{Path as AxumPath, Query, State};
        use std::sync::Arc;

        let state = Arc::new(AppState::for_test_with_sources(vec![SourceConfig {
            id: "test-source".into(),
            enabled: true,
            kind: SourceKind::MangaWordpress,
            ..Default::default()
        }]));
        let id = {
            let registry = state.registry.snapshot();
            let db = registry.get("test-source").unwrap().db();
            let vol = db.add_volume("Example Series").unwrap();
            db.add_chapter("Chapter", "https://example.com/c1", vol).unwrap();
            let id = db.get_chapters_by_volume(vol).unwrap()[0].id;
            let image = ChapterImage {
                chapter_id: id,
                seq: 0,
                path: format!("images/{}-0.png", id),
                source_url: "https://example.com/p1.png".into(),
                mime: "image/png".into(),
                width: 1,
                height: 1,
                data: vec![1, 2, 3],
            };
            let html = format!(
                r#"<p><img src="images/{}-0.png" alt="Page 1"/></p><p><img src="images/{}-9.png" alt="Missing"/></p>"#,
                id, id
            );
            db.store_chapter_content(id, &html, &[image], RegenerateFlags::ChapterOnly).unwrap();
            id
        };

        let body = body_of(
            super::chapter_raw(
                State(Arc::clone(&state)),
                AxumPath(("test-source".to_string(), id as i64)),
                Query(super::ReaderQuery { theme: None }),
            )
            .await,
        )
        .await;

        assert!(body.contains(r#"src="data:image/png;base64,AQID""#), "{}", body);
        assert!(body.contains(&format!(r#"src="images/{}-9.png""#, id)), "unknown path untouched: {}", body);
        assert!(!body.contains(&format!(r#"src="images/{}-0.png""#, id)), "{}", body);
        assert!(body.contains("max-width: none"), "manga reader uses the full width: {}", body);
    }

    /// Volumes fold, the newest starts open, and a folded volume still
    /// carries its counts and downloads in its summary.
    #[tokio::test]
    async fn volumes_fold_with_the_newest_open() {
        use axum::extract::{Path as AxumPath, State};
        use std::sync::Arc;
        let state = Arc::new(crate::web::app::AppState::for_test());
        {
            let registry = state.registry.snapshot();
            let db = registry.get("test-source").unwrap().db();
            for name in ["Volume 1", "Volume 2"] {
                let vol = db.add_volume(name).unwrap();
                let uri = format!("https://example.com/{}", name.replace(' ', "-"));
                db.add_chapter("Chapter", &uri, vol).unwrap();
                let id = db.get_chapters_by_volume(vol).unwrap()[0].id;
                db.add_chapter_data(id, "<p>one two</p>").unwrap();
            }
        }

        let body = body_of(
            super::source_toc(State(Arc::clone(&state)), AxumPath("test-source".to_string())).await,
        )
        .await;

        let volumes: Vec<&str> = body.split("<details class=\"volume\"").skip(1).collect();
        assert_eq!(volumes.len(), 2, "{}", body);
        let open_tag = |v: &str| v[..v.find('>').unwrap()].contains(" open");
        assert!(!open_tag(volumes[0]), "an older volume must start folded: {}", volumes[0]);
        assert!(open_tag(volumes[1]), "the newest volume must start open: {}", volumes[1]);
        for v in &volumes {
            let summary = &v[v.find("<summary>").unwrap()..v.find("</summary>").unwrap()];
            assert!(summary.contains("/epub\""), "the volume EPUB link must be in the summary: {}", summary);
            assert!(summary.contains("/epub?stripped=1\""), "and the no-colour one: {}", summary);
        }
        assert!(
            body.contains("id=\"toggle-volumes\" class=\"toggle-all\" aria-controls=\"volumes\" hidden"),
            "the button starts hidden, for readers without JavaScript: {}",
            body
        );
        assert!(body.contains("data-storage-key=\"toc-open:test-source\""), "{}", body);
        assert!(body.contains("Folding volumes on the table-of-contents page."), "script missing");
    }

    /// A manga source is one flat chapter list: no fold, no volume checkbox
    /// or EPUB link, no collapse button; a pending chapter is listed without
    /// a checkbox.
    #[tokio::test]
    async fn a_manga_source_lists_chapters_flat() {
        use crate::config::{SourceConfig, SourceKind};
        use axum::extract::{Path as AxumPath, State};
        use std::sync::Arc;
        let state = Arc::new(crate::web::app::AppState::for_test_with_sources(vec![SourceConfig {
            id: "test-source".into(),
            enabled: true,
            kind: SourceKind::MangaWordpress,
            ..Default::default()
        }]));
        {
            let registry = state.registry.snapshot();
            let db = registry.get("test-source").unwrap().db();
            let vol = db.add_volume("Example Series").unwrap();
            db.add_chapter("Chapter", "https://example.com/c1", vol).unwrap();
            db.add_chapter("Pending one", "https://example.com/c2", vol).unwrap();
            let id = db.get_chapters_by_volume(vol).unwrap()[0].id;
            db.add_chapter_data(id, "<p>one two</p>").unwrap();
        }

        let body = body_of(
            super::source_toc(State(Arc::clone(&state)), AxumPath("test-source".to_string())).await,
        )
        .await;

        assert!(!body.contains("name=\"v\""), "no volume checkbox: {}", body);
        assert!(!body.contains("<details class=\"volume\""), "no fold: {}", body);
        assert!(!body.contains("/volume/"), "no volume EPUB link: {}", body);
        assert!(!body.contains("id=\"toggle-volumes\""), "no collapse button: {}", body);
        assert_eq!(body.matches("name=\"c\"").count(), 1, "one checkbox per downloaded chapter: {}", body);
        assert!(body.contains("Pending one") && body.contains(">pending<"), "pending chapter listed: {}", body);
        assert!(body.contains("Tick chapters to send them."), "{}", body);
    }

    #[test]
    fn the_folding_script_only_touches_the_page() {
        for forbidden in ["fetch", "XMLHttpRequest", "innerHTML", "outerHTML", "insertAdjacentHTML", "eval("] {
            assert!(!super::TOC_SCRIPT.contains(forbidden), "the folding script must not use {}", forbidden);
        }
        assert!(!super::TOC_SCRIPT.contains("</script"), "that would end the inline script early");
    }

    /// The send script is inlined on this page too, so it gets the same check.
    #[test]
    fn the_send_script_only_touches_the_page() {
        let script = crate::web::send::SEND_SCRIPT;
        for forbidden in ["fetch", "XMLHttpRequest", "innerHTML", "outerHTML", "insertAdjacentHTML", "eval(", "document.write"] {
            assert!(!script.contains(forbidden), "the send script must not use {}", forbidden);
        }
        assert!(!script.contains("</script"), "that would end the inline script early");
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

    /// Without `color-scheme` the frame's native form widgets and scrollbars
    /// render in light chrome regardless of the palette, no matter which
    /// reader theme is selected.
    #[test]
    fn every_reader_variant_declares_color_scheme() {
        let auto = super::reader_css(super::ReaderTheme::Auto, false);
        assert!(
            auto.contains("color-scheme: light"),
            "auto must start from the light color-scheme: {}",
            auto
        );
        assert!(
            auto.contains("color-scheme: dark"),
            "auto must switch to the dark color-scheme under prefers-color-scheme: {}",
            auto
        );

        let light = super::reader_css(super::ReaderTheme::Light, false);
        assert!(
            light.contains("color-scheme: light"),
            "the light theme must declare a light color-scheme: {}",
            light
        );
        assert!(
            !light.contains("color-scheme: dark"),
            "the light theme must not also declare a dark color-scheme: {}",
            light
        );

        let dark = super::reader_css(super::ReaderTheme::Dark, false);
        assert!(
            dark.contains("color-scheme: dark"),
            "an explicit dark choice must declare a dark color-scheme: {}",
            dark
        );
    }

    #[test]
    fn formats_thousands_separators() {
        assert_eq!(format_thousands(0), "0");
        assert_eq!(format_thousands(999), "999");
        assert_eq!(format_thousands(1000), "1,000");
        assert_eq!(format_thousands(26451980), "26,451,980");
    }

    /// With `VolumeEnds`, a manga source folds like a serial: `Volume N`
    /// headings with an EPUB link, no volume checkbox or colour link, the
    /// series volume last and without a volume link, chapters selectable
    /// everywhere.
    #[tokio::test]
    async fn a_manga_source_with_volume_ends_lists_folded_volumes_without_volume_checkboxes() {
        use crate::config::{SourceConfig, SourceKind};
        use axum::extract::{Path as AxumPath, State};
        use std::sync::Arc;
        let state = Arc::new(crate::web::app::AppState::for_test_with_sources_and_kcc(vec![SourceConfig {
            id: "test-source".into(),
            name: "Example Series".into(),
            enabled: true,
            kind: SourceKind::MangaWordpress,
            volume_ends: vec![1.0],
            ..Default::default()
        }], true));
        let (series, v1, v2) = {
            let registry = state.registry.snapshot();
            let db = registry.get("test-source").unwrap().db();
            let series = db.add_volume("Example Series").unwrap();
            let v1 = db.add_volume("Volume 1").unwrap();
            let v2 = db.add_volume("Volume 2").unwrap();
            db.add_chapter("Loose", "https://example.com/special", series).unwrap();
            db.add_chapter("Chapter 1", "https://example.com/c1", v1).unwrap();
            db.add_chapter("Chapter 2", "https://example.com/c2", v2).unwrap();
            for chapter in db.get_chapters_by_volume(series).unwrap().into_iter().chain(db.get_chapters_by_volume(v1).unwrap()) {
                db.add_chapter_data(chapter.id, "<p>one</p>").unwrap();
            }
            (series, v1, v2)
        };

        let body = body_of(
            super::source_toc(State(Arc::clone(&state)), AxumPath("test-source".to_string())).await,
        )
        .await;

        assert!(body.contains("<details class=\"volume\""), "folded: {}", body);
        assert!(body.contains("id=\"toggle-volumes\""), "{}", body);
        assert!(!body.contains("name=\"v\""), "no volume checkbox: {}", body);
        assert!(!body.contains("stripped=1"), "no colour link: {}", body);
        assert!(body.contains(&format!("/volume/{}/epub\"", v1)), "Volume 1 link: {}", body);
        assert!(!body.contains(&format!("/volume/{}/epub", series)), "series volume has no link: {}", body);
        assert!(!body.contains(&format!("/volume/{}/epub", v2)), "a volume with nothing downloaded has no link: {}", body);
        assert_eq!(body.matches("name=\"c\"").count(), 2, "every downloaded chapter is selectable: {}", body);
        let pos = |needle: &str| body.find(needle).unwrap_or_else(|| panic!("{} missing: {}", needle, body));
        assert!(pos("<h2>Volume 1</h2>") < pos("<h2>Volume 2</h2>") && pos("<h2>Volume 2</h2>") < pos("<h2>Example Series</h2>"), "series volume last: {}", body);
    }

    /// `VolumeEnds` without Kindle Comic Converter groups the page but
    /// offers no volume EPUB link, since none is built.
    #[tokio::test]
    async fn volume_ends_without_kcc_folds_but_offers_no_volume_link() {
        use crate::config::{SourceConfig, SourceKind};
        use axum::extract::{Path as AxumPath, State};
        use std::sync::Arc;
        let state = Arc::new(crate::web::app::AppState::for_test_with_sources_and_kcc(vec![SourceConfig {
            id: "test-source".into(),
            name: "Example Series".into(),
            enabled: true,
            kind: SourceKind::MangaWordpress,
            volume_ends: vec![1.0],
            ..Default::default()
        }], false));
        {
            let registry = state.registry.snapshot();
            let db = registry.get("test-source").unwrap().db();
            let v1 = db.add_volume("Volume 1").unwrap();
            db.add_chapter("Chapter 1", "https://example.com/c1", v1).unwrap();
            let id = db.get_chapters_by_volume(v1).unwrap()[0].id;
            db.add_chapter_data(id, "<p>one</p>").unwrap();
        }

        let body = body_of(
            super::source_toc(State(Arc::clone(&state)), AxumPath("test-source".to_string())).await,
        )
        .await;

        assert!(body.contains("<details class=\"volume\""), "{}", body);
        assert!(!body.contains("/volume/"), "no volume link without a builder: {}", body);
        assert!(body.contains("name=\"c\""), "{}", body);
    }
}
