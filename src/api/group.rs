//! The client's "Accessible devices" tab (`Käytettävissä olevat laitteet`).
//!
//! The client builds this tab from three endpoints and joins them **by string
//! name**, not by id: a peer belongs to a device group when its
//! `device_group_name` exactly equals that group's `name`, and to a user when
//! its `user_name` equals that user's `name`.
//!
//! Devices here aren't owned by individual accounts — every account sees every
//! machine — so all workstations go into a single device group sharing the
//! address book's name, and the user list is left empty. Returning accounts
//! would render rows that select down to nothing, since no peer can be
//! attributed to one.
//!
//! Protocol notes confirmed against the client:
//!   * `total` is **required**; without it `data` is never read at all.
//!     `"total": 0` is fine as long as everything fits on page 1 — the client
//!     parses page 1 before evaluating the loop condition.
//!   * `total` must be a JSON *number*; a string throws and fails the fetch.
//!   * an `error` key on a 200 response counts as failure, and a failing
//!     `/api/users` or `/api/peers` aborts the whole pull, blanking the tab.
//!     `/api/device-group/accessible` is the only one whose failure is
//!     tolerated.
//!   * online dots do **not** come from here — the client asks the ID/rendezvous
//!     server directly, matching on `id`. The `status` field is parsed and
//!     discarded.

use super::{require_user, AppState};
use crate::api::ab::{paginate, PageQuery, SHARED_NAME};
use crate::db;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

/// `GET /api/device-group/accessible` — the client reads exactly one field per
/// item, `name`, and treats it as the group's primary key.
pub async fn device_groups(
    State(state): State<AppState>,
    Query(q): Query<PageQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_user(&state, &headers).await {
        return response;
    }

    let groups = vec![json!({ "name": SHARED_NAME })];
    let (total, data) = paginate(groups, &q);
    Json(json!({ "total": total, "data": data })).into_response()
}

/// `GET /api/users?accessible&status=1` — intentionally empty; see module docs.
///
/// This must still succeed: a failure here aborts the client's whole pull and
/// blanks the tab, device groups included.
pub async fn users(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_user(&state, &headers).await {
        return response;
    }
    Json(json!({ "total": 0, "data": [] })).into_response()
}

/// `GET /api/peers?accessible&status=1` — every registered workstation.
pub async fn peers(
    State(state): State<AppState>,
    Query(q): Query<PageQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_user(&state, &headers).await {
        return response;
    }

    let devices = match db::list_devices(&state.pool, state.config.device_stale_days).await {
        Ok(devices) => devices,
        Err(e) => {
            tracing::error!("failed to list devices: {e:#}");
            // An `error` key would abort the client's pull, so degrade to an
            // empty-but-valid page instead.
            return Json(json!({ "total": 0, "data": [] })).into_response();
        }
    };

    let items: Vec<Value> = devices.iter().map(device_json).collect();
    let (total, data) = paginate(items, &q);
    Json(json!({ "total": total, "data": data })).into_response()
}

/// The client picks a platform icon by splitting `info.os` on `" / "`,
/// lowercasing element 0 and matching `windows`/`linux`/`macos`/`android`.
///
/// The raw string a client reports doesn't reliably start with one of those —
/// Debian reports `"debian / Linux 13 …"`, which matches nothing and yields a
/// generic icon. Prefixing the platform already derived at registration makes
/// element 0 always match, while keeping the detailed string after it.
fn os_field(device: &db::Device) -> String {
    let token = match device.platform.as_str() {
        "Windows" => "windows",
        "Linux" => "linux",
        "Mac OS" => "macos",
        "Android" => "android",
        _ => return device.os.clone(),
    };
    if device.os.is_empty() {
        token.to_string()
    } else {
        format!("{token} / {}", device.os)
    }
}

fn device_json(device: &db::Device) -> Value {
    json!({
        "id": device.id,
        // Parsed by the client but never read; a user guid on the Pro server.
        "user": "",
        // Would nest this peer under a user of the same name. No ownership
        // model here, so it stays empty and the group link does the work.
        "user_name": "",
        "device_group_name": SHARED_NAME,
        "note": "",
        // Parsed and discarded by the client — online state comes from hbbs.
        "status": 1,
        "info": {
            // `device_name`, NOT `hostname` — this sub-object uses different
            // keys from the address book's peer shape.
            "device_name": device.hostname,
            "os": os_field(device),
            "username": device.username,
        },
    })
}
