//! The configuration editor.
//!
//! `GET` renders the current config with `Mail.Password` removed; `PUT`
//! validates a candidate, splices the stored password back in when the field
//! was left blank, and writes atomically with the file's mode preserved.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use maud::html;
use serde_json::Value;

use crate::web::app::AppState;
use crate::web::views::page;
use crate::webconfig::{changed_keys, prepare_for_write, read_current, read_redacted, write_atomic};

/// The CSRF token bound to this request's session.
///
/// Cookie parsing is deliberately NOT re-implemented here. `session_token` in
/// `crate::web::app` is `pub(crate)` precisely so this module can reuse it: a
/// second, subtly different parse of a security-relevant header is how the two
/// drift apart, and the authentication middleware and the CSRF check must agree
/// on exactly which session a request belongs to.
fn session_csrf(state: &AppState, headers: &HeaderMap) -> Option<String> {
    let token = crate::web::app::session_token(headers)?;
    state.sessions.csrf_for(&token)
}

pub async fn get_config(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let redacted = match read_redacted(&state.config_path) {
        Ok(v) => v,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let pretty = serde_json::to_string_pretty(&redacted).unwrap_or_default();
    let csrf = session_csrf(&state, &headers).unwrap_or_default();
    let password_set = redacted["Mail"]["PasswordSet"] == Value::Bool(true);

    page(
        "Configuration",
        html! {
            p class="note" {
                "The mail password is never sent to this page. Leave the field blank to keep the "
                "current one."
                @if !password_set { " No password is currently set." }
            }
            // The registry and the statistics cache are built once, at
            // startup. A save that adds, removes or renames a source is
            // written to disk immediately and picked up by the next scraper
            // run, but this UI keeps showing the old set until the service is
            // restarted. Saying so here and in the save confirmation, because
            // "Saved" on its own reads as "in effect".
            p class="note" {
                "Changes are written immediately, but this server reads the source list once "
                "at startup. Restart the web service for source changes to appear here."
            }
            // The forms below splice into the textarea, client-side, and
            // change nothing about how a save is performed: no new endpoint,
            // no change to PUT /config, so the CSRF check, `prepare_for_write`
            // and the atomic mode-preserving write stay exactly as reviewed.
            // The operator also sees the literal document that will be
            // written before committing to it.
            //
            // The deeper reason is that they only ever INSERT into the parsed
            // object. `config.json` carries keys the typed structs do not
            // model — `Sources[].Selectors.IgnoredVolumes` among them — and
            // serde drops every one of them on the way through. A form that
            // rebuilt the document from typed fields would delete those keys
            // silently, on a page whose whole job is not to lose settings.
            details class="adder" {
                summary { "Add a source" }
                p class="summary" {
                    "Fills in a source and appends it to the JSON below. Nothing is written "
                    "until you click Save."
                }
                div class="fields" {
                    div class="field" {
                        label for="src-toc" { "Table of contents URL" }
                        input type="url" id="src-toc" placeholder="https://example.com/contents/";
                    }
                    div class="field" {
                        label for="src-id" { "Source id" }
                        input type="text" id="src-id" placeholder="example-serial";
                    }
                    div class="field" {
                        label for="src-name" { "Name" }
                        input type="text" id="src-name" placeholder="Example Serial";
                    }
                    div class="field" {
                        label for="src-author" { "Author" }
                        input type="text" id="src-author";
                    }
                    div class="field" {
                        label for="src-desc" { "Description" }
                        input type="text" id="src-desc";
                    }
                }
                p class="summary" {
                    "A Royal Road fiction URL fills in the selectors and post-processors that "
                    "the built-in Royal Road scraper expects, and suggests an id beginning "
                    "royal-road-, which is what selects that scraper. Everything stays editable."
                }
                div class="fields" {
                    div class="field" {
                        label for="src-volume-wrapper" { "Volume wrapper" }
                        input type="text" id="src-volume-wrapper" value="volume-wrapper";
                    }
                    div class="field" {
                        label for="src-volume-title" { "Volume title" }
                        input type="text" id="src-volume-title" value="h2";
                    }
                    div class="field" {
                        label for="src-chapter-entry" { "Chapter entry" }
                        input type="text" id="src-chapter-entry" value="chapter-entry";
                    }
                    div class="field" {
                        label for="src-chapter-link" { "Chapter link" }
                        input type="text" id="src-chapter-link" value="a";
                    }
                    div class="field" {
                        label for="src-main-content" { "Main content" }
                        input type="text" id="src-main-content" value="main-content";
                    }
                    div class="field" {
                        label for="src-selector-type" { "Selector type" }
                        select id="src-selector-type" {
                            option value="class" selected { "class" }
                            option value="id" { "id" }
                            option value="tag" { "tag" }
                        }
                    }
                    div class="field" {
                        label for="src-post-processors" { "Post-processors (comma separated)" }
                        input type="text" id="src-post-processors" placeholder="strip-links";
                    }
                }
                button type="button" id="add-source" { "Add source below" }
                span id="source-status" class="form-status" {}
            }
            details class="adder" {
                summary { "Add a mail destination" }
                p class="summary" {
                    "Appends a destination to Mail.Destinations in the JSON below. Nothing is "
                    "written until you click Save."
                }
                div class="fields" {
                    div class="field" {
                        label for="dst-name" { "Name" }
                        input type="text" id="dst-name" placeholder="Kindle Upload";
                    }
                    div class="field" {
                        label for="dst-email" { "Email" }
                        input type="email" id="dst-email" placeholder="reader@example.com";
                    }
                }
                div class="checks" {
                    label { input type="checkbox" id="dst-strip-colour"; "StripColour" }
                    label { input type="checkbox" id="dst-full-volumes" checked; "SendFullVolumes" }
                    label {
                        input type="checkbox" id="dst-individual-chapters";
                        "SendIndividualChapters"
                    }
                }
                div class="field" { label { "Sources this destination receives" } }
                div id="dst-sources" class="picks" {}
                p class="summary" {
                    "Selecting none means every source: that is what an empty Sources map means "
                    "to the mailer."
                }
                button type="button" id="add-destination" { "Add destination below" }
                span id="destination-status" class="form-status" {}
            }
            form id="config-form" {
                label for="mail-password" { "New mail password (optional)" }
                input type="password" id="mail-password" autocomplete="new-password"
                      placeholder="leave blank to keep current";

                label for="config-json" { "config.json" }
                textarea id="config-json" rows="34" spellcheck="false" { (pretty) }

                button type="button" id="save" { "Save" }
                span id="status" {}
            }
            script { (maud::PreEscaped(FORMS_SCRIPT)) }
            script { (maud::PreEscaped(save_script(&csrf))) }
        },
    )
    .into_response()
}

/// The two adder forms.
///
/// Everything here is an insert into the object the operator is looking at:
/// parse the textarea, add one entry, pretty-print it back. It never rebuilds
/// the document, never touches a key it did not just add, and never saves —
/// the existing Save button remains the only write.
///
/// Three ways this refuses rather than guessing: unparseable JSON in the
/// textarea, a container that exists but is the wrong type (a `Sources` that
/// is not an array, a `Mail` that is not an object), and a duplicate source id
/// or destination address. Quietly doing nothing on any of those is how an
/// operator ends up saving a config they believe contains a source it does
/// not.
///
/// Source ids reach the destination picker through `createElement` and
/// `createTextNode`, never `innerHTML`: the ids come from whatever is in the
/// textarea, and building markup out of them would make the editor its own
/// injection vector.
const FORMS_SCRIPT: &str = r#"
(function () {
  var area = document.getElementById('config-json');
  var byId = function (id) { return document.getElementById(id); };
  var val = function (id) { return byId(id).value.trim(); };
  var checked = function (id) { return byId(id).checked; };

  function say(id, message, ok) {
    var el = byId(id);
    el.textContent = message;
    el.className = 'form-status ' + (ok ? 'ok' : 'error');
  }

  // Every splice starts from what is on screen, so the forms can only add to
  // the document the operator is about to save.
  function parsedConfig(statusId) {
    var cfg;
    try {
      cfg = JSON.parse(area.value);
    } catch (e) {
      say(statusId, 'Nothing added: the JSON below does not parse (' + e.message + ').', false);
      return null;
    }
    if (!cfg || typeof cfg !== 'object' || Array.isArray(cfg)) {
      say(statusId, 'Nothing added: the JSON below is not an object.', false);
      return null;
    }
    return cfg;
  }

  function render(cfg) { area.value = JSON.stringify(cfg, null, 2); }

  function sourceIds() {
    var ids = [];
    var cfg;
    try {
      cfg = JSON.parse(area.value);
    } catch (e) {
      // An unparsed config offers no ids rather than a stale list.
      return ids;
    }
    var list = cfg && cfg.Sources;
    if (!Array.isArray(list)) return ids;
    for (var i = 0; i < list.length; i++) {
      if (list[i] && typeof list[i].Id === 'string' && list[i].Id) ids.push(list[i].Id);
    }
    return ids;
  }

  function refreshPicker() {
    var box = byId('dst-sources');
    var keep = {};
    var existing = box.querySelectorAll('input');
    for (var i = 0; i < existing.length; i++) {
      if (existing[i].checked) keep[existing[i].value] = true;
    }
    box.textContent = '';
    var ids = sourceIds();
    if (!ids.length) {
      box.textContent = 'No source ids in the JSON below.';
      return;
    }
    for (var j = 0; j < ids.length; j++) {
      var label = document.createElement('label');
      var input = document.createElement('input');
      input.type = 'checkbox';
      input.value = ids[j];
      if (keep[ids[j]]) input.checked = true;
      label.appendChild(input);
      label.appendChild(document.createTextNode(ids[j]));
      box.appendChild(label);
    }
  }

  // Mirrors SourceConfig::royal_road and Selectors::default in src/config.rs.
  // Order matches SELECTOR_FIELDS.
  var SELECTOR_FIELDS =
    ['src-volume-wrapper', 'src-volume-title', 'src-chapter-entry',
     'src-chapter-link', 'src-main-content'];
  var GENERIC = ['volume-wrapper', 'h2', 'chapter-entry', 'a', 'main-content'];
  var ROYAL_ROAD = ['volume-selector', 'h6', 'chapter-row', 'a', 'chapter-content'];
  var ROYAL_ROAD_URL = /^https?:\/\/(?:www\.)?royalroad\.com\/fiction\/(\d+)/i;

  var idEdited = false;
  var selectorsEdited = false;

  function applyPreset() {
    var match = ROYAL_ROAD_URL.exec(val('src-toc'));
    if (!selectorsEdited) {
      var preset = match ? ROYAL_ROAD : GENERIC;
      for (var i = 0; i < SELECTOR_FIELDS.length; i++) {
        byId(SELECTOR_FIELDS[i]).value = preset[i];
      }
      byId('src-selector-type').value = 'class';
      byId('src-post-processors').value = match ? 'strip-links' : '';
    }
    // The royal-road- prefix is not cosmetic: ScraperRegistry selects the
    // built-in Royal Road scraper by it. Suggested, never forced.
    if (match && !idEdited) byId('src-id').value = 'royal-road-' + match[1];
  }

  byId('src-toc').addEventListener('input', applyPreset);
  byId('src-id').addEventListener('input', function () { idEdited = true; });
  var touched = SELECTOR_FIELDS.concat(['src-selector-type', 'src-post-processors']);
  for (var t = 0; t < touched.length; t++) {
    byId(touched[t]).addEventListener('input', function () { selectorsEdited = true; });
    byId(touched[t]).addEventListener('change', function () { selectorsEdited = true; });
  }

  byId('add-source').addEventListener('click', function () {
    var cfg = parsedConfig('source-status');
    if (!cfg) return;

    var id = val('src-id');
    var name = val('src-name');
    var toc = val('src-toc');
    if (!id || !name || !toc) {
      say('source-status', 'Nothing added: id, name and TOC URL are all required.', false);
      return;
    }

    if (cfg.Sources === undefined || cfg.Sources === null) cfg.Sources = [];
    if (!Array.isArray(cfg.Sources)) {
      say('source-status', 'Nothing added: Sources is present but is not an array.', false);
      return;
    }
    for (var i = 0; i < cfg.Sources.length; i++) {
      if (cfg.Sources[i] && cfg.Sources[i].Id === id) {
        say('source-status', 'Nothing added: a source with the id ' + id + ' already exists.',
            false);
        return;
      }
    }

    var processors = val('src-post-processors').split(',').map(function (s) {
      return s.trim();
    }).filter(function (s) { return s.length > 0; });

    cfg.Sources.push({
      Id: id,
      Name: name,
      Enabled: true,
      TocUrl: toc,
      Selectors: {
        VolumeWrapper: val('src-volume-wrapper'),
        VolumeTitle: val('src-volume-title'),
        ChapterEntry: val('src-chapter-entry'),
        ChapterLink: val('src-chapter-link'),
        MainContent: val('src-main-content'),
        SelectorType: byId('src-selector-type').value
      },
      Auth: { Type: 'None' },
      Metadata: { Author: val('src-author'), Description: val('src-desc') },
      PostProcessors: processors
    });

    render(cfg);
    refreshPicker();
    say('source-status', 'Added ' + id + ' to the JSON below. Review it, then click Save.', true);
  });

  byId('add-destination').addEventListener('click', function () {
    var cfg = parsedConfig('destination-status');
    if (!cfg) return;

    var name = val('dst-name');
    var email = val('dst-email');
    if (!name || !email) {
      say('destination-status', 'Nothing added: name and email are both required.', false);
      return;
    }

    if (cfg.Mail === undefined || cfg.Mail === null) cfg.Mail = {};
    if (typeof cfg.Mail !== 'object' || Array.isArray(cfg.Mail)) {
      say('destination-status', 'Nothing added: Mail is present but is not an object.', false);
      return;
    }
    if (cfg.Mail.Destinations === undefined || cfg.Mail.Destinations === null) {
      cfg.Mail.Destinations = [];
    }
    if (!Array.isArray(cfg.Mail.Destinations)) {
      say('destination-status',
          'Nothing added: Mail.Destinations is present but is not an array.', false);
      return;
    }
    for (var i = 0; i < cfg.Mail.Destinations.length; i++) {
      var existing = cfg.Mail.Destinations[i];
      if (existing && typeof existing.Email === 'string' &&
          existing.Email.trim().toLowerCase() === email.toLowerCase()) {
        say('destination-status', 'Nothing added: ' + email + ' is already a destination.', false);
        return;
      }
    }

    // An empty map means every source, which is what UserConfig::receives_source
    // treats it as. Selecting nothing is therefore a real answer, not a
    // missing one.
    var sources = {};
    var picks = byId('dst-sources').querySelectorAll('input');
    for (var j = 0; j < picks.length; j++) {
      if (picks[j].checked) sources[picks[j].value] = {};
    }

    cfg.Mail.Destinations.push({
      Name: name,
      Email: email,
      StripColour: checked('dst-strip-colour'),
      SendFullVolumes: checked('dst-full-volumes'),
      SendIndividualChapters: checked('dst-individual-chapters'),
      Sources: sources
    });

    render(cfg);
    say('destination-status',
        'Added ' + email + ' to the JSON below. Review it, then click Save.', true);
  });

  area.addEventListener('input', refreshPicker);
  applyPreset();
  refreshPicker();
})();
"#;

/// Render the client-side save script, with `csrf` embedded as a JS string
/// literal.
///
/// `csrf` comes from `random_token()`, which is hex today, so it can never
/// itself contain a quote or `</script>`. The encoding below does not lean on
/// that holding forever: `serde_json::to_string` produces a properly quoted
/// and backslash-escaped JS string literal (handling any quote or control
/// character), and the `</` -> `<\/` rewrite additionally guarantees the
/// token cannot close the surrounding `<script>` element early, which
/// JSON/JS string escaping alone would not prevent — a browser's HTML parser
/// looks for a literal `</script` byte sequence regardless of whether it sits
/// inside a JS string.
fn save_script(csrf: &str) -> String {
    let csrf_literal = serde_json::to_string(csrf)
        .expect("string serialisation cannot fail")
        .replace("</", "<\\/");

    format!(
        r#"
const csrf = {csrf_literal};
document.getElementById('save').addEventListener('click', async () => {{
  const status = document.getElementById('status');
  let body;
  try {{
    body = JSON.parse(document.getElementById('config-json').value);
  }} catch (e) {{
    status.textContent = 'Invalid JSON: ' + e.message;
    status.className = 'error';
    return;
  }}
  const pw = document.getElementById('mail-password').value;
  if (pw) {{
    body.Mail = body.Mail || {{}};
    body.Mail.Password = pw;
  }}
  status.textContent = 'Saving...';
  status.className = '';
  const res = await fetch('/config', {{
    method: 'PUT',
    headers: {{ 'Content-Type': 'application/json', 'X-CSRF-Token': csrf }},
    body: JSON.stringify(body)
  }});
  const text = await res.text();
  status.textContent = res.ok ? text : 'Failed: ' + text;
  status.className = res.ok ? 'ok' : 'error';
  if (res.ok) document.getElementById('mail-password').value = '';
}});
"#
    )
}

pub async fn put_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(candidate): Json<Value>,
) -> Response {
    // SameSite=Strict already blocks the cross-site form case; this is the
    // second layer, and the one that does not depend on browser behaviour.
    let expected = session_csrf(&state, &headers);
    let supplied = headers
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    match expected {
        Some(ref token) if !token.is_empty() && token == supplied => {}
        _ => return (StatusCode::FORBIDDEN, "Missing or invalid CSRF token").into_response(),
    }

    // Not `.ok()`. An unreadable or malformed `config.json` used to become an
    // empty object here, which made `merge_password` splice `""` in as the
    // "unchanged" password, pass validation, and get written over the live
    // credential — answered with 200 Saved. The operator leaving the write-only
    // password field blank is the documented normal path, so that turned a
    // transient read failure into silent credential loss.
    let current: Value = match read_current(&state.config_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "refusing to save: cannot read the current config at {}: {}",
                state.config_path.display(),
                e
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Cannot read the current configuration; nothing was written. \
                 See the server log.",
            )
                .into_response();
        }
    };

    // `prepare_for_write` is the only door: it merges the stored password
    // into `candidate` before validating, so a still-blank password never
    // reaches serde's deserialiser (whose errors quote field values).
    let prepared = match prepare_for_write(candidate, &current) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("Invalid configuration: {}", e))
                .into_response()
        }
    };

    if let Err(e) = write_atomic(&state.config_path, &prepared) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    // Key names only. This log must never become a second copy of the password.
    println!(
        "config updated at {:?}; changed keys: {}",
        std::time::SystemTime::now(),
        changed_keys(&current, &prepared).join(", ")
    );

    (
        StatusCode::OK,
        // The save script shows this text verbatim. The restart caveat
        // belongs in the confirmation, not only on the page above it: an
        // operator who adds a source and sees a bare "Saved" reasonably
        // concludes the service is already using it.
        "Saved. Restart the web service for source changes to take effect here.",
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webconfig::read_redacted;
    use serde_json::json;
    use std::path::PathBuf;

    /// Obviously-synthetic fixture. Never use a real credential here.
    fn sample_config() -> Value {
        json!({
            "Mail": {
                "Name": "Epub Mail Sender",
                "Address": "sender@example.com",
                "Password": "abcdefghijklmnop",
                "Destinations": [
                    {"Name": "Test Reader", "Email": "reader@example.com", "Sources": {}}
                ]
            },
            "EpubGen": {"Volumes": true, "Chapters": true, "StripColour": false},
            "Sources": [{
                "Id": "test-source",
                "Name": "Test Serial",
                "Enabled": true,
                "TocUrl": "https://example.com/toc/",
                "Auth": {"Type": "None"},
                "Metadata": {"Author": "A. Writer", "Description": "Test"},
                "PostProcessors": ["strip-links"]
            }]
        })
    }

    /// State pointed at `path`, plus the cookie and CSRF headers of a live
    /// session — `put_config` rejects anything else with 403 before it gets
    /// as far as the behaviour these tests are about.
    fn state_with_session(path: PathBuf) -> (Arc<AppState>, HeaderMap) {
        let mut state = AppState::for_test();
        state.config_path = path;
        let state = Arc::new(state);

        let token = state.sessions.create();
        let csrf = state.sessions.csrf_for(&token).expect("new session has a csrf token");

        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            format!("scraper_session={}", token).parse().unwrap(),
        );
        headers.insert("x-csrf-token", csrf.parse().unwrap());
        (state, headers)
    }

    fn write_config(dir: &std::path::Path, body: &str) -> PathBuf {
        let path = dir.join("config.json");
        std::fs::write(&path, body).unwrap();
        path
    }

    fn stored_password(path: &std::path::Path) -> Value {
        let raw = std::fs::read_to_string(path).unwrap();
        let value: Value = serde_json::from_str(&raw).unwrap();
        value["Mail"]["Password"].clone()
    }

    async fn body_of(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    /// Render the config page against a throwaway `config.json`.
    ///
    /// Never against the process's own working directory: `AppState::for_test`
    /// defaults `config_path` to a relative "config.json", and a test that
    /// renders it would be reading whatever real config the run happens to sit
    /// next to.
    async fn rendered_config_page(dir: &std::path::Path) -> String {
        let path = write_config(dir, &serde_json::to_string_pretty(&sample_config()).unwrap());
        let (state, headers) = state_with_session(path);
        body_of(get_config(State(state), headers).await).await
    }

    #[tokio::test]
    async fn the_page_offers_an_add_source_form() {
        let dir = tempfile::tempdir().unwrap();
        let page = rendered_config_page(dir.path()).await;

        for control in &[
            "id=\"src-id\"",
            "id=\"src-name\"",
            "id=\"src-toc\"",
            "id=\"src-author\"",
            "id=\"src-desc\"",
            "id=\"src-selector-type\"",
            "id=\"add-source\"",
        ] {
            assert!(page.contains(control), "add-source form is missing {}", control);
        }

        // Prefilled and editable in both cases, not hidden: the generic
        // defaults are rendered into the fields, and the Royal Road set is in
        // the script that swaps them in.
        for default in &["volume-wrapper", "chapter-entry", "main-content"] {
            assert!(
                page.contains(&format!("value=\"{}\"", default)),
                "the generic selector default {} must be prefilled and visible",
                default
            );
        }
    }

    /// The Royal Road preset is not decoration: those five selectors are what
    /// `SourceConfig::royal_road` builds, and the `royal-road-` id prefix is
    /// what makes `ScraperRegistry` pick the built-in scraper at all. A preset
    /// that drifts from `src/config.rs` produces a source that parses nothing.
    #[tokio::test]
    async fn the_royal_road_preset_matches_the_built_in_scraper() {
        let dir = tempfile::tempdir().unwrap();
        let page = rendered_config_page(dir.path()).await;

        let reference = crate::config::SourceConfig::royal_road("1", "Example", "A", "B");
        for selector in &[
            reference.selectors.volume_wrapper.as_str(),
            reference.selectors.volume_title.as_str(),
            reference.selectors.chapter_entry.as_str(),
            reference.selectors.main_content.as_str(),
        ] {
            assert!(
                page.contains(&format!("'{}'", selector)),
                "the Royal Road preset must carry {} exactly as SourceConfig::royal_road does",
                selector
            );
        }
        assert!(
            reference.post_processors == vec!["strip-links".to_string()],
            "fixture precondition: royal_road uses strip-links"
        );
        assert!(page.contains("'strip-links'"), "the preset must set the post-processors");
        assert!(
            page.contains("'royal-road-'"),
            "the suggested id must carry the prefix that selects the built-in scraper"
        );
    }

    #[tokio::test]
    async fn the_page_offers_an_add_destination_form() {
        let dir = tempfile::tempdir().unwrap();
        let page = rendered_config_page(dir.path()).await;

        for control in &[
            "id=\"dst-name\"",
            "id=\"dst-email\"",
            "id=\"dst-strip-colour\"",
            "id=\"dst-full-volumes\"",
            "id=\"dst-individual-chapters\"",
            "id=\"dst-sources\"",
            "id=\"add-destination\"",
        ] {
            assert!(page.contains(control), "add-destination form is missing {}", control);
        }
        for key in &["StripColour", "SendFullVolumes", "SendIndividualChapters"] {
            assert!(page.contains(key), "the destination form must emit {}", key);
        }
    }

    /// The forms deliberately have no server side. If one ever grows an
    /// endpoint, the hardened write path stops being the only way config
    /// reaches disk and the operator stops seeing what will be written.
    #[tokio::test]
    async fn the_forms_add_no_second_write_path() {
        let dir = tempfile::tempdir().unwrap();
        let page = rendered_config_page(dir.path()).await;

        assert_eq!(
            page.matches("fetch(").count(),
            1,
            "the Save button must remain the only request the page makes"
        );
        assert!(page.contains("fetch('/config'"), "and it must still go to PUT /config");
        assert!(
            !super::FORMS_SCRIPT.contains("fetch"),
            "the forms must not save; the operator reviews and clicks Save"
        );
    }

    /// The reason the forms splice client-side instead of posting typed
    /// fields. `Selectors` does not model `IgnoredVolumes`, but the live
    /// config has one, and serde drops every key it does not model. The write
    /// path carries the operator's own document through, so an insert-only
    /// editor preserves it; anything that rebuilt the document from the typed
    /// structs would delete it with no warning and a "Saved" in reply.
    #[tokio::test]
    async fn a_key_the_structs_do_not_model_survives_a_save() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = sample_config();
        config["Sources"][0]["Selectors"] = json!({
            "VolumeWrapper": "volume-wrapper",
            "IgnoredVolumes": ["Volume 1", "Volume 2"]
        });
        let path = write_config(dir.path(), &serde_json::to_string_pretty(&config).unwrap());
        let (state, headers) = state_with_session(path.clone());

        // What the browser sends back: the rendered document plus one spliced
        // source, exactly the shape the add-source form produces.
        let mut candidate = read_redacted(&path).unwrap();
        candidate["Sources"].as_array_mut().unwrap().push(json!({
            "Id": "royal-road-1",
            "Name": "Example",
            "Enabled": true,
            "TocUrl": "https://www.royalroad.com/fiction/1/example",
            "Selectors": {
                "VolumeWrapper": "volume-selector",
                "VolumeTitle": "h6",
                "ChapterEntry": "chapter-row",
                "ChapterLink": "a",
                "MainContent": "chapter-content",
                "SelectorType": "class"
            },
            "Auth": {"Type": "None"},
            "Metadata": {"Author": "A. Writer", "Description": "Example"},
            "PostProcessors": ["strip-links"]
        }));

        let response = put_config(State(state), headers, Json(candidate)).await;
        assert_eq!(response.status(), StatusCode::OK);

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            written["Sources"][0]["Selectors"]["IgnoredVolumes"],
            json!(["Volume 1", "Volume 2"]),
            "an unmodelled key must survive a save that adds a source"
        );
        assert_eq!(
            written["Sources"][1]["Id"],
            json!("royal-road-1"),
            "and the spliced source must actually be written"
        );
    }

    /// The documented normal path — operator leaves the write-only password
    /// field blank and clicks Save — at a moment when `config.json` cannot be
    /// parsed. Treating that as an empty current config used to splice `""` in
    /// as the "unchanged" password and answer 200, destroying the live
    /// credential over a now-valid file.
    #[tokio::test]
    async fn refuses_to_save_when_the_current_config_is_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let malformed = "{ this is not json";
        let path = write_config(dir.path(), malformed);
        let (state, headers) = state_with_session(path.clone());

        let mut candidate = sample_config();
        candidate["Mail"]["Password"] = json!("");

        let response = put_config(State(state), headers, Json(candidate)).await;

        assert_eq!(
            response.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "an unreadable current config must not be treated as an empty one"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            malformed,
            "nothing may be written when the current config could not be read"
        );
    }

    /// Same cause, second shape. `Config` and `MailConfig` are both
    /// `#[serde(default)]`, so a body that simply omits `Mail` validates and
    /// would delete the sender, the destinations and the password together.
    #[tokio::test]
    async fn refuses_a_body_with_no_mail_section() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            dir.path(),
            &serde_json::to_string_pretty(&sample_config()).unwrap(),
        );
        let (state, headers) = state_with_session(path.clone());

        let mut candidate = sample_config();
        candidate.as_object_mut().unwrap().remove("Mail");

        let response = put_config(State(state), headers, Json(candidate)).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            stored_password(&path),
            json!("abcdefghijklmnop"),
            "the stored mail settings must survive a body that omits them"
        );
    }

    /// The full editor round trip: what `get_config` renders is what the
    /// browser PUTs back when the operator changes nothing else. That document
    /// carries `PasswordSet` and no `Password`, so both the splice and the
    /// removal of the rendering flag have to work or the credential is lost.
    #[tokio::test]
    async fn round_trip_of_the_rendered_config_keeps_the_password() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            dir.path(),
            &serde_json::to_string_pretty(&sample_config()).unwrap(),
        );
        let (state, headers) = state_with_session(path.clone());

        let rendered = read_redacted(&path).unwrap();
        assert_eq!(rendered["Mail"]["PasswordSet"], json!(true), "fixture precondition");

        let response = put_config(State(state), headers, Json(rendered)).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            stored_password(&path),
            json!("abcdefghijklmnop"),
            "an unmodified round trip must not change the stored password"
        );
        assert!(
            !std::fs::read_to_string(&path).unwrap().contains("PasswordSet"),
            "PasswordSet is a rendering flag and must never reach disk"
        );
    }
}
