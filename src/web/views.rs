//! Shared page chrome and placeholder views.
//!
//! Task 13 replaces `index`/`stats_page` with the real dashboard and
//! statistics pages; `page()` is the shared chrome they, and the login
//! form in `app.rs`, render into.

use axum::response::IntoResponse;
use maud::{html, Markup, DOCTYPE};

/// Wrap `body` in the page's shared HTML chrome.
pub fn page(title: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) " · Wandering Inn Scraper" }
            }
            body {
                main {
                    h1 { (title) }
                    (body)
                }
            }
        }
    }
}

/// Placeholder landing page. Replaced by Task 13.
pub async fn index() -> impl IntoResponse {
    page("Dashboard", html! { p { "Nothing here yet." } })
}

/// Placeholder statistics page. Replaced by Task 13.
pub async fn stats_page() -> impl IntoResponse {
    page("Statistics", html! { p { "Nothing here yet." } })
}
