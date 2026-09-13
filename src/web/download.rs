//! On-demand EPUB downloads. Filled in by a later task.

use axum::http::StatusCode;

pub async fn chapter_epub() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}

pub async fn volume_epub() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}
