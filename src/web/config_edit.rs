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
            form id="config-form" {
                label for="mail-password" { "New mail password (optional)" }
                input type="password" id="mail-password" autocomplete="new-password"
                      placeholder="leave blank to keep current";

                label for="config-json" { "config.json" }
                textarea id="config-json" rows="34" spellcheck="false" { (pretty) }

                button type="button" id="save" { "Save" }
                span id="status" {}
            }
            script { (maud::PreEscaped(save_script(&csrf))) }
        },
    )
    .into_response()
}

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
  status.textContent = res.ok ? 'Saved.' : 'Failed: ' + text;
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

    (StatusCode::OK, "Saved").into_response()
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
