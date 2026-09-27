//! Router assembly, shared state, and the login flow.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use maud::html;
use tokio::sync::Semaphore;

use crate::config::MailConfig;
use crate::mail::Mailer;
use crate::web::reload::LiveRegistry;
use crate::stats::cache::StatsCache;
use crate::web::auth::{
    hash_password, load_credential, store_credential, verify_password, RateLimiter, SessionStore,
};
use crate::web::send_jobs::SendJobs;
use crate::web::views::page;

const SESSION_COOKIE: &str = "scraper_session";

/// Web frontend for the scraper.
#[derive(clap::Args, Debug)]
pub struct WebArgs {
    /// Address to bind. Defaults to loopback so an unconfigured run is not
    /// reachable off-box; set explicitly for a deployment that needs to
    /// listen on all interfaces.
    #[arg(long, default_value = "127.0.0.1:8080")]
    pub bind: String,

    /// Path to the admin credential file. Deliberately not in config.json,
    /// which this service can rewrite.
    #[arg(long, default_value = "web-auth.json")]
    pub auth_file: PathBuf,

    /// Path to the scraper configuration this service edits.
    #[arg(long, default_value = "config.json")]
    pub config_file: PathBuf,

    /// Set the Secure flag on the session cookie, which stops the browser
    /// from sending it over plain HTTP.
    ///
    /// Turn this on whenever browsers reach the site over HTTPS — including
    /// when TLS terminates at a reverse proxy and the hop from that proxy to
    /// this service is plaintext. The attribute constrains the browser's leg
    /// of the connection, the only leg the browser can see; where TLS
    /// terminates behind the proxy does not enter into it.
    ///
    /// Off by default because it fails closed on a plain-HTTP deployment:
    /// the browser withholds the cookie, so a correct password appears to
    /// log in and then bounce straight back to the login form.
    #[arg(long)]
    pub secure_cookies: bool,

    /// Prompt for a new admin password, write it, and exit. The password is
    /// read once at server startup and held in memory for the life of the
    /// process, so rotating it with `--set-password` takes effect only after
    /// the service is restarted — a still-working old password after
    /// rotation means "restart pending", not "rotation failed". The prompt
    /// itself does not disable terminal echo, so the password is visible
    /// while typing; that is deliberate, to avoid a terminal-handling
    /// dependency for a command that is run once, interactively, on a
    /// console.
    #[arg(long)]
    pub set_password: bool,

    /// Where to read the client address used for login rate limiting.
    ///
    /// Defaults to the connection's peer address, which a client cannot
    /// forge. Behind a reverse proxy that address is the PROXY for every
    /// request, so all clients collapse into a single bucket and anyone who
    /// can reach the login page can exhaust it and lock the operator out.
    /// A proxied deployment should name the header its proxy sets.
    #[arg(long, value_enum, default_value = "peer")]
    pub client_ip_from: ClientIpSource,

    /// A system crontab file (`/etc/crontab` format, e.g. a file under
    /// `/etc/cron.d`) whose entries run the scraper. When given, the Sources
    /// page shows the schedule and the next run; the file is only read,
    /// and only its timing fields are shown, never the commands.
    #[arg(long, value_name = "PATH")]
    pub schedule_file: Option<PathBuf>,
}

/// Where `login_submit` reads the client address it rate-limits on.
///
/// One setting with three values rather than a flag per header: the sources
/// are mutually exclusive, and a bool per header makes "both set" a state
/// someone has to invent a precedence rule for.
///
/// Every header option is a statement by the operator that their proxy
/// controls that header. Getting that wrong is not a small mistake — a
/// client-settable value here hands an attacker a fresh bucket per request
/// and the limiter stops existing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ClientIpSource {
    /// The TCP peer address. Unforgeable, and right for a service clients
    /// reach directly. Wrong behind a proxy, for the reason above.
    Peer,
    /// The LAST entry of `X-Forwarded-For`.
    ///
    /// Last, not first. The common nginx idiom
    /// (`$proxy_add_x_forwarded_for`) APPENDS, so a client that sends its
    /// own `X-Forwarded-For` keeps that value at the head of the list and
    /// only the final entry was written by the proxy itself. Keying on the
    /// first entry would therefore read attacker-controlled text. The last
    /// entry is also correct for a proxy that overwrites rather than
    /// appends, since then it is the only entry.
    XForwardedFor,
    /// `X-Real-IP`, whole. Carries one value with no list semantics, so a
    /// proxy that sets it necessarily overwrites it.
    XRealIp,
}

pub struct AppState {
    pub registry: LiveRegistry,
    pub stats: StatsCache,
    pub sessions: SessionStore,
    pub limiter: RateLimiter,
    pub credential: String,
    pub config_path: PathBuf,
    pub secure_cookies: bool,
    /// Where the login rate limiter reads the client address. See
    /// `WebArgs::client_ip_from`.
    pub client_ip_from: ClientIpSource,
    /// See `WebArgs::schedule_file`.
    pub schedule_file: Option<PathBuf>,
    /// EPUB generation is CPU-bound and each build holds several megabytes,
    /// so concurrent builds are capped rather than unbounded.
    pub epub_permits: Arc<Semaphore>,
    /// Manual sends, running and recent. See `web::send_jobs`.
    pub sends: Arc<SendJobs>,
    /// Makes the mailer for one send job from the mail settings in use.
    /// Tests substitute a recording one; nothing in the test suite opens a
    /// network connection.
    pub mailer: Arc<dyn Fn(MailConfig) -> Box<dyn Mailer> + Send + Sync>,
}

impl AppState {
    #[cfg(test)]
    pub fn for_test() -> Self {
        use crate::config::{Config, SourceConfig};
        let mut config = Config::default();
        config.sources = vec![SourceConfig {
            id: "test-source".to_string(),
            enabled: true,
            ..SourceConfig::default()
        }];

        AppState {
            registry: LiveRegistry::fixed(crate::db::SourceRegistry::from_config_for_test(&config)),
            stats: StatsCache::new(),
            sessions: SessionStore::new(Duration::from_secs(3600)),
            limiter: RateLimiter::new(10, Duration::from_secs(900)),
            credential: hash_password("hunter2").unwrap(),
            config_path: PathBuf::from("config.json"),
            secure_cookies: false,
            client_ip_from: ClientIpSource::Peer,
            schedule_file: None,
            epub_permits: Arc::new(Semaphore::new(2)),
            sends: Arc::new(SendJobs::new()),
            mailer: Arc::new(|_| {
                let (m, _) = crate::test_support::RecordingMailer::failing_on(0, crate::mail::SendError::Other);
                Box::new(m)
            }),
        }
    }
}

/// Extract the session cookie's value from a request's `Cookie` header.
///
/// `pub(crate)` because Task 12's CSRF check needs to read the same cookie
/// and must not re-implement cookie parsing for a security-relevant header.
pub(crate) fn session_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|c| {
            let mut parts = c.trim().splitn(2, '=');
            Some((parts.next()?, parts.next()?))
        })
        .find(|(k, _)| *k == SESSION_COOKIE)
        .map(|(_, v)| v.to_string())
}

/// Whether `supplied` is this session's CSRF token. Pages that write send it
/// as a header from script or as a `csrf` form field.
pub(crate) fn csrf_matches(state: &AppState, headers: &HeaderMap, supplied: &str) -> bool {
    let Some(token) = session_token(headers) else { return false };
    match state.sessions.csrf_for(&token) {
        Some(expected) => !expected.is_empty() && expected == supplied,
        None => false,
    }
}

/// Rejects every request without a valid session. Applied to the whole
/// router except `/login`, so a new route is protected by default rather
/// than by remembering to protect it.
async fn require_session(
    State(state): State<Arc<AppState>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let token = session_token(request.headers()).unwrap_or_default();
    if state.sessions.validate(&token) {
        next.run(request).await
    } else {
        Redirect::to("/login").into_response()
    }
}

async fn login_form() -> impl IntoResponse {
    page(
        "Sign in",
        html! {
            form method="post" action="/login" class="login" {
                label for="password" { "Password" }
                input type="password" id="password" name="password" autofocus required;
                button type="submit" { "Sign in" }
            }
        },
    )
}

#[derive(serde::Deserialize)]
struct LoginForm {
    password: String,
}

async fn login_submit(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    axum::Form(form): axum::Form<LoginForm>,
) -> Response {
    // Keyed on the IP, not the full socket address — the source port changes
    // per connection, so including it would give every attempt its own bucket
    // and defeat the limiter entirely.
    //
    // A header is read only because the operator named it, and an absent or
    // empty one falls back to the peer address: keying on "" would drop every
    // header-less request into one shared bucket, which is the lockout this
    // setting exists to prevent.
    let client = match state.client_ip_from {
        ClientIpSource::Peer => peer.ip().to_string(),
        ClientIpSource::XForwardedFor => headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            // Last entry: see `ClientIpSource::XForwardedFor`.
            .and_then(|v| v.rsplit(',').next())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| peer.ip().to_string()),
        ClientIpSource::XRealIp => headers
            .get("x-real-ip")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| peer.ip().to_string()),
    };

    // This check runs BEFORE verify_password, and the order is load-bearing:
    // argon2 verification is deliberately expensive, so answering a
    // rate-limited request without it keeps the login endpoint from being
    // usable as a CPU-exhaustion lever. Measured on a deployed instance, a
    // rejected attempt costs ~285ms and a rate-limited one ~16ms. Moving the
    // verification above this check would surrender that with no visible
    // change in behaviour.
    if !state.limiter.check(&client) {
        return (StatusCode::TOO_MANY_REQUESTS, "Too many attempts. Try again later.")
            .into_response();
    }

    if !verify_password(&form.password, &state.credential) {
        // Constant delay on failure, so timing does not distinguish
        // "no such password" from "wrong password".
        tokio::time::sleep(Duration::from_millis(250)).await;
        return (
            StatusCode::UNAUTHORIZED,
            page("Sign in", html! { p class="error" { "Incorrect password." } }),
        )
            .into_response();
    }

    let token = state.sessions.create();
    let secure = if state.secure_cookies { "; Secure" } else { "" };
    let cookie = format!(
        "{SESSION_COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/{secure}"
    );

    (
        StatusCode::SEE_OTHER,
        [(header::LOCATION, "/"), (header::SET_COOKIE, cookie.as_str())],
    )
        .into_response()
}

async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(token) = session_token(&headers) {
        state.sessions.revoke(&token);
    }
    let cookie = format!("{SESSION_COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0");
    (
        StatusCode::SEE_OTHER,
        [(header::LOCATION, "/login"), (header::SET_COOKIE, cookie.as_str())],
    )
        .into_response()
}

pub fn router(state: Arc<AppState>) -> Router {
    // Routes added here are behind the session check automatically.
    let protected = Router::new()
        .route("/", get(crate::web::views::index))
        .route("/stats", get(crate::web::views::stats_page))
        .route(
            "/config",
            get(crate::web::config_edit::get_config).put(crate::web::config_edit::put_config),
        )
        .route("/source/{source_id}", get(crate::web::toc::source_toc))
        .route(
            "/source/{source_id}/chapter/{chapter_id}",
            get(crate::web::toc::chapter_page),
        )
        .route(
            "/source/{source_id}/chapter/{chapter_id}/raw",
            get(crate::web::toc::chapter_raw),
        )
        .route(
            "/source/{source_id}/chapter/{chapter_id}/epub",
            get(crate::web::download::chapter_epub),
        )
        .route(
            "/source/{source_id}/volume/{volume_id}/epub",
            get(crate::web::download::volume_epub),
        )
        .route(
            "/source/{source_id}/send",
            get(crate::web::send::send_form).post(crate::web::send::send_submit),
        )
        .route("/send/{job_id}", get(crate::web::send::job_status))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            require_session,
        ));

    Router::new()
        .route("/login", get(login_form).post(login_submit))
        .route("/logout", get(logout))
        .merge(protected)
        .with_state(state)
}

pub async fn serve(args: WebArgs) -> Result<(), Box<dyn std::error::Error>> {
    if args.set_password {
        return set_password(&args.auth_file);
    }

    let credential = load_credential(&args.auth_file)?;
    // Re-checked at most this often, on a request. The config editor does
    // not wait for it: a save reloads immediately.
    let registry = LiveRegistry::load(args.config_file.clone(), Duration::from_secs(2))?;

    let state = Arc::new(AppState {
        registry,
        stats: StatsCache::new(),
        sessions: SessionStore::new(Duration::from_secs(12 * 3600)),
        limiter: RateLimiter::new(10, Duration::from_secs(900)),
        credential,
        config_path: args.config_file.clone(),
        secure_cookies: args.secure_cookies,
        client_ip_from: args.client_ip_from,
        schedule_file: args.schedule_file.clone(),
        epub_permits: Arc::new(Semaphore::new(2)),
        sends: Arc::new(SendJobs::new()),
        mailer: Arc::new(|config| Box::new(crate::mail::SmtpMailer::new(config))),
    });

    // Warm the statistics cache before accepting traffic, so the first
    // request does not pay the full scan.
    //
    // A source that fails to scan is reported and skipped, never fatal. This
    // runs before `TcpListener::bind`, so a panic here leaves no HTTP surface
    // at all — the operator would see a process that exits on start, with the
    // only clue in the log, for a fault that affects exactly one source.
    let registry = state.registry.snapshot();
    for entry in registry.entries() {
        match state.stats.get(entry) {
            Ok(stat) => println!(
                "{}: {} chapters, {} words",
                stat.source_id, stat.total_chapters, stat.total_words
            ),
            Err(e) => eprintln!(
                "warning: statistics unavailable for {}: {}",
                entry.config.id, e
            ),
        }
    }

    // `entries()` above never includes these — each one was already logged
    // in detail by `SourceRegistry::from_config` — so this is a one-line
    // summary an operator scanning startup output can see at a glance,
    // rather than having to notice their absence from the loop above.
    if !registry.skipped().is_empty() {
        println!(
            "{} configured source(s) not registered at startup; see warnings above",
            registry.skipped().len()
        );
    }

    let listener = tokio::net::TcpListener::bind(&args.bind).await?;
    println!("listening on {}", args.bind);
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}

fn set_password(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;

    print!("New admin password: ");
    std::io::stdout().flush()?;
    let mut plain = String::new();
    std::io::stdin().read_line(&mut plain)?;
    let plain = plain.trim();

    if plain.len() < 12 {
        return Err("password must be at least 12 characters".into());
    }

    store_credential(path, &hash_password(plain)?)?;
    println!("Credential written to {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::{Request, StatusCode};
    use std::net::SocketAddr;
    use tower::ServiceExt;

    fn test_state() -> Arc<AppState> {
        Arc::new(AppState::for_test())
    }

    /// Build a login POST from `peer`, carrying `forwarded_for` as its
    /// `X-Forwarded-For` header. `peer` is TEST-NET-1 (192.0.2.0/24,
    /// RFC 5737) throughout these tests, so the fixture is obviously
    /// synthetic.
    fn login_request(peer: SocketAddr, forwarded_for: &str) -> Request<Body> {
        login_request_with(peer, &[("x-forwarded-for", forwarded_for)])
    }

    /// The same, with an explicit header set — including none at all.
    fn login_request_with(peer: SocketAddr, extra: &[(&str, &str)]) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/login")
            .header("content-type", "application/x-www-form-urlencoded");
        for (name, value) in extra {
            builder = builder.header(*name, *value);
        }
        let mut req = builder.body(Body::from("password=wrong")).unwrap();
        req.extensions_mut().insert(ConnectInfo(peer));
        req
    }

    /// Every route in the application. Keep this list exhaustive: the
    /// auth test below is only as good as this table.
    const ALL_ROUTES: &[(&str, &str)] = &[
        ("GET", "/"),
        ("GET", "/stats"),
        ("GET", "/config"),
        ("PUT", "/config"),
        ("GET", "/source/test-source"),
        ("GET", "/source/test-source/chapter/1"),
        ("GET", "/source/test-source/chapter/1/raw"),
        ("GET", "/source/test-source/chapter/1/epub"),
        ("GET", "/source/test-source/volume/1/epub"),
        ("GET", "/source/test-source/send"),
        ("POST", "/source/test-source/send"),
        ("GET", "/send/0000"),
    ];

    /// State with two destinations and a recording mailer. The first lists
    /// the source; the second does not.
    fn send_state() -> (Arc<AppState>, Arc<std::sync::Mutex<Vec<crate::test_support::Sent>>>) {
        use crate::config::{Config, SourceConfig, UserConfig};
        let mut config = Config::default();
        config.sources = vec![SourceConfig { id: "test-source".into(), name: "Test Serial".into(), enabled: true, ..SourceConfig::default() }];
        config.mail.password = "synthetic-pw-918273".into();
        let mut listed = UserConfig { name: "Test Reader".into(), email: "reader@example.com".into(), strip_colour: true, ..Default::default() };
        listed.sources.insert("test-source".into(), Default::default());
        config.mail.destinations = vec![
            listed,
            UserConfig { name: "Second Reader".into(), email: "second@example.org".into(), ..Default::default() },
        ];
        let (mailer, sent) = crate::test_support::RecordingMailer::new();
        let mailer = std::sync::Mutex::new(Some(mailer));
        let state = AppState {
            registry: LiveRegistry::fixed_with_config(config),
            mailer: Arc::new(move |_| {
                let m = mailer.lock().unwrap().take().expect("one job per test");
                Box::new(m) as Box<dyn crate::mail::Mailer>
            }),
            ..AppState::for_test()
        };
        {
            let registry = state.registry.snapshot();
            let db = registry.get("test-source").unwrap().db();
            let vol = db.add_volume("Volume 1").unwrap();
            db.add_chapter("First Chapter", "https://example.com/c1", vol).unwrap();
            let c = db.get_chapters_by_volume(vol).unwrap()[0].id;
            db.add_chapter_data(c, "<p>one two three</p>").unwrap();
        }
        (Arc::new(state), sent)
    }

    async fn body_text(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn authed(state: &AppState, method: &str, uri: &str, body: String) -> Request<Body> {
        let token = state.sessions.create();
        Request::builder()
            .method(method)
            .uri(uri)
            .header("cookie", format!("scraper_session={}", token))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap()
    }

    /// A POST body as the form would send it, with a valid CSRF token for a
    /// fresh session. Returns the request.
    fn send_post(state: &AppState, fields: &str, csrf: Option<&str>) -> Request<Body> {
        let token = state.sessions.create();
        let real = state.sessions.csrf_for(&token).unwrap();
        let mail = state.registry.snapshot_and_mail().mail;
        let fp = crate::web::send_plan::fingerprint(&mail);
        let csrf = csrf.map(str::to_string).unwrap_or(real);
        Request::builder()
            .method("POST")
            .uri("/source/test-source/send")
            .header("cookie", format!("scraper_session={}", token))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(format!("{}&config={}&csrf={}", fields, fp, csrf)))
            .unwrap()
    }

    fn volume_id(state: &AppState) -> i64 {
        let registry = state.registry.snapshot();
        let db = registry.get("test-source").unwrap().db();
        db.connection().query_row("SELECT id FROM volumes", [], |r| r.get(0)).unwrap()
    }

    #[tokio::test]
    async fn the_contents_page_is_a_selection_form() {
        let (state, _) = send_state();
        let res = router(Arc::clone(&state)).oneshot(authed(&state, "GET", "/source/test-source", String::new())).await.unwrap();
        let text = body_text(res).await;
        assert!(text.contains(r#"action="/source/test-source/send""#), "{}", text);
        assert!(text.contains(r#"name="v""#) && text.contains(r#"name="c""#), "{}", text);
    }

    /// Only what can be sent gets a checkbox: a pending chapter has no
    /// content to build an EPUB from.
    #[tokio::test]
    async fn pending_items_get_no_checkbox() {
        // The `toc_distinguishes_pending_chapters_from_downloaded_ones`
        // fixture shape: one downloaded chapter, one pending, one volume.
        let state = test_state();
        let (downloaded, pending) = {
            let registry = state.registry.snapshot();
            let db = registry.get("test-source").unwrap().db();
            let vol = db.add_volume("Volume 1").unwrap();
            db.add_chapter("Downloaded Chapter", "https://example.com/c1", vol).unwrap();
            db.add_chapter("Pending Chapter", "https://example.com/c2", vol).unwrap();
            let cs = db.get_chapters_by_volume(vol).unwrap();
            let id = |name: &str| cs.iter().find(|c| c.name == name).unwrap().id;
            let (downloaded, pending) = (id("Downloaded Chapter"), id("Pending Chapter"));
            db.add_chapter_data(downloaded, "<p>one</p>").unwrap();
            (downloaded, pending)
        };
        let res = router(Arc::clone(&state)).oneshot(authed(&state, "GET", "/source/test-source", String::new())).await.unwrap();
        let text = body_text(res).await;
        assert!(text.contains(&format!(r#"name="c" value="{}""#, downloaded)), "{}", text);
        assert!(!text.contains(&format!(r#"name="c" value="{}""#, pending)), "{}", text);
    }

    #[tokio::test]
    async fn the_form_offers_every_destination_and_marks_an_unusual_one() {
        let (state, _) = send_state();
        let uri = format!("/source/test-source/send?v={}", volume_id(&state));
        let res = router(Arc::clone(&state)).oneshot(authed(&state, "GET", &uri, String::new())).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let text = body_text(res).await;
        assert!(text.contains("Test Reader") && text.contains("Second Reader"), "{}", text);
        assert_eq!(text.matches("Does not normally receive this series").count(), 1, "{}", text);
        assert!(!text.contains("synthetic-pw-918273"));
        // Each destination's Colour/Stripped radio is pre-checked; its
        // checkbox must not be. maud writes `checked` right after `id`.
        for i in 0..2 {
            assert!(!text.contains(&format!(r#"id="dest-{}" checked"#, i)),
                    "no destination is ticked for you: {}", text);
        }
        assert!(text.contains(r#"value="stripped" checked"#), "Test Reader starts on Stripped: {}", text);
    }

    #[tokio::test]
    async fn a_send_without_the_csrf_token_is_refused_and_starts_nothing() {
        let (state, _) = send_state();
        let req = send_post(&state, &format!("v={}&d=0", volume_id(&state)), Some("wrong"));
        let res = router(Arc::clone(&state)).oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        assert!(state.sends.latest().is_none());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn a_send_redirects_to_its_status_and_runs() {
        // The job's build opens `db/test-source.db` by path; point the cwd
        // at an empty scratch directory so it can never find the
        // repository's own `db/`.
        let dir = tempfile::tempdir().unwrap();
        let _cwd = crate::test_support::CwdGuard::change_to(dir.path());
        let (state, _) = send_state();
        let req = send_post(&state, &format!("v={}&d=0&colour-0=stripped", volume_id(&state)), None);
        let res = router(Arc::clone(&state)).oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::SEE_OTHER);
        let location = res.headers()[header::LOCATION].to_str().unwrap().to_string();
        assert!(location.starts_with("/send/"), "{}", location);

        // The job runs on its own task; the fixture database is in memory,
        // so the build fails, which is enough to show the job ran and ended.
        for _ in 0..100 {
            if state.sends.latest().map_or(false, |j| j.finished) { break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let job = state.sends.latest().unwrap();
        assert!(job.finished);
        assert_eq!(job.emails.len(), 1);
        assert!(job.emails[0].strip_colour);

        let res = router(Arc::clone(&state)).oneshot(authed(&state, "GET", &location, String::new())).await.unwrap();
        let text = body_text(res).await;
        assert!(text.contains("Test Reader"), "{}", text);
        assert!(!text.contains("http-equiv=\"refresh\""), "a finished job stops refreshing: {}", text);
    }

    /// State that reloads from a real `config.json` in the current directory
    /// (`test-source`, one destination), over a real `db/test-source.db`
    /// holding one downloaded chapter in one volume, with a recording mailer.
    /// The caller holds a `CwdGuard` into a scratch directory and is serial.
    fn file_backed_send_state() -> (Arc<AppState>, Arc<std::sync::Mutex<Vec<crate::test_support::Sent>>>) {
        std::fs::create_dir_all("db").unwrap();
        {
            let db = crate::db::SourceDatabase::open("test-source").unwrap();
            let vol = db.add_volume("Volume 1").unwrap();
            db.add_chapter("First Chapter", "https://example.com/c1", vol).unwrap();
            let c = db.get_chapters_by_volume(vol).unwrap()[0].id;
            db.add_chapter_data(c, "<p>one two three</p>").unwrap();
        }
        let config = serde_json::json!({
            "Sources": [{ "Id": "test-source", "Name": "Test Serial", "Enabled": true }],
            "Mail": {
                "Name": "Example Sender", "Address": "sender@example.com",
                "Password": "synthetic-pw-918273",
                "SmtpHostname": "smtp.example.com", "SmtpPort": 587,
                "Destinations": [{ "Name": "Test Reader", "Email": "reader@example.com",
                                   "Sources": { "test-source": {} } }]
            }
        });
        std::fs::write("config.json", serde_json::to_vec(&config).unwrap()).unwrap();
        // Checks only on `reload_now`, so a test controls when a change lands.
        let registry = LiveRegistry::load(PathBuf::from("config.json"), Duration::from_secs(3600)).unwrap();

        let (mailer, sent) = crate::test_support::RecordingMailer::new();
        let mailer = std::sync::Mutex::new(Some(mailer));
        let state = AppState {
            registry,
            mailer: Arc::new(move |_| {
                let m = mailer.lock().unwrap().take().expect("one job per test");
                Box::new(m) as Box<dyn crate::mail::Mailer>
            }),
            ..AppState::for_test()
        };
        (Arc::new(state), sent)
    }

    async fn wait_for_the_job(state: &AppState) -> crate::web::send_jobs::Job {
        for _ in 0..500 {
            if state.sends.latest().map_or(false, |j| j.finished) { break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let job = state.sends.latest().unwrap();
        assert!(job.finished, "the job did not finish in time");
        job
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn a_send_builds_the_download_bytes_and_delivers() {
        let dir = tempfile::tempdir().unwrap();
        let _cwd = crate::test_support::CwdGuard::change_to(dir.path());
        let (state, sent) = file_backed_send_state();
        let v = volume_id(&state);

        let req = send_post(&state, &format!("v={}&d=0&colour-0=stripped", v), None);
        let res = router(Arc::clone(&state)).oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::SEE_OTHER);

        let job = wait_for_the_job(&state).await;
        assert_eq!(job.emails.len(), 1);
        assert_eq!(job.emails[0].state, crate::web::send_jobs::EmailState::Sent, "{:?}", job.emails[0].state);
        let source = state.registry.snapshot().get("test-source").unwrap().config.clone();
        let expected = crate::web::download::build_volume(&source, v, true).unwrap().filename;
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 1, "{:?}", *sent);
        assert_eq!(sent[0].to, "reader@example.com");
        assert_eq!(sent[0].filename, expected);
    }

    /// A bad edit leaves the last good destinations in use, but nothing may
    /// be sent to them until the file loads again, and the form says so
    /// before the reader fills it in.
    #[tokio::test]
    #[serial_test::serial]
    async fn a_send_is_refused_while_the_config_does_not_load() {
        let dir = tempfile::tempdir().unwrap();
        let _cwd = crate::test_support::CwdGuard::change_to(dir.path());
        let (state, sent) = file_backed_send_state();
        let v = volume_id(&state);
        let not_loaded = crate::web::send_plan::Refusal::ConfigNotLoaded.message();

        std::fs::write("config.json", r#"{"Sources": ["#).unwrap();
        assert!(state.registry.reload_now().is_some());

        let uri = format!("/source/test-source/send?v={}", v);
        let res = router(Arc::clone(&state)).oneshot(authed(&state, "GET", &uri, String::new())).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(body_text(res).await.contains(&not_loaded), "the form warns first");

        // The fingerprint `send_post` uses is the last good list's, so only
        // the load error stands in the way.
        let req = send_post(&state, &format!("v={}&d=0", v), None);
        let res = router(Arc::clone(&state)).oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(body_text(res).await.contains(&not_loaded));
        assert!(state.sends.latest().is_none());
        assert!(sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn refusals_re_render_the_form_and_start_nothing() {
        let (state, _) = send_state();
        let v = volume_id(&state);
        for (fields, expect) in [
            (format!("v={}", v), "Tick at least one destination."),
            ("d=0".to_string(), "Nothing to send."),
        ] {
            let res = router(Arc::clone(&state)).oneshot(send_post(&state, &fields, None)).await.unwrap();
            assert_eq!(res.status(), StatusCode::BAD_REQUEST);
            assert!(body_text(res).await.contains(expect));
        }
        let stale = send_post(&state, &format!("v={}&d=0", v), None);
        let (parts, body) = stale.into_parts();
        let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        let replaced = String::from_utf8_lossy(&body).replace("config=", "config=stale");
        let res = router(Arc::clone(&state)).oneshot(Request::from_parts(parts, Body::from(replaced))).await.unwrap();
        let text = body_text(res).await;
        assert!(text.contains("destination list changed"), "{}", text);
        // The indices may now name different people, so none is ticked.
        assert!(!text.contains(r#"id="dest-0" checked"#), "{}", text);
        assert!(state.sends.latest().is_none());
    }

    #[tokio::test]
    async fn a_second_send_is_refused_while_one_runs() {
        let (state, _) = send_state();
        state.sends.try_start("test-source", "Test Serial", &[]).unwrap();
        let res = router(Arc::clone(&state))
            .oneshot(send_post(&state, &format!("v={}&d=0", volume_id(&state)), None)).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(body_text(res).await.contains("A send is in progress"));
    }

    #[tokio::test]
    async fn a_running_job_refreshes_and_the_index_links_to_it() {
        let (state, _) = send_state();
        let id = state.sends.try_start("test-source", "Test Serial", &[]).unwrap();
        let res = router(Arc::clone(&state)).oneshot(authed(&state, "GET", &format!("/send/{}", id), String::new())).await.unwrap();
        assert!(body_text(res).await.contains(r#"http-equiv="refresh""#));
        let res = router(Arc::clone(&state)).oneshot(authed(&state, "GET", "/", String::new())).await.unwrap();
        assert!(body_text(res).await.contains(&format!("/send/{}", id)));
    }

    #[tokio::test]
    async fn every_route_requires_authentication() {
        for (method, path) in ALL_ROUTES {
            let app = router(test_state());
            let response = app
                .oneshot(
                    Request::builder()
                        .method(*method)
                        .uri(*path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            assert!(
                matches!(
                    response.status(),
                    StatusCode::SEE_OTHER | StatusCode::UNAUTHORIZED | StatusCode::FOUND
                ),
                "{method} {path} returned {} without a session — every route must be behind auth",
                response.status()
            );
        }
    }

    #[tokio::test]
    async fn login_page_is_reachable_without_a_session() {
        let app = router(test_state());
        let response = app
            .oneshot(Request::builder().uri("/login").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn correct_password_sets_a_hardened_session_cookie() {
        let state = test_state();
        let app = router(Arc::clone(&state));

        let mut req = Request::builder()
            .method("POST")
            .uri("/login")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from("password=hunter2"))
            .unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 1], 1234))));

        let response = app.oneshot(req).await.unwrap();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let cookie = response
            .headers()
            .get("set-cookie")
            .expect("login must set a cookie")
            .to_str()
            .unwrap();

        assert!(cookie.contains("HttpOnly"), "cookie: {}", cookie);
        assert!(cookie.contains("SameSite=Strict"), "cookie: {}", cookie);
        assert!(cookie.contains("Path=/"), "cookie: {}", cookie);
    }

    #[tokio::test]
    async fn wrong_password_sets_no_cookie() {
        let app = router(test_state());
        let mut req = Request::builder()
            .method("POST")
            .uri("/login")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from("password=wrong"))
            .unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 1], 1234))));

        let response = app.oneshot(req).await.unwrap();

        assert!(response.headers().get("set-cookie").is_none());
    }

    #[tokio::test]
    async fn secure_flag_follows_the_configuration() {
        let mut state = AppState::for_test();
        state.secure_cookies = true;
        let app = router(Arc::new(state));

        let mut req = Request::builder()
            .method("POST")
            .uri("/login")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from("password=hunter2"))
            .unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 1], 1234))));

        let response = app.oneshot(req).await.unwrap();

        let cookie = response.headers().get("set-cookie").unwrap().to_str().unwrap();
        assert!(cookie.contains("Secure"), "cookie: {}", cookie);
    }

    #[tokio::test]
    async fn forwarded_for_cannot_bypass_the_rate_limit_when_untrusted() {
        // `client_ip_from` defaults to `Peer`.
        let state = test_state();
        let app = router(Arc::clone(&state));
        let peer = SocketAddr::from(([192, 0, 2, 1], 1234));

        // Exhaust this peer's bucket, rotating the (untrusted) forwarded
        // header on every request. If the header were consulted, each
        // request would land in its own bucket and none of this would ever
        // block.
        for i in 0..10 {
            let response = app
                .clone()
                .oneshot(login_request(peer, &format!("203.0.113.{i}")))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "attempt {i} should still be within the limit"
            );
        }

        let response = app
            .clone()
            .oneshot(login_request(peer, "203.0.113.99"))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "the same peer must be blocked despite a freshly rotated X-Forwarded-For"
        );
    }

    #[tokio::test]
    async fn forwarded_for_gives_separate_buckets_when_trusted() {
        let mut state = AppState::for_test();
        state.client_ip_from = ClientIpSource::XForwardedFor;
        let state = Arc::new(state);
        let app = router(Arc::clone(&state));
        let peer = SocketAddr::from(([192, 0, 2, 1], 1234));

        // Exhaust the bucket for one forwarded client.
        for i in 0..10 {
            let response = app
                .clone()
                .oneshot(login_request(peer, "203.0.113.1"))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "attempt {i} should still be within the limit"
            );
        }
        let response = app
            .clone()
            .oneshot(login_request(peer, "203.0.113.1"))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "the exhausted forwarded client must now be blocked"
        );

        // A different forwarded client, same peer, must be unaffected.
        let response = app
            .clone()
            .oneshot(login_request(peer, "203.0.113.2"))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "a distinct forwarded client must get its own bucket"
        );
    }

    /// The spoofing case the LAST-entry rule exists for.
    ///
    /// nginx's `$proxy_add_x_forwarded_for` appends, so a client that sends
    /// its own `X-Forwarded-For` keeps that forged value at the HEAD of the
    /// list while the proxy's own observation lands at the tail. Keying on
    /// the head would mint a fresh bucket per request and the limiter would
    /// never fire. This is the exact proxy configuration deployed in front
    /// of this service, so it is not a hypothetical.
    #[tokio::test]
    async fn forwarded_for_ignores_a_forged_leading_entry() {
        let mut state = AppState::for_test();
        state.client_ip_from = ClientIpSource::XForwardedFor;
        let state = Arc::new(state);
        let app = router(Arc::clone(&state));
        let peer = SocketAddr::from(([192, 0, 2, 1], 1234));

        // A different forged head every time; the tail — what the proxy
        // appended — is one client throughout.
        for i in 0..10 {
            let response = app
                .clone()
                .oneshot(login_request(
                    peer,
                    &format!("198.51.100.{i}, 203.0.113.7"),
                ))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "attempt {i} should still be within the limit"
            );
        }

        let response = app
            .clone()
            .oneshot(login_request(peer, "198.51.100.99, 203.0.113.7"))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "a forged leading entry must not mint a fresh bucket"
        );
    }

    #[tokio::test]
    async fn real_ip_gives_separate_buckets() {
        let mut state = AppState::for_test();
        state.client_ip_from = ClientIpSource::XRealIp;
        let state = Arc::new(state);
        let app = router(Arc::clone(&state));
        let peer = SocketAddr::from(([192, 0, 2, 1], 1234));

        for i in 0..10 {
            let response = app
                .clone()
                .oneshot(login_request_with(peer, &[("x-real-ip", "203.0.113.1")]))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "attempt {i} should still be within the limit"
            );
        }
        let response = app
            .clone()
            .oneshot(login_request_with(peer, &[("x-real-ip", "203.0.113.1")]))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "the exhausted client must now be blocked"
        );

        // Same peer (the proxy), different real client: its own bucket.
        let response = app
            .clone()
            .oneshot(login_request_with(peer, &[("x-real-ip", "203.0.113.2")]))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "a distinct real client must get its own bucket"
        );
    }

    /// A header-less request must not key on the empty string. If it did,
    /// every such request would share one bucket and ten of them would lock
    /// out the very clients this setting is meant to separate.
    #[tokio::test]
    async fn a_missing_real_ip_header_falls_back_to_the_peer() {
        let mut state = AppState::for_test();
        state.client_ip_from = ClientIpSource::XRealIp;
        let state = Arc::new(state);
        let app = router(Arc::clone(&state));
        let peer = SocketAddr::from(([192, 0, 2, 1], 1234));
        let other_peer = SocketAddr::from(([192, 0, 2, 2], 1234));

        for i in 0..10 {
            let response = app
                .clone()
                .oneshot(login_request_with(peer, &[]))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "attempt {i} should still be within the limit"
            );
        }
        let response = app
            .clone()
            .oneshot(login_request_with(peer, &[]))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "with no header present the peer address must still bound attempts"
        );

        // Proof the fallback key is the peer and not one shared bucket.
        let response = app
            .clone()
            .oneshot(login_request_with(other_peer, &[]))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "a different peer must not inherit the exhausted bucket"
        );
    }

    /// The header PRESENT but empty, which is the case the emptiness filter
    /// actually covers — an absent header never reaches it. Without the
    /// filter both peers below would key on "" and share one bucket.
    #[tokio::test]
    async fn an_empty_real_ip_header_falls_back_to_the_peer() {
        let mut state = AppState::for_test();
        state.client_ip_from = ClientIpSource::XRealIp;
        let state = Arc::new(state);
        let app = router(Arc::clone(&state));
        let peer = SocketAddr::from(([192, 0, 2, 3], 1234));
        let other_peer = SocketAddr::from(([192, 0, 2, 4], 1234));

        for i in 0..10 {
            let response = app
                .clone()
                .oneshot(login_request_with(peer, &[("x-real-ip", "")]))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "attempt {i} should still be within the limit"
            );
        }
        let response = app
            .clone()
            .oneshot(login_request_with(peer, &[("x-real-ip", "")]))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "an empty header must still bound attempts by peer"
        );

        let response = app
            .clone()
            .oneshot(login_request_with(other_peer, &[("x-real-ip", "")]))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "an empty header must not collapse distinct peers into one bucket"
        );
    }

    #[tokio::test]
    async fn chapter_raw_sets_a_locking_csp_and_strips_script() {
        let state = test_state();

        // Seed a chapter whose stored HTML contains active content, so this test
        // exercises the real path. Without seeding, /raw can only 404 and the
        // header assertions below would never run.
        {
            let registry = state.registry.snapshot();
            let entry = registry.get("test-source").expect("fixture source");
            let db = entry.db();
            let vol = db.add_volume("Volume 1").unwrap();
            db.add_chapter("C1", "https://example.com/c1", vol).unwrap();
            let chapters = db.get_chapters_by_volume(vol).unwrap();
            db.add_chapter_data(
                chapters[0].id,
                "<p>prose</p><script>alert(1)</script><iframe src=\"https://evil.example/\"></iframe>",
            )
            .unwrap();
        }

        let chapter_id = {
            let registry = state.registry.snapshot();
            let entry = registry.get("test-source").unwrap();
            let db = entry.db();
            let v = db.get_latest_volume().unwrap().unwrap();
            db.get_chapters_by_volume(v.id).unwrap()[0].id
        };

        let token = state.sessions.create();
        let app = router(Arc::clone(&state));

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/source/test-source/chapter/{}/raw", chapter_id))
                    .header("cookie", format!("scraper_session={}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK, "seeded chapter must render");

        let csp = response
            .headers()
            .get("content-security-policy")
            .expect("chapter body must carry a CSP")
            .to_str()
            .unwrap()
            .to_string();
        assert!(csp.contains("default-src 'none'"), "csp: {}", csp);
        assert!(csp.contains("img-src data:"), "csp: {}", csp);
        assert!(!csp.contains("script-src"), "csp must not permit script: {}", csp);

        assert_eq!(
            response.headers().get("referrer-policy").unwrap(),
            "no-referrer"
        );

        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(!text.contains("<script"), "script survived into the response");
        assert!(!text.contains("<iframe"), "iframe survived into the response");
        assert!(text.contains("prose"), "prose must survive");
    }

    /// A pending chapter (TOC-listed, not yet downloaded) has `words: 0`,
    /// exactly like a genuinely empty chapter would. The rendered TOC must
    /// not present them identically — otherwise "pending" is indistinguishable
    /// from "the scraper downloaded an empty chapter", which it never does.
    #[tokio::test]
    async fn toc_distinguishes_pending_chapters_from_downloaded_ones() {
        let state = test_state();

        let (downloaded_id, pending_id) = {
            let registry = state.registry.snapshot();
            let entry = registry.get("test-source").expect("fixture source");
            let db = entry.db();
            let vol = db.add_volume("Volume 1").unwrap();
            db.add_chapter("Downloaded Chapter", "https://example.com/c1", vol)
                .unwrap();
            db.add_chapter("Pending Chapter", "https://example.com/c2", vol)
                .unwrap();
            let chapters = db.get_chapters_by_volume(vol).unwrap();
            let downloaded = chapters
                .iter()
                .find(|c| c.name == "Downloaded Chapter")
                .unwrap();
            let pending = chapters
                .iter()
                .find(|c| c.name == "Pending Chapter")
                .unwrap();
            // "Pending Chapter" is deliberately left without a raw_data row.
            db.add_chapter_data(downloaded.id, "<p>one two three</p>")
                .unwrap();
            (downloaded.id, pending.id)
        };

        let token = state.sessions.create();
        let app = router(Arc::clone(&state));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/source/test-source")
                    .header("cookie", format!("scraper_session={}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8_lossy(&body);

        assert!(
            text.contains("pending"),
            "the pending chapter must be marked pending, not 0 words: {}",
            text
        );
        assert!(
            text.contains("3 words"),
            "the downloaded chapter must still show its real word count: {}",
            text
        );
        assert!(
            !text.contains("0 words"),
            "a pending chapter must never render as \"0 words\": {}",
            text
        );

        // The download link would 404 for a chapter with no `raw_data` row
        // (`chapter_epub` rejects it deliberately) — the TOC must not offer
        // a link that cannot work.
        assert!(
            text.contains(&format!("/source/test-source/chapter/{}/epub", downloaded_id)),
            "the downloaded chapter must still offer its EPUB link: {}",
            text
        );
        assert!(
            !text.contains(&format!("/source/test-source/chapter/{}/epub", pending_id)),
            "a pending chapter must not offer an EPUB link that can only 404: {}",
            text
        );
    }

    #[tokio::test]
    async fn config_get_never_returns_the_password() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        std::fs::write(
            &config_path,
            r#"{"Mail":{"Name":"S","Address":"s@example.com","Password":"abcdefghijklmnop",
               "Destinations":[]},"EpubGen":{},"Sources":[]}"#,
        )
        .unwrap();

        let mut state = AppState::for_test();
        state.config_path = config_path;
        let state = Arc::new(state);
        let token = state.sessions.create();
        let app = router(Arc::clone(&state));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/config")
                    .header("cookie", format!("scraper_session={}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8_lossy(&body);

        assert!(
            !text.contains("abcdefghijklmnop"),
            "the password must never reach the client"
        );
    }

    #[tokio::test]
    async fn config_put_requires_the_csrf_token() {
        let state = test_state();
        let token = state.sessions.create();
        let app = router(Arc::clone(&state));

        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/config")
                    .header("cookie", format!("scraper_session={}", token))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "a write without the CSRF header must be refused"
        );
    }

    #[tokio::test]
    async fn config_put_requires_json_content_type() {
        let state = test_state();
        let token = state.sessions.create();
        let csrf = state.sessions.csrf_for(&token).unwrap();
        let app = router(Arc::clone(&state));

        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/config")
                    .header("cookie", format!("scraper_session={}", token))
                    .header("x-csrf-token", csrf)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("Sources=[]"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(
            response.status().is_client_error(),
            "form-encoded writes must be refused; got {}",
            response.status()
        );
    }
}
