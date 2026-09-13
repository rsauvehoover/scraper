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
use crate::webconfig::{changed_keys, prepare_for_write, read_redacted, write_atomic};

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

    let current: Value = match std::fs::read_to_string(&state.config_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
    {
        Some(v) => v,
        None => Value::Object(Default::default()),
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
