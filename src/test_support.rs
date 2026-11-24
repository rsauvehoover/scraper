//! Helpers shared by unit tests.

use std::path::{Path, PathBuf};

/// Points the process cwd at a scratch directory, restoring it on drop,
/// including on unwind from a panic.
///
/// `SourceDatabase::open` and `open_query_only` resolve `db/` relative to the
/// cwd, so a test that exercises either must redirect it or it would touch
/// the repository's own `db/`. The cwd is process-global, so every test that
/// uses this must also be `#[serial]`.
pub struct CwdGuard {
    original: PathBuf,
}

impl CwdGuard {
    pub fn change_to(dir: &Path) -> Self {
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

use std::sync::{Arc, Mutex};

use crate::mail::{Attachment, Mailer, Recipient, SendError};

/// What a `RecordingMailer` was asked to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sent {
    pub to: String,
    pub filename: String,
    pub bytes: usize,
}

/// A `Mailer` that records instead of sending, and can fail on request.
pub struct RecordingMailer {
    sent: Arc<Mutex<Vec<Sent>>>,
    fail_on: Option<(usize, SendError)>,
    calls: usize,
}

impl RecordingMailer {
    pub fn new() -> (Self, Arc<Mutex<Vec<Sent>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        (RecordingMailer { sent: Arc::clone(&sent), fail_on: None, calls: 0 }, sent)
    }

    /// Fails the `n`th call (0-based) with `error`; every other call records.
    pub fn failing_on(n: usize, error: SendError) -> (Self, Arc<Mutex<Vec<Sent>>>) {
        let (mut m, sent) = Self::new();
        m.fail_on = Some((n, error));
        (m, sent)
    }
}

#[async_trait::async_trait]
impl Mailer for RecordingMailer {
    async fn send(&mut self, to: &Recipient, attachment: &Attachment) -> Result<(), SendError> {
        let call = self.calls;
        self.calls += 1;
        if let Some((n, error)) = self.fail_on {
            if n == call {
                return Err(error);
            }
        }
        self.sent.lock().unwrap().push(Sent {
            to: to.email.clone(),
            filename: attachment.filename.clone(),
            bytes: attachment.bytes.len(),
        });
        Ok(())
    }
}

use axum::http::{header::CONTENT_TYPE, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

/// One canned response for `serve_fixtures`.
pub struct FixtureRoute {
    pub path: &'static str,
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

/// Serve `routes` over HTTP on a loopback port until the test ends, and
/// return the base URL (`http://127.0.0.1:{port}`). `routes` gets the base URL
/// so bodies can link to other fixtures. Unknown paths answer 404.
pub async fn serve_fixtures(routes: impl FnOnce(&str) -> Vec<FixtureRoute>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let routes = Arc::new(routes(&base));

    let router = axum::Router::new().fallback(move |uri: Uri| {
        let routes = Arc::clone(&routes);
        async move { fixture_response(&routes, uri.path()) }
    });
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    base
}

/// Serve one page that answers its first `challenges` requests the way
/// Cloudflare's managed challenge does (403, `cf-mitigated: challenge`, a
/// "Just a moment" body) and every later one with `body` and 200. Returns
/// the base URL and the request counter.
pub async fn serve_challenged(
    challenges: usize,
    body: &'static str,
) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    let router = axum::Router::new().fallback(move || {
        let counter = Arc::clone(&counter);
        async move {
            let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
            if n <= challenges {
                (
                    StatusCode::FORBIDDEN,
                    [(CONTENT_TYPE, "text/html"), (axum::http::HeaderName::from_static("cf-mitigated"), "challenge")],
                    "<html><head><title>Just a moment...</title></head><body></body></html>",
                )
                    .into_response()
            } else {
                (StatusCode::OK, [(CONTENT_TYPE, "text/html")], body).into_response()
            }
        }
    });
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (base, hits)
}

/// A fake `kcc-c2e` written into `dir`: records its arguments in
/// `args.txt` and the input listing in `listing.txt`, then behaves per
/// `mode`: `ok` writes `<name>.epub` holding `fake`; `split` writes
/// `<name> 1.epub` and `<name> 2.epub`; `fail` exits 3 with `boom` on
/// stderr; `silent` writes nothing. Returns the script's path.
pub fn fake_kcc(dir: &std::path::Path, mode: &str) -> String {
    let script = dir.join("kcc-c2e");
    let body = format!(
        r#"#!/bin/sh
printf '%s\n' "$@" > "{args}"
out=""
while [ $# -gt 1 ]; do
  if [ "$1" = "-o" ]; then out="$2"; fi
  shift
done
input="$1"
name=$(basename "$input")
ls -R "$input" > "{listing}"
case "{mode}" in
  ok) printf 'fake' > "$out/$name.epub" ;;
  split) printf 'fakeone' > "$out/$name 1.epub"; printf 'faketwo' > "$out/$name 2.epub" ;;
  fail) echo boom >&2; exit 3 ;;
  silent) ;;
esac
"#,
        args = dir.join("args.txt").display(),
        listing = dir.join("listing.txt").display(),
        mode = mode
    );
    std::fs::write(&script, body).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script.to_string_lossy().into_owned()
}

fn fixture_response(routes: &[FixtureRoute], path: &str) -> Response {
    match routes.iter().find(|r| r.path == path) {
        Some(r) => (
            StatusCode::from_u16(r.status).unwrap(),
            [(CONTENT_TYPE, r.content_type)],
            r.body.clone(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
