//! Shared page chrome and the overview and statistics pages.
//!
//! Markup is a compile-time `maud` macro, so there is no template directory to
//! resolve at runtime, package into the `.deb`, or get wrong. Same reasoning as
//! embedding the EPUB stylesheet.

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use maud::{html, Markup, PreEscaped, DOCTYPE};

use crate::stats::cache::SourceStat;
use crate::web::app::AppState;
use crate::web::toc::format_thousands;

const STYLE: &str = r#"
:root { --fg:#1c1c1c; --muted:#666; --line:#e2e2e2; --accent:#3a5a8c; --bg:#fdfdfc; }
* { box-sizing: border-box; }
body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
       margin:0; background:var(--bg); color:var(--fg); line-height:1.55; }
header { border-bottom:1px solid var(--line); padding:0.85rem 1.5rem;
         display:flex; gap:1.25rem; align-items:baseline; }
header a { color:var(--accent); text-decoration:none; font-weight:500; }
header .spacer { margin-left:auto; }
main { max-width:62rem; margin:0 auto; padding:1.5rem; }
h1 { font-size:1.5rem; margin:0 0 0.35rem; }
h2 { font-size:1.15rem; margin:1.75rem 0 0.35rem; }
.summary { color:var(--muted); font-size:0.9rem; margin:0 0 0.75rem; }
.summary a { color:var(--accent); }
table { border-collapse:collapse; width:100%; font-size:0.92rem; }
th, td { text-align:left; padding:0.4rem 0.65rem; border-bottom:1px solid var(--line); }
td.num, th.num { text-align:right; font-variant-numeric:tabular-nums; }
ol.chapters { list-style:none; padding:0; margin:0; }
ol.chapters li { display:flex; gap:0.75rem; align-items:baseline;
                 padding:0.3rem 0; border-bottom:1px solid var(--line); font-size:0.92rem; }
ol.chapters li a:first-child { flex:1; color:var(--fg); text-decoration:none; }
ol.chapters li a:first-child:hover { text-decoration:underline; }
ol.chapters .words, ol.chapters .date { color:var(--muted); font-size:0.82rem;
                                        font-variant-numeric:tabular-nums; }
ol.chapters .dl { color:var(--accent); font-size:0.82rem; text-decoration:none; }
iframe.reader { width:100%; height:78vh; border:1px solid var(--line);
                background:#fff; border-radius:3px; }
form.login { max-width:20rem; margin:5rem auto; display:flex; flex-direction:column; gap:0.5rem; }
label { font-size:0.85rem; color:var(--muted); margin-top:0.5rem; }
input, textarea { font:inherit; padding:0.45rem 0.6rem; border:1px solid var(--line);
                  border-radius:3px; background:#fff; width:100%; }
textarea { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size:0.82rem; }
button { font:inherit; margin-top:0.85rem; padding:0.45rem 1.1rem; border:0; border-radius:3px;
         background:var(--accent); color:#fff; cursor:pointer; align-self:flex-start; }
.note { background:#fff8e6; border:1px solid #f0dca0; padding:0.6rem 0.8rem;
        border-radius:3px; font-size:0.88rem; }
.error { color:#a3272c; } .ok { color:#2a7a3f; }
footer.version { color:var(--muted); font-size:0.75rem; text-align:right;
                 padding:1rem 1.5rem; font-variant-numeric:tabular-nums; }
#status { margin-left:0.75rem; font-size:0.88rem; }
"#;

/// Shared chrome. Every page goes through here.
pub fn page(title: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) " — Scraper" }
                style { (PreEscaped(STYLE)) }
            }
            body {
                header {
                    a href="/" { "Sources" }
                    a href="/stats" { "Statistics" }
                    a href="/config" { "Configuration" }
                    span class="spacer" {}
                    a href="/logout" { "Sign out" }
                }
                // The single h1 lives here, not in each handler. Task 10's
                // handlers already omit their own heading on this basis, so
                // removing it here would silently strip the heading from every
                // table-of-contents and chapter page.
                main { h1 { (title) } (body) }
                // The scraper and the server are the same binary. A package
                // upgrade replaces the file without restarting a long-running
                // process, so a scheduled run can exec new code while this
                // process still serves the old from memory. Reporting the built
                // version makes that skew visible rather than inferred.
                footer class="version" { "v" (env!("CARGO_PKG_VERSION")) }
            }
        }
    }
}

/// Statistics for every registered source, with the ones whose database could
/// not be read reported separately instead of dropped.
///
/// An aggregate page must not go blank because one source's database is
/// broken, and it must not quietly present a total that is missing a source
/// either. The cause goes to the log; the page names the sources it could not
/// read so the omission is visible from the browser.
fn collect_stats(state: &AppState, page_name: &str) -> (Vec<Arc<SourceStat>>, Vec<String>) {
    let mut stats = Vec::new();
    let mut unreadable = Vec::new();

    for entry in state.registry.entries() {
        match state.stats.get(entry) {
            Ok(stat) => stats.push(stat),
            Err(e) => {
                eprintln!(
                    "statistics scan failed for {} while rendering {}: {}",
                    entry.config.id, page_name, e
                );
                unreadable.push(entry.config.id.clone());
            }
        }
    }

    (stats, unreadable)
}

/// The names of sources whose statistics could not be computed, or nothing.
fn unreadable_note(unreadable: &[String]) -> Markup {
    html! {
        @if !unreadable.is_empty() {
            p class="error" {
                "Could not read statistics for: " (unreadable.join(", "))
                ". The totals below exclude them; see the server log for the cause."
            }
        }
    }
}

pub async fn index(State(state): State<Arc<AppState>>) -> Response {
    let (stats, unreadable) = collect_stats(&state, "the source overview");

    let total_words: usize = stats.iter().map(|s| s.total_words).sum();
    let total_chapters: usize = stats.iter().map(|s| s.total_chapters).sum();
    let total_pending: usize = stats.iter().map(|s| s.pending_chapters).sum();

    page(
        "Sources",
        html! {
            (unreadable_note(&unreadable))
            p class="summary" {
                (stats.len()) " sources · " (format_thousands(total_chapters)) " chapters · "
                (format_thousands(total_words)) " words"
                // total_chapters/total_words count only downloaded chapters,
                // matching wordcount.py — a pending chapter has no raw_data
                // row and would otherwise silently vanish from the total
                // rather than being counted or explained.
                @if total_pending > 0 {
                    " · " (format_thousands(total_pending)) " pending"
                }
            }
            table {
                thead {
                    tr {
                        th { "Source" }
                        th class="num" { "Volumes" }
                        th class="num" { "Chapters" }
                        th class="num" { "Words" }
                        th class="num" { "Pending" }
                        th { "Latest" }
                    }
                }
                tbody {
                    @for stat in &stats {
                        tr {
                            td { a href={ "/source/" (stat.source_id) } { (stat.name) } }
                            td class="num" { (stat.volumes.len()) }
                            td class="num" { (format_thousands(stat.total_chapters)) }
                            td class="num" { (format_thousands(stat.total_words)) }
                            td class="num" {
                                @if stat.pending_chapters > 0 {
                                    (format_thousands(stat.pending_chapters))
                                } @else {
                                    "—"
                                }
                            }
                            td { @match &stat.latest_published {
                                Some(d) => (d),
                                // Royal Road stores no date anywhere.
                                None => "—",
                            } }
                        }
                    }
                }
            }
        },
    )
    .into_response()
}

pub async fn stats_page(State(state): State<Arc<AppState>>) -> Response {
    let (stats, unreadable) = collect_stats(&state, "the statistics page");

    page(
        "Statistics",
        html! {
            (unreadable_note(&unreadable))
            p class="summary" {
                "Word counts exclude the contents of style and script elements. "
                "Dates come from the chapter URL where the source provides one; "
                "Royal Road does not, so those show a dash. Chapter and word "
                "totals count downloaded chapters only; pending chapters are "
                "listed separately."
            }
            @for stat in &stats {
                h2 { (stat.name) }
                p class="summary" {
                    (format_thousands(stat.total_chapters)) " chapters · "
                    (format_thousands(stat.total_words)) " words · "
                    "mean " (format_thousands(stat.mean_chapter_words)) " words per chapter"
                    @if stat.pending_chapters > 0 {
                        " · " (format_thousands(stat.pending_chapters)) " pending"
                    }
                }
                table {
                    thead {
                        tr {
                            th { "Volume" }
                            th class="num" { "Chapters" }
                            th class="num" { "Words" }
                            th class="num" { "Mean" }
                            th class="num" { "Pending" }
                            th { "EPUB" }
                        }
                    }
                    tbody {
                        @for volume in &stat.volumes {
                            // `volume.chapters` includes pending (undownloaded)
                            // chapters so the TOC listing stays complete, but
                            // they carry `words: 0` — counting them here would
                            // dilute "Mean" the same way folding them into
                            // `total_words` would, so subtract them out first.
                            @let downloaded_chapters =
                                volume.chapters.len() - volume.pending_chapters;
                            tr {
                                td { (volume.name) }
                                td class="num" { (downloaded_chapters) }
                                td class="num" { (format_thousands(volume.words)) }
                                td class="num" {
                                    (format_thousands(
                                        if downloaded_chapters == 0 { 0 }
                                        else { volume.words / downloaded_chapters }
                                    ))
                                }
                                td class="num" {
                                    @if volume.pending_chapters > 0 {
                                        (format_thousands(volume.pending_chapters))
                                    } @else {
                                        "—"
                                    }
                                }
                                td {
                                    a href={ "/source/" (stat.source_id)
                                             "/volume/" (volume.id) "/epub" } { "EPUB" }
                                    " · "
                                    a href={ "/source/" (stat.source_id)
                                             "/volume/" (volume.id) "/epub?stripped=1" } {
                                        "no colour"
                                    }
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
