//! Manual send: the form, the submit, and the job status page.
//!
//! Destinations are addressed by index into the configuration in use and
//! checked against a fingerprint of it; no address is read from a request.
//! See `send_plan` for the rules and `send_jobs` for the job itself.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path as AxumPath, RawQuery, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use maud::{html, Markup, PreEscaped};

use crate::config::MailConfig;
use crate::web::app::{csrf_matches, session_token, AppState};
use crate::web::send_jobs::{build_item, run_job, EmailState};
use crate::web::send_plan::{self, fingerprint, form_pairs, Refusal, Resolved, Selection, MAX_EMAILS};
use crate::web::views::{page, page_with_refresh};

const SEND_SCRIPT: &str = include_str!("send_select.js");

pub async fn send_form(
    State(state): State<Arc<AppState>>,
    AxumPath(source_id): AxumPath<String>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Response {
    let pairs = form_pairs(query.unwrap_or_default().as_bytes());
    render(&state, &headers, &source_id, &pairs, None)
}

pub async fn send_submit(
    State(state): State<Arc<AppState>>,
    AxumPath(source_id): AxumPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let pairs = form_pairs(&body);
    let supplied = field(&pairs, "csrf");
    if !csrf_matches(&state, &headers, supplied) {
        return (StatusCode::FORBIDDEN, "Missing or invalid CSRF token").into_response();
    }

    let (registry, mail) = state.registry.snapshot_and_mail();
    let Some(entry) = registry.get(&source_id) else {
        return (StatusCode::NOT_FOUND, "Unknown source").into_response();
    };
    if state.registry.load_error().is_some() {
        return render(&state, &headers, &source_id, &pairs, Some(Refusal::ConfigNotLoaded));
    }
    let stat = match state.stats.get(entry) {
        Ok(stat) => stat,
        Err(e) => {
            eprintln!("statistics scan failed for {}: {}", source_id, e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error").into_response();
        }
    };
    let resolved = send_plan::resolve(&stat, &Selection::parse(&pairs));
    let planned = match send_plan::plan(&resolved.items, &mail, field(&pairs, "config"), &send_plan::choices(&pairs)) {
        Ok(p) => p,
        Err(refusal) => return render(&state, &headers, &source_id, &pairs, Some(refusal)),
    };
    let id = match state.sends.try_start(&source_id, &stat.name, &planned) {
        Ok(id) => id,
        Err(refusal) => return render(&state, &headers, &source_id, &pairs, Some(refusal)),
    };

    // Nothing between `try_start` and the spawn may fail or return early: an
    // unspawned job never finishes and blocks every later send until restart.
    let mailer = (state.mailer)(mail);
    tokio::spawn(run_job(
        Arc::clone(&state.sends),
        id.clone(),
        entry.config.clone(),
        planned,
        mailer,
        build_item,
        Arc::clone(&state.epub_permits),
    ));
    (StatusCode::SEE_OTHER, [(header::LOCATION, format!("/send/{}", id))]).into_response()
}

pub async fn job_status(
    State(state): State<Arc<AppState>>,
    AxumPath(job_id): AxumPath<String>,
) -> Response {
    let Some(job) = state.sends.get(&job_id) else {
        return (StatusCode::NOT_FOUND, "Unknown send").into_response();
    };
    let (sent, failed, total) = job.counts();
    page_with_refresh(
        &format!("Send from {}", job.source_name),
        (!job.finished).then_some(3),
        html! {
            p class="summary" {
                (sent) " of " (total) " sent"
                @if failed > 0 { ", " (failed) " failed" }
                @if job.finished { ". Finished." } @else { ". This page refreshes every 3 seconds." }
            }
            table class="jobs" {
                thead { tr { th { "Item" } th { "To" } th { "Version" } th { "State" } } }
                tbody {
                    @for e in &job.emails {
                        tr {
                            td { (e.item) }
                            td { (e.dest_name) }
                            td { @if e.strip_colour { "Stripped" } @else { "Colour" } }
                            (state_cell(&e.state))
                        }
                    }
                }
            }
            p class="summary" { a href={ "/source/" (job.source_id) } { "Back to the contents" } }
        },
    )
    .into_response()
}

fn state_cell(state: &EmailState) -> Markup {
    match state {
        EmailState::Waiting => html! { td class="state-waiting" { "Waiting" } },
        EmailState::Building => html! { td { "Building" } },
        EmailState::Sending => html! { td { "Sending" } },
        EmailState::Sent => html! { td class="state-sent" { "Sent" } },
        EmailState::Failed(reason) => html! { td class="state-failed" { "Failed: " (reason) } },
    }
}

/// For the index: the running send, or the last one, linked.
pub(crate) fn job_note(state: &AppState) -> Markup {
    let Some(job) = state.sends.latest() else { return html! {} };
    let (sent, failed, total) = job.counts();
    html! {
        p class="note" {
            @if job.finished {
                "Last send, " (job.started.format("%H:%M")) ": " (sent) " of " (total) " sent"
                @if failed > 0 { ", " (failed) " failed" }
                ". "
            } @else {
                "A send is running: " (sent) " of " (total) " sent. "
            }
            a href={ "/send/" (job.id) } { "Details" }
        }
    }
}

fn field<'a>(pairs: &'a [(String, String)], key: &str) -> &'a str {
    pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str()).unwrap_or("")
}

/// The form, for a GET or for a refused POST. `pairs` carries the selection
/// either way, and on a refused POST the destination choices too, so nothing
/// the reader ticked is lost.
fn render(
    state: &AppState,
    headers: &HeaderMap,
    source_id: &str,
    pairs: &[(String, String)],
    refusal: Option<Refusal>,
) -> Response {
    let (registry, mail) = state.registry.snapshot_and_mail();
    let Some(entry) = registry.get(source_id) else {
        return (StatusCode::NOT_FOUND, "Unknown source").into_response();
    };
    let stat = match state.stats.get(entry) {
        Ok(stat) => stat,
        Err(e) => {
            eprintln!("statistics scan failed for {}: {}", source_id, e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error").into_response();
        }
    };
    let resolved = send_plan::resolve(&stat, &Selection::parse(pairs));
    let csrf = session_token(headers)
        .and_then(|t| state.sessions.csrf_for(&t))
        .unwrap_or_default();
    let chosen = send_plan::choices(pairs);
    let busy = state.sends.is_running();
    let markup = page(
        &format!("Send from {}", stat.name),
        html! {
            p class="summary" { a href={ "/source/" (source_id) } { "Back to the contents" } }
            @if let Some(r) = refusal { p class="error" { (r.message()) } }
            @else if busy { p class="error" { (Refusal::Busy.message()) } }
            (form_body(source_id, &resolved, &mail, &chosen, &csrf, pairs))
            script { (PreEscaped(SEND_SCRIPT)) }
        },
    );
    let status = if refusal.is_some() { StatusCode::BAD_REQUEST } else { StatusCode::OK };
    (status, markup).into_response()
}

fn form_body(
    source_id: &str,
    resolved: &Resolved,
    mail: &MailConfig,
    chosen: &[send_plan::DestChoice],
    csrf: &str,
    pairs: &[(String, String)],
) -> Markup {
    let selection = Selection::parse(pairs);
    html! {
        @if resolved.items.is_empty() {
            p class="note" { "Nothing is selected. Tick volumes or chapters on the contents page." }
        } @else {
            form id="send-form" method="post" action={ "/source/" (source_id) "/send" } {
                h2 { "What will be sent" }
                ul class="send-list" {
                    @for line in &resolved.items {
                        li { span { (line.item.label()) } span class="detail" { (line.detail) } }
                    }
                }
                @if !resolved.inside_volume.is_empty() {
                    p class="summary" {
                        "Not sent separately, because their volume is already included: "
                        (resolved.inside_volume.join(", ")) "."
                    }
                }
                @if resolved.unavailable > 0 {
                    p class="summary" { (resolved.unavailable) " selected items no longer exist or are not downloaded yet, and are left out." }
                }
                h2 { "To" }
                @if mail.destinations.is_empty() {
                    p class="note" { "No destinations are configured. Add one on the Configuration page." }
                }
                div class="table-wrap" {
                table class="dests" {
                    tbody {
                        @for (i, dest) in mail.destinations.iter().enumerate() {
                            @let choice = chosen.iter().find(|c| c.index == i);
                            @let strip = choice.map_or(dest.source_config(source_id).strip_colour, |c| c.strip_colour);
                            tr {
                                td { input type="checkbox" class="dest" name="d" value=(i) id={ "dest-" (i) } checked[choice.is_some()]; }
                                td {
                                    label for={ "dest-" (i) } { (dest.name) }
                                    div class="dest-email" { (dest.email) }
                                    @if !dest.receives_source(source_id) {
                                        div class="dest-unusual" { "Does not normally receive this series" }
                                    }
                                }
                                td {
                                    label class="variant" { input type="radio" name={ "colour-" (i) } value="colour" checked[!strip]; " Colour" }
                                    label class="variant" { input type="radio" name={ "colour-" (i) } value="stripped" checked[strip]; " Stripped" }
                                }
                            }
                        }
                    }
                }
                }
                @for id in &selection.volumes { input type="hidden" name="v" value=(id); }
                @for id in &selection.chapters { input type="hidden" name="c" value=(id); }
                input type="hidden" name="csrf" value=(csrf);
                input type="hidden" name="config" value=(fingerprint(mail));
                div class="send-actions" {
                    button type="submit" id="send-submit" { "Send" }
                    span id="send-total" data-items=(resolved.items.len()) data-cap=(MAX_EMAILS) {
                        "At most " (MAX_EMAILS) " emails per send."
                    }
                }
            }
        }
    }
}
