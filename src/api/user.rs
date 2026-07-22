use super::{error_json, require_user, AppState};
use crate::{auth, db};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: Option<String>,
    pub password: Option<String>,
    /// The RustDesk ID of the machine performing the login.
    pub id: Option<String>,
    pub uuid: Option<String>,
    #[serde(rename = "autoLogin")]
    #[allow(dead_code)]
    pub auto_login: Option<bool>,
    #[serde(rename = "type")]
    #[allow(dead_code)]
    pub r#type: Option<String>,
    #[serde(rename = "deviceInfo")]
    #[allow(dead_code)]
    pub device_info: Option<serde_json::Value>,
}

/// Login failures are returned as HTTP 200 with an `{"error": ...}` body.
///
/// The client checks `body['error']` on 200 responses as well as on failures,
/// so this displays the message correctly, and it avoids any chance of a 401
/// on the login path being confused with the session-expiry 401 handling.
fn login_error(message: &str) -> Response {
    (StatusCode::OK, Json(serde_json::json!({ "error": message }))).into_response()
}

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Response {
    let username = req.username.unwrap_or_default();
    let password = req.password.unwrap_or_default();

    if username.is_empty() || password.is_empty() {
        return login_error("Username and password are required");
    }

    let user = match db::find_user_by_name(&state.pool, &username).await {
        Ok(Some(user)) => user,
        Ok(None) => {
            tracing::info!("login rejected for unknown user {username:?}");
            return login_error("Wrong username or password");
        }
        Err(e) => {
            tracing::error!("user lookup failed: {e:#}");
            return error_json(StatusCode::INTERNAL_SERVER_ERROR, "Internal error");
        }
    };

    if !auth::verify_password(&password, &user.password_hash) {
        tracing::info!("login rejected for user {username:?}: bad password");
        return login_error("Wrong username or password");
    }

    if user.status == 0 {
        return login_error("Account is disabled");
    }

    let token = auth::generate_token();
    if let Err(e) = db::create_token(
        &state.pool,
        &token,
        user.id,
        req.id.as_deref().unwrap_or_default(),
        req.uuid.as_deref().unwrap_or_default(),
        state.config.token_ttl,
    )
    .await
    {
        tracing::error!("token creation failed: {e:#}");
        return error_json(StatusCode::INTERNAL_SERVER_ERROR, "Internal error");
    }

    tracing::info!("user {:?} logged in from device {:?}", user.name, req.id.unwrap_or_default());

    // `type` must be exactly "access_token" — the client checks it verbatim.
    Json(serde_json::json!({
        "access_token": token,
        "type": "access_token",
        "user": user.payload(),
    }))
    .into_response()
}

/// Returns a *bare* user payload, not wrapped in a `user` key.
pub async fn current_user(State(state): State<AppState>, headers: HeaderMap) -> Response {
    match require_user(&state, &headers).await {
        Ok(user) => Json(user.payload()).into_response(),
        Err(response) => response,
    }
}

/// The client ignores the body and resets local state regardless of the
/// outcome, and gives up after 2 seconds. Keep it cheap.
pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = auth::bearer_token(&headers) {
        if let Err(e) = db::delete_token(&state.pool, &token).await {
            tracing::warn!("failed to delete token on logout: {e:#}");
        }
    }
    Json(serde_json::json!({})).into_response()
}

/// No OIDC/SSO providers are offered, so the list is empty.
pub async fn login_options() -> Response {
    Json(serde_json::json!([])).into_response()
}
