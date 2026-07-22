pub mod ab;
pub mod device;
pub mod group;
pub mod misc;
pub mod user;

use crate::config::Config;
use crate::db;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use sqlx::SqlitePool;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub config: Config,
}

/// The client expects `{"error": "..."}` with a string value, and the body must
/// be valid JSON even on failure — a non-JSON error body makes the Dart side
/// throw during decode, leaving the user with a bare HTTP status.
pub fn error_json(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

/// Resolves the bearer token to a user, or returns the response to send back.
///
/// 401 here is deliberate and load-bearing: on 401 the client wipes its token
/// and logs the user out, which is exactly right for an expired/unknown token.
/// Never use 401 for mere permission failures.
pub async fn require_user(state: &AppState, headers: &HeaderMap) -> Result<db::User, Response> {
    let token = crate::auth::bearer_token(headers)
        .ok_or_else(|| error_json(StatusCode::UNAUTHORIZED, "Not authenticated"))?;

    match db::user_for_token(&state.pool, &token).await {
        Ok(Some(user)) if user.status != 0 => Ok(user),
        Ok(Some(_)) => Err(error_json(StatusCode::UNAUTHORIZED, "Account is disabled")),
        Ok(None) => Err(error_json(StatusCode::UNAUTHORIZED, "Invalid token")),
        Err(e) => {
            tracing::error!("token lookup failed: {e:#}");
            Err(error_json(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal error",
            ))
        }
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        // --- account ---------------------------------------------------
        .route("/api/login", post(user::login))
        .route("/api/logout", post(user::logout))
        .route("/api/currentUser", post(user::current_user))
        // Also the TLS-detection probe the client hits before anything else,
        // so it must answer properly rather than 404.
        .route(
            "/api/login-options",
            get(user::login_options).post(user::login_options),
        )
        // --- device self-registration (unauthenticated by design) ------
        .route("/api/sysinfo", post(device::sysinfo))
        .route("/api/sysinfo_ver", post(device::sysinfo_ver))
        .route("/api/heartbeat", post(device::heartbeat))
        // --- address book ----------------------------------------------
        // Reads arrive as POST with an empty body from the client, and as GET
        // from RustDesk's own res/ab.py tooling. Both verbs are wired up.
        .route("/api/ab/personal", post(ab::personal).get(ab::personal))
        .route("/api/ab/settings", post(ab::settings).get(ab::settings))
        .route(
            "/api/ab/shared/profiles",
            post(ab::shared_profiles).get(ab::shared_profiles),
        )
        .route("/api/ab/peers", post(ab::peers).get(ab::peers))
        .route("/api/ab/tags/{guid}", post(ab::tags).get(ab::tags))
        .route("/api/ab/peer/add/{guid}", post(ab::peer_add))
        .route("/api/ab/peer/update/{guid}", put(ab::peer_update))
        .route("/api/ab/peer/{guid}", delete(ab::peer_delete))
        .route("/api/ab/tag/add/{guid}", post(ab::tag_put))
        .route("/api/ab/tag/update/{guid}", put(ab::tag_put))
        .route("/api/ab/tag/rename/{guid}", put(ab::tag_rename))
        .route("/api/ab/tag/{guid}", delete(ab::tag_delete))
        // --- fire-and-forget audit sinks -------------------------------
        .route("/api/audit/conn", post(misc::audit_ok))
        .route("/api/audit/file", post(misc::audit_ok))
        .route("/api/audit/alarm", post(misc::audit_ok))
        .route("/api/audit", put(misc::audit_ok))
        // --- "Accessible devices" tab ----------------------------------
        .route("/api/users", get(group::users).post(group::users))
        .route("/api/peers", get(group::peers).post(group::peers))
        .route(
            "/api/device-group/accessible",
            get(group::device_groups).post(group::device_groups),
        )
        // --- operational ------------------------------------------------
        .route("/health", get(health))
        .fallback(misc::not_found)
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}

async fn health(State(state): State<AppState>) -> Response {
    match db::list_devices(&state.pool, state.config.device_stale_days).await {
        Ok(devices) => Json(serde_json::json!({
            "status": "ok",
            "devices": devices.len(),
        }))
        .into_response(),
        Err(e) => {
            tracing::error!("health check failed: {e:#}");
            error_json(StatusCode::INTERNAL_SERVER_ERROR, "database unavailable")
        }
    }
}
