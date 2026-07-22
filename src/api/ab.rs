//! Address book endpoints (RustDesk 1.2.4+ "shared address book" protocol).
//!
//! Two books are exposed to every user:
//!
//!   * their **personal** book — writable, stored per user;
//!   * **"All Workstations"** — a shared book with `rule: 1` (read-only),
//!     generated live from the `devices` table so every machine that has
//!     reported in shows up for everybody automatically.
//!
//! Protocol notes that are easy to get wrong, all confirmed against the client:
//!   * read endpoints arrive as POST with `Content-Length: 0`, so no handler
//!     here may use a body extractor; `res/ab.py` uses GET for the same paths,
//!     hence both verbs are routed.
//!   * paged responses must carry `total` or the client discards `data`.
//!   * `/api/ab/tags/{guid}` returns a *bare array*; every other read returns
//!     an object.
//!   * successful mutations must return 200 with a **zero-length** body — any
//!     body at all is stringified into a user-visible error toast.

use super::{require_user, AppState};
use crate::db::{self, AbPeer};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

/// Stable guid of the auto-populated, read-only shared address book.
pub const SHARED_GUID: &str = "all-workstations";
pub const SHARED_NAME: &str = "All Workstations";

#[derive(Debug, Deserialize)]
pub struct PageQuery {
    #[serde(default)]
    pub current: Option<i64>,
    #[serde(rename = "pageSize", default)]
    pub page_size: Option<i64>,
    /// Which address book to read peers from.
    #[serde(default)]
    pub ab: Option<String>,
}

/// Mutations signal success with 200 and an empty body.
fn ok_empty() -> Response {
    StatusCode::OK.into_response()
}

/// Mutations signal failure with an `{"error": ...}` body, which the client
/// surfaces as a toast. Deliberately not 401 — that would log the user out.
fn action_error(message: &str) -> Response {
    (StatusCode::OK, Json(json!({ "error": message }))).into_response()
}

pub fn paginate<T>(items: Vec<T>, q: &PageQuery) -> (usize, Vec<T>) {
    let total = items.len();
    let page_size = q.page_size.unwrap_or(0);
    let current = q.current.unwrap_or(0);
    if page_size <= 0 || current <= 0 {
        return (total, items);
    }
    let skip = ((current - 1) * page_size).max(0) as usize;
    let page = items.into_iter().skip(skip).take(page_size as usize).collect();
    (total, page)
}

/// Which book a guid refers to.
enum AbRef {
    Personal,
    Shared,
}

async fn resolve_ab(
    state: &AppState,
    guid: &str,
    user_id: i64,
) -> Result<AbRef, Response> {
    if guid == SHARED_GUID {
        return Ok(AbRef::Shared);
    }
    match db::owns_ab(&state.pool, guid, user_id).await {
        Ok(true) => Ok(AbRef::Personal),
        Ok(false) => Err(action_error("Address book not found")),
        Err(e) => {
            tracing::error!("address book lookup failed: {e:#}");
            Err(action_error("Internal error"))
        }
    }
}

/// A peer as the client's `Peer.fromJson` expects it.
///
/// The mixed casing below (`forceAlwaysRelay` next to `device_group_name`) is
/// the real wire format, not a typo, and `forceAlwaysRelay` is compared against
/// the *string* `"true"` on the client — hence the quoted value.
#[derive(Default)]
struct PeerView<'a> {
    id: &'a str,
    username: &'a str,
    hostname: &'a str,
    platform: &'a str,
    alias: &'a str,
    tags: Value,
    note: &'a str,
    hash: &'a str,
    password: &'a str,
}

impl PeerView<'_> {
    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "username": self.username,
            "hostname": self.hostname,
            "platform": self.platform,
            "alias": self.alias,
            "tags": if self.tags.is_null() { json!([]) } else { self.tags.clone() },
            "note": self.note,
            "hash": self.hash,
            "password": self.password,
            "forceAlwaysRelay": "false",
            "rdpPort": "",
            "rdpUsername": "",
            "loginName": "",
            "device_group_name": "",
            "same_server": true,
        })
    }
}

fn stored_peer_json(peer: &AbPeer) -> Value {
    PeerView {
        id: &peer.id,
        username: &peer.username,
        hostname: &peer.hostname,
        platform: &peer.platform,
        alias: &peer.alias,
        tags: serde_json::from_str(&peer.tags).unwrap_or_else(|_| json!([])),
        note: &peer.note,
        hash: &peer.hash,
        password: &peer.password,
    }
    .to_json()
}

/// Devices come straight from the registration table, so they carry no alias,
/// tags, note or saved credentials — those live in a user's personal book.
fn device_peer_json(device: &db::Device) -> Value {
    PeerView {
        id: &device.id,
        username: &device.username,
        hostname: &device.hostname,
        platform: &device.platform,
        ..Default::default()
    }
    .to_json()
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// `POST /api/ab/personal` — returning a guid here is what selects the modern
/// protocol. (A 404 would drop the client into legacy mode instead.)
pub async fn personal(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let user = match require_user(&state, &headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };

    match db::get_or_create_personal_ab(&state.pool, user.id).await {
        Ok(guid) => Json(json!({ "guid": guid })).into_response(),
        Err(e) => {
            tracing::error!("failed to resolve personal address book: {e:#}");
            action_error("Internal error")
        }
    }
}

/// `POST /api/ab/settings` — `0` means no cap on peers per address book.
pub async fn settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_user(&state, &headers).await {
        return response;
    }
    Json(json!({ "max_peer_one_ab": 0 })).into_response()
}

/// `POST /api/ab/shared/profiles` — the personal book must NOT be listed here;
/// the client prepends it itself.
pub async fn shared_profiles(
    State(state): State<AppState>,
    Query(q): Query<PageQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_user(&state, &headers).await {
        return response;
    }

    let profiles = vec![json!({
        "guid": SHARED_GUID,
        "name": SHARED_NAME,
        "owner": "system",
        "note": "Automatically maintained: every workstation registered with this server.",
        // 1 = read-only. 2 = read/write, 3 = full control.
        "rule": 1,
        "info": null,
    })];

    let (total, data) = paginate(profiles, &q);
    Json(json!({ "total": total, "data": data })).into_response()
}

/// `POST /api/ab/peers?ab={guid}` — note `ab` is a query param, not a path
/// segment.
pub async fn peers(
    State(state): State<AppState>,
    Query(q): Query<PageQuery>,
    headers: HeaderMap,
) -> Response {
    let user = match require_user(&state, &headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };

    // An absent `ab` means the personal book.
    let guid = match &q.ab {
        Some(guid) if !guid.is_empty() => guid.clone(),
        _ => match db::get_or_create_personal_ab(&state.pool, user.id).await {
            Ok(guid) => guid,
            Err(e) => {
                tracing::error!("failed to resolve personal address book: {e:#}");
                return action_error("Internal error");
            }
        },
    };

    let items: Vec<Value> = match resolve_ab(&state, &guid, user.id).await {
        Ok(AbRef::Shared) => {
            match db::list_devices(&state.pool, state.config.device_stale_days).await {
                Ok(devices) => devices.iter().map(device_peer_json).collect(),
                Err(e) => {
                    tracing::error!("failed to list devices: {e:#}");
                    return action_error("Internal error");
                }
            }
        }
        Ok(AbRef::Personal) => match db::list_ab_peers(&state.pool, &guid).await {
            Ok(peers) => peers.iter().map(stored_peer_json).collect(),
            Err(e) => {
                tracing::error!("failed to list address book peers: {e:#}");
                return action_error("Internal error");
            }
        },
        Err(response) => return response,
    };

    let (total, data) = paginate(items, &q);
    Json(json!({ "total": total, "data": data })).into_response()
}

/// `POST /api/ab/tags/{guid}` — responds with a bare JSON array.
pub async fn tags(
    State(state): State<AppState>,
    Path(guid): Path<String>,
    headers: HeaderMap,
) -> Response {
    let user = match require_user(&state, &headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };

    match resolve_ab(&state, &guid, user.id).await {
        // The shared book carries no tags; users tag peers in their own book.
        Ok(AbRef::Shared) => Json(json!([])).into_response(),
        Ok(AbRef::Personal) => match db::list_ab_tags(&state.pool, &guid).await {
            Ok(tags) => {
                let data: Vec<Value> = tags
                    .iter()
                    .map(|t| json!({ "name": t.name, "color": t.color }))
                    .collect();
                Json(Value::Array(data)).into_response()
            }
            Err(e) => {
                tracing::error!("failed to list tags: {e:#}");
                action_error("Internal error")
            }
        },
        Err(response) => response,
    }
}

// ---------------------------------------------------------------------------
// Peer mutations
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct PeerPayload {
    pub id: Option<String>,
    pub alias: Option<String>,
    pub tags: Option<Vec<String>>,
    pub hash: Option<String>,
    pub password: Option<String>,
    pub note: Option<String>,
    pub username: Option<String>,
    pub hostname: Option<String>,
    pub platform: Option<String>,
}

/// Guards a write, rejecting the read-only shared book.
async fn require_writable(
    state: &AppState,
    guid: &str,
    user_id: i64,
) -> Result<(), Response> {
    match resolve_ab(state, guid, user_id).await? {
        AbRef::Shared => Err(action_error(
            "\"All Workstations\" is read-only; it is maintained automatically",
        )),
        AbRef::Personal => Ok(()),
    }
}

/// `POST /api/ab/peer/add/{guid}` — one peer per request.
pub async fn peer_add(
    State(state): State<AppState>,
    Path(guid): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<PeerPayload>,
) -> Response {
    let user = match require_user(&state, &headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_writable(&state, &guid, user.id).await {
        return response;
    }

    let Some(id) = payload.id.filter(|s| !s.is_empty()) else {
        return action_error("Peer id is required");
    };

    let peer = AbPeer {
        id,
        alias: payload.alias.unwrap_or_default(),
        tags: serde_json::to_string(&payload.tags.unwrap_or_default())
            .unwrap_or_else(|_| "[]".into()),
        hash: payload.hash.unwrap_or_default(),
        password: payload.password.unwrap_or_default(),
        note: payload.note.unwrap_or_default(),
        username: payload.username.unwrap_or_default(),
        hostname: payload.hostname.unwrap_or_default(),
        platform: payload.platform.unwrap_or_default(),
    };

    match db::put_ab_peer(&state.pool, &guid, &peer).await {
        Ok(()) => ok_empty(),
        Err(e) => {
            tracing::error!("failed to add peer: {e:#}");
            action_error("Internal error")
        }
    }
}

/// `PUT /api/ab/peer/update/{guid}` — a *sparse* update keyed on `id`; only
/// the changed fields are sent. Unknown peers are ignored rather than created,
/// matching the client's assumption that it only updates peers already present.
pub async fn peer_update(
    State(state): State<AppState>,
    Path(guid): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<PeerPayload>,
) -> Response {
    let user = match require_user(&state, &headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_writable(&state, &guid, user.id).await {
        return response;
    }

    let Some(id) = payload.id.filter(|s| !s.is_empty()) else {
        return action_error("Peer id is required");
    };

    let existing = match db::get_ab_peer(&state.pool, &guid, &id).await {
        Ok(Some(peer)) => peer,
        Ok(None) => return ok_empty(),
        Err(e) => {
            tracing::error!("failed to read peer: {e:#}");
            return action_error("Internal error");
        }
    };

    let updated = AbPeer {
        id,
        alias: payload.alias.unwrap_or(existing.alias),
        tags: match payload.tags {
            Some(tags) => serde_json::to_string(&tags).unwrap_or_else(|_| "[]".into()),
            None => existing.tags,
        },
        hash: payload.hash.unwrap_or(existing.hash),
        password: payload.password.unwrap_or(existing.password),
        note: payload.note.unwrap_or(existing.note),
        username: payload.username.unwrap_or(existing.username),
        hostname: payload.hostname.unwrap_or(existing.hostname),
        platform: payload.platform.unwrap_or(existing.platform),
    };

    match db::put_ab_peer(&state.pool, &guid, &updated).await {
        Ok(()) => ok_empty(),
        Err(e) => {
            tracing::error!("failed to update peer: {e:#}");
            action_error("Internal error")
        }
    }
}

/// `DELETE /api/ab/peer/{guid}` — body is a bare array of peer ids.
pub async fn peer_delete(
    State(state): State<AppState>,
    Path(guid): Path<String>,
    headers: HeaderMap,
    Json(ids): Json<Vec<String>>,
) -> Response {
    let user = match require_user(&state, &headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_writable(&state, &guid, user.id).await {
        return response;
    }

    match db::delete_ab_peers(&state.pool, &guid, &ids).await {
        Ok(()) => ok_empty(),
        Err(e) => {
            tracing::error!("failed to delete peers: {e:#}");
            action_error("Internal error")
        }
    }
}

// ---------------------------------------------------------------------------
// Tag mutations
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TagPayload {
    pub name: String,
    /// Flutter ARGB colour as a 32-bit integer (0xAARRGGBB in decimal).
    #[serde(default)]
    pub color: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct TagRenamePayload {
    pub old: String,
    pub new: String,
}

/// `POST /api/ab/tag/add/{guid}` and `PUT /api/ab/tag/update/{guid}` — add
/// creates, update repaints; an upsert serves both.
pub async fn tag_put(
    State(state): State<AppState>,
    Path(guid): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<TagPayload>,
) -> Response {
    let user = match require_user(&state, &headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_writable(&state, &guid, user.id).await {
        return response;
    }
    if payload.name.is_empty() {
        return action_error("Tag name is required");
    }

    // 4288585374 is the client's own default tag colour.
    let color = payload.color.unwrap_or(4288585374);
    match db::put_ab_tag(&state.pool, &guid, &payload.name, color).await {
        Ok(()) => ok_empty(),
        Err(e) => {
            tracing::error!("failed to save tag: {e:#}");
            action_error("Internal error")
        }
    }
}

/// `PUT /api/ab/tag/rename/{guid}`
pub async fn tag_rename(
    State(state): State<AppState>,
    Path(guid): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<TagRenamePayload>,
) -> Response {
    let user = match require_user(&state, &headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_writable(&state, &guid, user.id).await {
        return response;
    }
    if payload.old.is_empty() || payload.new.is_empty() {
        return action_error("Both old and new tag names are required");
    }

    match db::rename_ab_tag(&state.pool, &guid, &payload.old, &payload.new).await {
        Ok(()) => ok_empty(),
        Err(e) => {
            tracing::error!("failed to rename tag: {e:#}");
            action_error("Internal error")
        }
    }
}

/// `DELETE /api/ab/tag/{guid}` — body is a bare array of tag names.
pub async fn tag_delete(
    State(state): State<AppState>,
    Path(guid): Path<String>,
    headers: HeaderMap,
    Json(names): Json<Vec<String>>,
) -> Response {
    let user = match require_user(&state, &headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_writable(&state, &guid, user.id).await {
        return response;
    }

    match db::delete_ab_tags(&state.pool, &guid, &names).await {
        Ok(()) => ok_empty(),
        Err(e) => {
            tracing::error!("failed to delete tags: {e:#}");
            action_error("Internal error")
        }
    }
}
