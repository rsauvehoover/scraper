//! Router assembly, shared state, and the login flow.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
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

    /// Set the Secure flag on the session cookie. Off by default because a
    /// deployment may terminate TLS at a proxy and reach this service over
    /// plain HTTP. Turn on when TLS reaches this service directly.
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
}

pub struct AppState {
    pub registry: SourceRegistry,
    pub stats: StatsCache,
    pub sessions: SessionStore,
    pub limiter: RateLimiter,
    pub credential: String,
    pub config_path: PathBuf,
    pub secure_cookies: bool,
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
    headers: HeaderMap,
    axum::Form(form): axum::Form<LoginForm>,
) -> Response {
    // Behind a reverse proxy the peer address is the proxy, so prefer the
    // forwarded client address for rate limiting.
    let client = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .unwrap_or("unknown")
        .trim()
        .to_string();

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
        epub_permits: Arc::new(Semaphore::new(2)),
    });

    // Warm the statistics cache before accepting traffic, so the first
    // request does not pay the full scan.
    for entry in state.registry.entries() {
        let stat = state.stats.get(entry);
        println!(
            "{}: {} chapters, {} words",
            stat.source_id, stat.total_chapters, stat.total_words
        );
    }

    let listener = tokio::net::TcpListener::bind(&args.bind).await?;
    println!("listening on {}", args.bind);
    axum::serve(listener, router(state)).await?;
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
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn test_state() -> Arc<AppState> {
        Arc::new(AppState::for_test())
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

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/login")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("password=hunter2"))
                    .unwrap(),
            )
            .await
            .unwrap();

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
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/login")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("password=wrong"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(response.headers().get("set-cookie").is_none());
    }

    #[tokio::test]
    async fn secure_flag_follows_the_configuration() {
        let mut state = AppState::for_test();
        state.secure_cookies = true;
        let app = router(Arc::new(state));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/login")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("password=hunter2"))
                    .unwrap(),
            )
            .await
            .unwrap();

        let cookie = response.headers().get("set-cookie").unwrap().to_str().unwrap();
        assert!(cookie.contains("Secure"), "cookie: {}", cookie);
    }
}
