//! Device self-registration.
//!
//! These three endpoints are what make the address book fill itself in. They
//! are **unauthenticated** — the RustDesk client sends them with no
//! `Authorization` header, before and independently of any user login — so no
//! auth middleware may sit in front of them.

use super::AppState;
use crate::db;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;

/// Maps the client's `os` string onto the platform names the client's own peer
/// cards have icons for. Anything unrecognised gets a generic icon.
fn platform_from_os(os: &str) -> String {
    let lower = os.to_ascii_lowercase();
    if lower.contains("windows") {
        "Windows"
    } else if lower.contains("android") {
        "Android"
    } else if lower.contains("ios") {
        "iOS"
    } else if lower.contains("mac") || lower.contains("darwin") {
        "Mac OS"
    } else if lower.contains("linux") {
        "Linux"
    } else {
        ""
    }
    .to_string()
}

fn field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// `POST /api/sysinfo` — the full registration payload.
///
/// The response is **plain text, not JSON**, and the exact string matters:
/// `SYSINFO_UPDATED` marks the upload complete, `ID_NOT_FOUND` makes the client
/// retry on its next heartbeat, and anything else triggers a 120-second backoff.
pub async fn sysinfo(State(state): State<AppState>, body: String) -> Response {
    let Ok(info) = serde_json::from_str::<Value>(&body) else {
        tracing::warn!("sysinfo with unparseable body: {body:?}");
        return (StatusCode::OK, "ID_NOT_FOUND").into_response();
    };

    let id = field(&info, "id");
    if id.is_empty() {
        tracing::warn!("sysinfo without an id, ignoring");
        return (StatusCode::OK, "ID_NOT_FOUND").into_response();
    }

    let os = field(&info, "os");
    let platform = platform_from_os(&os);
    let hostname = field(&info, "hostname");

    if let Err(e) = db::upsert_device(
        &state.pool,
        &id,
        &field(&info, "uuid"),
        &hostname,
        &field(&info, "username"),
        &platform,
        &os,
        &field(&info, "version"),
        &field(&info, "cpu"),
        &field(&info, "memory"),
    )
    .await
    {
        tracing::error!("failed to register device {id}: {e:#}");
        // Not SYSINFO_UPDATED, so the client backs off and retries.
        return (StatusCode::OK, "ERROR").into_response();
    }

    tracing::info!("registered device {id} ({hostname}, {platform})");
    (StatusCode::OK, "SYSINFO_UPDATED").into_response()
}

/// `POST /api/sysinfo_ver` — an opaque plain-text version token. Only ever
/// consulted for `*.rustdesk.com` hosts, but cheap to answer correctly.
pub async fn sysinfo_ver() -> Response {
    (StatusCode::OK, "1").into_response()
}

/// `POST /api/heartbeat` — arrives every ~15s per device (every 3s while a
/// session is live), so it stays deliberately cheap: one indexed UPDATE.
///
/// A heartbeat carries no descriptive fields. If it names a device that was
/// never registered — a fresh install, or a database that has been reset — the
/// reply includes a `sysinfo` key, which is the documented way to ask the
/// client for a full upload right away.
pub async fn heartbeat(State(state): State<AppState>, body: String) -> Response {
    let Ok(payload) = serde_json::from_str::<Value>(&body) else {
        return Json(serde_json::json!({})).into_response();
    };

    let id = field(&payload, "id");
    if id.is_empty() {
        return Json(serde_json::json!({})).into_response();
    }

    match db::touch_device(&state.pool, &id).await {
        Ok(true) => Json(serde_json::json!({})).into_response(),
        Ok(false) => {
            tracing::debug!("heartbeat from unknown device {id}, requesting sysinfo");
            Json(serde_json::json!({ "sysinfo": "" })).into_response()
        }
        Err(e) => {
            tracing::error!("heartbeat update failed for {id}: {e:#}");
            Json(serde_json::json!({})).into_response()
        }
    }
}
