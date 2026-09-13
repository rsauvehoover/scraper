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

use crate::config::load_config_from;
use crate::db::SourceRegistry;
use crate::stats::cache::StatsCache;
use crate::web::auth::{
    hash_password, load_credential, store_credential, verify_password, RateLimiter, SessionStore,
};
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
    pub registry: SourceRegistry,
    pub stats: StatsCache,
    pub sessions: SessionStore,
    pub limiter: RateLimiter,
    pub credential: String,
    pub config_path: PathBuf,
    pub secure_cookies: bool,
    /// Where the login rate limiter reads the client address. See
    /// `WebArgs::client_ip_from`.
    pub client_ip_from: ClientIpSource,
    /// EPUB generation is CPU-bound and each build holds several megabytes,
    /// so concurrent builds are capped rather than unbounded.
    pub epub_permits: Arc<Semaphore>,
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
            registry: SourceRegistry::from_config_for_test(&config),
            stats: StatsCache::new(),
            sessions: SessionStore::new(Duration::from_secs(3600)),
            limiter: RateLimiter::new(10, Duration::from_secs(900)),
            credential: hash_password("hunter2").unwrap(),
            config_path: PathBuf::from("config.json"),
            secure_cookies: false,
            client_ip_from: ClientIpSource::Peer,
            epub_permits: Arc::new(Semaphore::new(2)),
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
    let config = load_config_from(&args.config_file);
    let registry = SourceRegistry::from_config(&config);

    let state = Arc::new(AppState {
        registry,
        stats: StatsCache::new(),
        sessions: SessionStore::new(Duration::from_secs(12 * 3600)),
        limiter: RateLimiter::new(10, Duration::from_secs(900)),
        credential,
        config_path: args.config_file.clone(),
        secure_cookies: args.secure_cookies,
        client_ip_from: args.client_ip_from,
        epub_permits: Arc::new(Semaphore::new(2)),
    });

    // Warm the statistics cache before accepting traffic, so the first
    // request does not pay the full scan.
    //
    // A source that fails to scan is reported and skipped, never fatal. This
    // runs before `TcpListener::bind`, so a panic here leaves no HTTP surface
    // at all — the operator would see a process that exits on start, with the
    // only clue in the log, for a fault that affects exactly one source.
    for entry in state.registry.entries() {
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
    if !state.registry.skipped().is_empty() {
        println!(
            "{} configured source(s) not registered at startup; see warnings above",
            state.registry.skipped().len()
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
    ];

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
            let entry = state.registry.get("test-source").expect("fixture source");
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
            let entry = state.registry.get("test-source").unwrap();
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
            let entry = state.registry.get("test-source").expect("fixture source");
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
