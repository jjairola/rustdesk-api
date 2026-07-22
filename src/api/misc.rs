use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;

/// The three `/api/audit/*` sinks plus `PUT /api/audit`. The client fires these
/// and never reads the response, so accepting and discarding is enough. This
/// server keeps no session logs.
pub async fn audit_ok() -> Response {
    StatusCode::OK.into_response()
}

/// Logs anything unexpected the client asks for — useful when a new RustDesk
/// release starts calling an endpoint this server doesn't know about yet.
pub async fn not_found(method: Method, uri: Uri) -> Response {
    tracing::warn!("unhandled request: {method} {uri}");
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "error": "Not found" })),
    )
        .into_response()
}
