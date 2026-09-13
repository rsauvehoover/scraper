//! Shared page chrome and the overview and statistics pages.
//!
//! Markup is a compile-time `maud` macro, so there is no template directory to
//! resolve at runtime, package into the `.deb`, or get wrong. Same reasoning as
//! embedding the EPUB stylesheet.

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use maud::{html, Markup, PreEscaped, DOCTYPE};

use crate::db::{SkipReason, SkippedSource};
use crate::stats::cache::SourceStat;
use crate::web::app::AppState;
use crate::web::toc::format_thousands;

/// Every colour in the interface, and nothing else.
///
/// Three blocks, deliberately: bare `:root` carries the complete light
/// palette, so no colour has its only definition inside a media query — a
/// browser that never matches one still gets a full set. The
/// `prefers-color-scheme` block is guarded with `:not([data-theme="light"])`
/// so an operator who explicitly picks light keeps it on a dark desktop, and
/// `[data-theme="dark"]` repeats the dark values unguarded so the toggle wins
/// in the other direction too. Dropping either guard makes the toggle a
/// one-way door on a machine whose system preference disagrees.
///
/// `BASE` below must contain no literal colour: a hardcoded one survives the
/// swap and is wrong in whichever theme it was not written for. There is a
/// test for that.
const PALETTE: &str = r#"
:root {
  color-scheme: light;
  --fg:#1c1c1c; --muted:#666666; --line:#e2e2e2; --accent:#3a5a8c;
  --bg:#fdfdfc; --surface:#ffffff; --on-accent:#ffffff;
  --note-bg:#fff8e6; --note-line:#f0dca0; --error:#a3272c; --ok:#2a7a3f;
}
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) {
    color-scheme: dark;
    --fg:#e4e4e2; --muted:#9b9b97; --line:#33333a; --accent:#8ab0e4;
    --bg:#17171a; --surface:#1f1f24; --on-accent:#10131a;
    --note-bg:#2d2718; --note-line:#5c4f26; --error:#ef8a8f; --ok:#79c98d;
  }
}
:root[data-theme="dark"] {
  color-scheme: dark;
  --fg:#e4e4e2; --muted:#9b9b97; --line:#33333a; --accent:#8ab0e4;
  --bg:#17171a; --surface:#1f1f24; --on-accent:#10131a;
  --note-bg:#2d2718; --note-line:#5c4f26; --error:#ef8a8f; --ok:#79c98d;
}
"#;

const BASE: &str = r#"
* { box-sizing: border-box; }
body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
       margin:0; background:var(--bg); color:var(--fg); line-height:1.55; }
a { color:var(--accent); }
a:visited { color:var(--accent); }
header { border-bottom:1px solid var(--line); padding:0.85rem 1.5rem;
         display:flex; gap:1.25rem; align-items:baseline; }
header a { color:var(--accent); text-decoration:none; font-weight:500; }
header .spacer { margin-left:auto; }
button.theme { margin:0; padding:0.2rem 0.7rem; background:transparent; color:var(--accent);
               border:1px solid var(--line); border-radius:3px; font-size:0.85rem;
               align-self:center; }
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
                background:var(--surface); border-radius:3px; }
form.login { max-width:20rem; margin:5rem auto; display:flex; flex-direction:column; gap:0.5rem; }
label { font-size:0.85rem; color:var(--muted); margin-top:0.5rem; }
input, textarea, select { font:inherit; padding:0.45rem 0.6rem; border:1px solid var(--line);
                  border-radius:3px; background:var(--surface); color:var(--fg); width:100%; }
textarea { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size:0.82rem; }
button { font:inherit; margin-top:0.85rem; padding:0.45rem 1.1rem; border:0; border-radius:3px;
         background:var(--accent); color:var(--on-accent); cursor:pointer; align-self:flex-start; }
.note { background:var(--note-bg); border:1px solid var(--note-line); padding:0.6rem 0.8rem;
        border-radius:3px; font-size:0.88rem; }
.error { color:var(--error); } .ok { color:var(--ok); }
footer.version { color:var(--muted); font-size:0.75rem; text-align:right;
                 padding:1rem 1.5rem; font-variant-numeric:tabular-nums; }
#status { margin-left:0.75rem; font-size:0.88rem; }
details.adder { border:1px solid var(--line); border-radius:3px; background:var(--surface);
                padding:0.6rem 0.9rem; margin:1rem 0; }
details.adder summary { cursor:pointer; font-weight:500; font-size:0.92rem; }
/* One grid cell per label-and-input pair. Putting the label and the input in
   cells of their own lets a row wrap between them, which pairs every label
   with the next field's box. */
.fields { display:grid; grid-template-columns:repeat(auto-fit, minmax(15rem, 1fr));
          gap:0 1.25rem; align-items:start; }
.fields .field label { display:block; margin-top:0.6rem; }
.checks { margin-top:0.6rem; }
.checks label, .picks label { display:inline-flex; align-items:center; margin-right:1.25rem;
                              color:var(--fg); font-size:0.85rem; }
.picks { display:flex; flex-wrap:wrap; margin-top:0.35rem; font-size:0.85rem;
         color:var(--muted); }
input[type="checkbox"] { width:auto; margin-right:0.35rem; }
.adder button { margin-top:1rem; }
.adder .form-status { margin-left:0.75rem; font-size:0.88rem; }
"#;

/// Runs in `<head>`, before the body is parsed, so the stored choice is on the
/// document element by the time anything paints. Deferring this to the script
/// at the end of the page would render one frame of the wrong theme on every
/// load. `localStorage` throws outright in a blocked-site-data or private
/// context rather than returning null, so the read is wrapped — a page that
/// cannot remember the choice must still render.
const PREPAINT_SCRIPT: &str = r#"
(function () {
  var t = null;
  try { t = window.localStorage.getItem('scraper-theme'); } catch (e) { t = null; }
  if (t === 'light' || t === 'dark') {
    document.documentElement.setAttribute('data-theme', t);
  }
})();
"#;

/// The toggle, plus the one thing the toggle cannot do with CSS alone.
///
/// `iframe.reader` is a separate document served by `chapter_raw` inside
/// `sandbox=""` with no `allow-same-origin`, so it cannot read this origin's
/// `localStorage` and no stylesheet here reaches inside it. The theme
/// therefore travels in its URL. The server renders the frame with a bare
/// `src`, which means "follow `prefers-color-scheme`" — so the frame is only
/// re-pointed when an explicit choice actually disagrees with the system
/// preference, and the common case costs no second fetch.
const THEME_SCRIPT: &str = r#"
(function () {
  var KEY = 'scraper-theme';
  var root = document.documentElement;
  var btn = document.getElementById('theme-toggle');

  function systemTheme() {
    return window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches
      ? 'dark' : 'light';
  }
  // The attribute is authoritative: the pre-paint script has already copied
  // storage into it, and it keeps working for the rest of the page even when
  // localStorage refuses every write.
  function resolved() {
    var a = root.getAttribute('data-theme');
    if (a === 'light' || a === 'dark') return a;
    return systemTheme();
  }
  function relabel() {
    if (!btn) return;
    var dark = resolved() === 'dark';
    btn.textContent = dark ? 'Light mode' : 'Dark mode';
    btn.setAttribute('aria-pressed', dark ? 'true' : 'false');
  }
  function syncReader() {
    var frame = document.querySelector('iframe.reader');
    if (!frame || !frame.src) return;
    var base = frame.src.split('?')[0];
    var theme = resolved();
    var want = theme === systemTheme() ? base : base + '?theme=' + theme;
    if (frame.src !== want) frame.src = want;
  }

  if (btn) {
    btn.addEventListener('click', function () {
      var next = resolved() === 'dark' ? 'light' : 'dark';
      root.setAttribute('data-theme', next);
      try { window.localStorage.setItem(KEY, next); } catch (e) {}
      relabel();
      syncReader();
    });
  }
  relabel();
  syncReader();
})();
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
                style { (PreEscaped(PALETTE)) (PreEscaped(BASE)) }
                // Before <body>, on purpose. See PREPAINT_SCRIPT.
                script { (PreEscaped(PREPAINT_SCRIPT)) }
            }
            body {
                header {
                    a href="/" { "Sources" }
                    a href="/stats" { "Statistics" }
                    a href="/config" { "Configuration" }
                    span class="spacer" {}
                    // Labelled by THEME_SCRIPT once it knows which way round
                    // the toggle currently sits; the static text is what a
                    // browser with scripting off is left holding.
                    button type="button" id="theme-toggle" class="theme" aria-pressed="false" {
                        "Dark mode"
                    }
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
                script { (PreEscaped(THEME_SCRIPT)) }
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
                // The log line above carries the id, which is what an operator
                // greps config.json for. The page carries the display name,
                // matching both the table below it and `skipped_note` — one
                // page must not name the same source two different ways.
                unreadable.push(entry.config.name.clone());
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

/// Sources present in config that never made it into the registry at all —
/// one step earlier than `unreadable_note`, which only covers a source that
/// registered and then faulted.
///
/// Same reasoning as `unreadable_note`, same markup shape, placed with it:
/// a source silently missing from this page must not be indistinguishable
/// from a source that was never configured. The two `SkipReason`s call for
/// different operator action, so they render as two distinct notes rather
/// than one list — and, per the rule already established on this page,
/// only the source names reach the response; the cause stays in the log.
fn skipped_note(skipped: &[SkippedSource]) -> Markup {
    let not_yet_scraped: Vec<&str> = skipped
        .iter()
        .filter(|s| s.reason == SkipReason::NotYetScraped)
        .map(|s| s.name.as_str())
        .collect();
    let broken: Vec<&str> = skipped
        .iter()
        .filter(|s| s.reason == SkipReason::Broken)
        .map(|s| s.name.as_str())
        .collect();

    html! {
        @if !not_yet_scraped.is_empty() {
            p class="note" {
                "Configured but not yet scraped: " (not_yet_scraped.join(", "))
                ". It will appear here after the next scrape."
            }
        }
        @if !broken.is_empty() {
            p class="error" {
                "Configured but unreadable: " (broken.join(", "))
                ". See the server log for the cause."
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
            (skipped_note(state.registry.skipped()))
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
            (skipped_note(state.registry.skipped()))
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

#[cfg(test)]
mod tests {
    use super::{index, page, stats_page, BASE, PALETTE, PREPAINT_SCRIPT, THEME_SCRIPT};
    use std::collections::HashMap;

    /// A colour left hardcoded in the rules is simply wrong in one of the two
    /// themes, and the mistake is invisible to whoever is looking at the theme
    /// it was written for. Every colour belongs in `PALETTE`, which is the only
    /// place the swap happens.
    #[test]
    fn no_rule_outside_the_palette_carries_a_literal_colour() {
        let bytes: Vec<char> = BASE.chars().collect();
        for (i, c) in bytes.iter().enumerate() {
            if *c != '#' {
                continue;
            }
            // `#status` is an id selector, not a colour.
            let next = bytes.get(i + 1).copied().unwrap_or(' ');
            assert!(
                !next.is_ascii_hexdigit(),
                "hardcoded colour in BASE near byte {}: {}",
                i,
                &BASE[i.saturating_sub(40)..(i + 40).min(BASE.len())]
            );
        }
        assert!(
            !BASE.contains("rgb(") && !BASE.contains("hsl("),
            "BASE must express colour only through custom properties: {}",
            BASE
        );
    }

    /// Bare `:root` has to carry the whole light palette, the media query has
    /// to be guarded against an explicit light choice, and the dark values have
    /// to exist outside the media query too or the toggle is a one-way door on
    /// a machine whose system preference is dark.
    #[test]
    fn palette_defines_light_dark_and_both_overrides() {
        assert!(PALETTE.contains("\n:root {"), "bare :root light palette missing: {}", PALETTE);
        assert!(
            PALETTE.contains(":root:not([data-theme=\"light\"])"),
            "the prefers-color-scheme block must not override an explicit light choice: {}",
            PALETTE
        );
        assert!(
            PALETTE.contains(":root[data-theme=\"dark\"]"),
            "an explicit dark choice must win without a media query: {}",
            PALETTE
        );
        // Every property named once in the light block must be redefined in
        // both override blocks, or it keeps its light value in dark mode.
        let light = PALETTE.split("@media").next().unwrap();
        let dark_blocks: Vec<&str> = PALETTE.match_indices(":root[data-theme=\"dark\"]").map(|(i, _)| &PALETTE[i..]).collect();
        let media = PALETTE.split("@media").nth(1).unwrap();
        for property in light.split("--").skip(1) {
            let name = property.split(':').next().unwrap();
            assert!(
                media.contains(&format!("--{}:", name)),
                "--{} has no dark value in the media query",
                name
            );
            assert!(
                dark_blocks[0].contains(&format!("--{}:", name)),
                "--{} has no dark value in the data-theme block",
                name
            );
        }
    }

    /// The stored choice has to be on the document element before the body is
    /// parsed. Running it at the end of the page renders one frame of the wrong
    /// theme on every single load.
    #[test]
    fn the_theme_script_runs_before_the_body() {
        let rendered = page("Sources", maud::html! {}).into_string();
        let prepaint = rendered.find("scraper-theme").expect("pre-paint script is rendered");
        let body = rendered.find("<body").expect("the page has a body");
        assert!(
            prepaint < body,
            "the theme must be applied before the body renders: {}",
            rendered
        );
    }

    /// The toggle is in the header, next to Sign out, and every storage access
    /// is guarded — blocked site data throws on access rather than returning
    /// null, and an exception there would take the rest of the script with it.
    #[test]
    fn the_header_carries_a_guarded_theme_toggle() {
        let rendered = page("Sources", maud::html! {}).into_string();
        assert!(
            rendered.contains(r#"id="theme-toggle""#),
            "the header must offer a theme toggle: {}",
            rendered
        );
        let toggle = rendered.find("theme-toggle").expect("toggle is rendered");
        let sign_out = rendered.find("/logout").expect("sign out is rendered");
        assert!(
            toggle < sign_out,
            "the toggle belongs beside Sign out on the right: {}",
            rendered
        );

        for script in &[PREPAINT_SCRIPT, THEME_SCRIPT] {
            // The trailing dot counts accesses, not the word: the scripts
            // also mention localStorage in a comment.
            let reads = script.matches("localStorage.").count();
            assert!(reads > 0, "script must consult localStorage: {}", script);
            assert_eq!(
                script.matches("try {").count(),
                reads,
                "every localStorage access must be wrapped in try/catch: {}",
                script
            );
        }
    }

    /// The sandboxed reader cannot read this origin's storage, so the toggle
    /// has to re-point its URL. Losing this line leaves a light chapter body
    /// inside a dark page.
    #[test]
    fn the_toggle_repoints_the_reader_frame() {
        assert!(
            THEME_SCRIPT.contains("iframe.reader") && THEME_SCRIPT.contains("?theme="),
            "the toggle must carry the theme into the sandboxed reader: {}",
            THEME_SCRIPT
        );
    }

    /// A link inside a table cell (or anywhere else with no more specific
    /// selector) must not fall back to the browser's own blue/purple
    /// defaults. `a:visited` matters as much as the base rule: without it,
    /// any link the operator has actually clicked keeps the browser's default
    /// visited purple regardless of what `a` says.
    #[test]
    fn every_link_has_a_declared_colour_including_visited() {
        assert!(
            BASE.contains("a { color:var(--accent); }"),
            "a base `a` colour rule using the palette is required: {}",
            BASE
        );
        assert!(
            BASE.contains("a:visited { color:var(--accent); }"),
            "an `a:visited` rule using the palette is required, or the \
             browser's visited-link purple wins for any clicked link: {}",
            BASE
        );

        // The base rule has to come before the more specific selectors so
        // those still win the cascade.
        let base_pos = BASE.find("a { color:var(--accent); }").unwrap();
        let header_pos = BASE.find("header a {").unwrap();
        let summary_pos = BASE.find(".summary a {").unwrap();
        assert!(
            base_pos < header_pos && base_pos < summary_pos,
            "the base `a` rule must precede the more specific link rules: {}",
            BASE
        );

        // The one deliberate exception: chapter titles read as text, not
        // links, and must keep using --fg rather than the new base rule.
        assert!(
            BASE.contains("ol.chapters li a:first-child { flex:1; color:var(--fg);"),
            "chapter titles must keep their deliberate --fg colour: {}",
            BASE
        );
    }

    /// Without `color-scheme` the browser renders native widgets (form
    /// controls, scrollbars, `::placeholder` text) in light chrome no matter
    /// what the palette says, because nothing here told it a dark theme was
    /// in play.
    #[test]
    fn color_scheme_is_declared_in_every_root_block() {
        let light = PALETTE.split("@media").next().unwrap();
        let media = PALETTE.split("@media").nth(1).unwrap();
        let dark_blocks: Vec<&str> = PALETTE
            .match_indices(":root[data-theme=\"dark\"]")
            .map(|(i, _)| &PALETTE[i..])
            .collect();

        assert!(
            light.contains("color-scheme: light;"),
            "the bare :root block must declare color-scheme: light: {}",
            light
        );
        assert!(
            media.contains("color-scheme: dark;"),
            "the prefers-color-scheme block must declare color-scheme: dark: {}",
            media
        );
        assert!(
            dark_blocks[0].contains("color-scheme: dark;"),
            "the [data-theme=\"dark\"] block must declare color-scheme: dark: {}",
            dark_blocks[0]
        );
    }

    /// sRGB relative luminance per the WCAG formula, on a 0..=1 linear scale.
    fn relative_luminance((r, g, b): (u8, u8, u8)) -> f64 {
        fn linearise(channel: u8) -> f64 {
            let c = channel as f64 / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * linearise(r) + 0.7152 * linearise(g) + 0.0722 * linearise(b)
    }

    /// WCAG contrast ratio between two colours, order-independent.
    fn contrast_ratio(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
        let (la, lb) = (relative_luminance(a), relative_luminance(b));
        let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    fn parse_hex(value: &str) -> (u8, u8, u8) {
        let value = value.trim().trim_start_matches('#');
        assert_eq!(value.len(), 6, "expected a 6-digit hex colour, got {}", value);
        let byte = |i: usize| u8::from_str_radix(&value[i..i + 2], 16).unwrap();
        (byte(0), byte(2), byte(4))
    }

    /// Pulls every `--name:#rrggbb` declaration out of one `:root`-style
    /// block, so the test checks whatever the palette currently says rather
    /// than a copy pasted into the test.
    fn parse_palette_vars(block: &str) -> HashMap<String, (u8, u8, u8)> {
        let mut vars = HashMap::new();
        for decl in block.split(';') {
            let decl = decl.trim();
            let Some(rest) = decl.strip_prefix("--") else { continue };
            let Some((name, value)) = rest.split_once(':') else { continue };
            if !value.trim().starts_with('#') {
                continue;
            }
            vars.insert(name.trim().to_string(), parse_hex(value));
        }
        vars
    }

    /// Extracts the declaration list of one `selector { ... }` block,
    /// assuming (as is true of every block here) that it contains no nested
    /// braces of its own.
    fn extract_block<'a>(css: &'a str, selector: &str) -> &'a str {
        let start = css.find(selector).unwrap_or_else(|| panic!("{} not found in: {}", selector, css));
        let open = css[start..].find('{').map(|i| start + i).unwrap();
        let close = css[open..].find('}').map(|i| open + i).unwrap();
        &css[open + 1..close]
    }

    /// Every foreground colour the palette defines must read against every
    /// background it is meant to sit on, at the 4.5:1 WCAG AA threshold for
    /// body text, in both themes. This is exactly the class of bug that let
    /// browser-default link blue through undetected: a colour pairing that
    /// nothing checked.
    #[test]
    fn every_palette_pair_meets_wcag_aa_contrast() {
        let light = parse_palette_vars(extract_block(PALETTE, "\n:root {"));
        let dark = parse_palette_vars(extract_block(PALETTE, ":root[data-theme=\"dark\"]"));

        for (theme_name, vars) in [("light", &light), ("dark", &dark)] {
            let get = |name: &str| {
                *vars
                    .get(name)
                    .unwrap_or_else(|| panic!("--{} missing from the {} palette", name, theme_name))
            };
            let fg = get("fg");
            let muted = get("muted");
            let accent = get("accent");
            let bg = get("bg");
            let surface = get("surface");
            let on_accent = get("on-accent");

            for (fg_name, fg_colour) in [("fg", fg), ("muted", muted), ("accent", accent)] {
                for (bg_name, bg_colour) in [("bg", bg), ("surface", surface)] {
                    let ratio = contrast_ratio(fg_colour, bg_colour);
                    assert!(
                        ratio >= 4.5,
                        "{} theme: --{} on --{} is only {:.2}:1, needs 4.5:1",
                        theme_name,
                        fg_name,
                        bg_name,
                        ratio
                    );
                }
            }

            let ratio = contrast_ratio(on_accent, accent);
            assert!(
                ratio >= 4.5,
                "{} theme: --on-accent on --accent is only {:.2}:1, needs 4.5:1",
                theme_name,
                ratio
            );
        }
    }

    /// Restores the process cwd on drop, including on unwind from a panic.
    /// Mirrors the same-named helper duplicated in `db::registry`,
    /// `db::connection` and `stats::cache` — `SourceDatabase::open` and
    /// `open_query_only` both resolve `db/` relative to the cwd.
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

    fn fixture_source(id: &str, name: &str) -> crate::config::SourceConfig {
        crate::config::SourceConfig {
            id: id.to_string(),
            name: name.to_string(),
            enabled: true,
            ..crate::config::SourceConfig::default()
        }
    }

    /// Lays out four sources on disk under the current (already-redirected)
    /// cwd, one per state this branch now distinguishes, and returns the
    /// `AppState` over them:
    ///
    /// - `healthy-source`: a real, complete schema — scans cleanly.
    /// - `faulted-source`: has the three tables `has_scraper_schema` checks
    ///   for, so the registry admits it, but they are missing the columns
    ///   `compute` actually selects — so it registers and then faults on
    ///   every scan, same as `collect_stats`'s pre-existing `unreadable`.
    /// - `never-scraped-source`: no database file at all.
    /// - `broken-source`: the path exists but is a directory, not a
    ///   database — stands in for permissions, corruption, or any other
    ///   open failure that is not "never scraped".
    fn build_test_state() -> std::sync::Arc<crate::web::app::AppState> {
        use crate::db::{SourceDatabase, SourceRegistry};
        use crate::stats::cache::StatsCache;
        use crate::web::app::AppState;
        use crate::web::auth::{hash_password, RateLimiter, SessionStore};

        std::fs::create_dir_all("db").unwrap();

        SourceDatabase::open("healthy-source").unwrap();

        let conn = rusqlite::Connection::open("db/faulted-source.db").unwrap();
        conn.execute_batch(
            "CREATE TABLE volumes (id INTEGER PRIMARY KEY);
             CREATE TABLE chapters (id INTEGER PRIMARY KEY);
             CREATE TABLE raw_data (id INTEGER PRIMARY KEY);",
        )
        .unwrap();

        std::fs::create_dir_all("db/broken-source.db").unwrap();

        let mut config = crate::config::Config::default();
        config.sources = vec![
            fixture_source("healthy-source", "Healthy Source"),
            fixture_source("faulted-source", "Faulted Source"),
            fixture_source("never-scraped-source", "Never Scraped Source"),
            fixture_source("broken-source", "Broken Source"),
        ];

        std::sync::Arc::new(AppState {
            registry: SourceRegistry::from_config(&config),
            stats: StatsCache::new(),
            sessions: SessionStore::new(std::time::Duration::from_secs(3600)),
            limiter: RateLimiter::new(10, std::time::Duration::from_secs(900)),
            credential: hash_password("hunter2-hunter2").unwrap(),
            config_path: std::path::PathBuf::from("config.json"),
            secure_cookies: false,
            trust_forwarded_for: false,
            epub_permits: std::sync::Arc::new(tokio::sync::Semaphore::new(2)),
        })
    }

    async fn body_of(response: axum::response::Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// One error text a missing-file open failure produces, and one an
    /// empty/schema-less database's failed query produces. Neither may ever
    /// reach a response body — only the source names and the fixed wording
    /// may.
    fn asserts_no_error_text_leaked(body: &str) {
        assert!(
            !body.contains("no such table"),
            "a schema fault's error text leaked into the page: {}",
            body
        );
        assert!(
            !body.contains("unable to open database"),
            "an open failure's error text leaked into the page: {}",
            body
        );
        assert!(
            !body.contains("Is a directory") && !body.contains("CANTOPEN"),
            "the broken-database error text leaked into the page: {}",
            body
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn index_names_skipped_sources_alongside_unreadable_ones() {
        let dir = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::change_to(dir.path());
        let state = build_test_state();

        let body = body_of(index(axum::extract::State(state)).await).await;

        assert!(body.contains("Healthy Source"), "a healthy source must still render: {}", body);
        assert!(
            body.contains("Never Scraped Source"),
            "a source with no database yet must be named on the page: {}",
            body
        );
        assert!(
            body.contains("It will appear here after the next scrape"),
            "the not-yet-scraped case must say what happens next: {}",
            body
        );
        assert!(
            body.contains("Broken Source"),
            "a source whose database could not be opened must be named: {}",
            body
        );
        assert!(
            // Named the way `skipped_note` names its sources and the way the
            // table below names them: by display name, not by config id.
            body.contains("Could not read statistics for: Faulted Source"),
            "a registered-then-faulted source must still get its existing note: {}",
            body
        );
        asserts_no_error_text_leaked(&body);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn stats_page_names_skipped_sources_alongside_unreadable_ones() {
        let dir = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::change_to(dir.path());
        let state = build_test_state();

        let body = body_of(stats_page(axum::extract::State(state)).await).await;

        assert!(body.contains("Healthy Source"));
        assert!(body.contains("Never Scraped Source"));
        assert!(body.contains("It will appear here after the next scrape"));
        assert!(body.contains("Broken Source"));
        assert!(body.contains("Could not read statistics for: Faulted Source"));
        asserts_no_error_text_leaked(&body);
    }
}
