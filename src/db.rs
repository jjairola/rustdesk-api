use anyhow::{Context, Result};
use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{FromRow, SqlitePool};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub async fn connect(database_url: &str) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::from_str(database_url)
        .with_context(|| format!("invalid database url: {database_url}"))?
        .create_if_missing(true)
        .foreign_keys(true)
        // WAL keeps the heartbeat write load from blocking address-book reads.
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(5));

    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await
        .context("failed to open database")?;

    migrate(&pool).await?;
    Ok(pool)
}

async fn migrate(pool: &SqlitePool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS users (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            name          TEXT NOT NULL UNIQUE COLLATE NOCASE,
            password_hash TEXT NOT NULL,
            email         TEXT NOT NULL DEFAULT '',
            note          TEXT NOT NULL DEFAULT '',
            is_admin      INTEGER NOT NULL DEFAULT 0,
            status        INTEGER NOT NULL DEFAULT 1,
            created_at    INTEGER NOT NULL
        );
        "#,
    )
    .execute(pool)
    .await
    .context("creating users table")?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS tokens (
            token       TEXT PRIMARY KEY,
            user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            device_id   TEXT NOT NULL DEFAULT '',
            device_uuid TEXT NOT NULL DEFAULT '',
            created_at  INTEGER NOT NULL,
            expires_at  INTEGER NOT NULL
        );
        "#,
    )
    .execute(pool)
    .await
    .context("creating tokens table")?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_tokens_user ON tokens(user_id);")
        .execute(pool)
        .await?;

    // One row per workstation that has ever reported in. `id` is the RustDesk
    // ID the client dials, so it is the natural primary key.
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS devices (
            id         TEXT PRIMARY KEY,
            uuid       TEXT NOT NULL DEFAULT '',
            hostname   TEXT NOT NULL DEFAULT '',
            username   TEXT NOT NULL DEFAULT '',
            platform   TEXT NOT NULL DEFAULT '',
            os         TEXT NOT NULL DEFAULT '',
            version    TEXT NOT NULL DEFAULT '',
            cpu        TEXT NOT NULL DEFAULT '',
            memory     TEXT NOT NULL DEFAULT '',
            first_seen INTEGER NOT NULL,
            last_seen  INTEGER NOT NULL
        );
        "#,
    )
    .execute(pool)
    .await
    .context("creating devices table")?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_devices_last_seen ON devices(last_seen);")
        .execute(pool)
        .await?;

    // One personal address book per user. The shared "All Workstations" book is
    // virtual — generated from `devices` on read — so it has no row here.
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS address_books (
            guid       TEXT PRIMARY KEY,
            owner_id   INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            name       TEXT NOT NULL,
            created_at INTEGER NOT NULL
        );
        "#,
    )
    .execute(pool)
    .await
    .context("creating address_books table")?;

    sqlx::query("CREATE UNIQUE INDEX IF NOT EXISTS idx_ab_owner ON address_books(owner_id);")
        .execute(pool)
        .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS ab_peers (
            guid     TEXT NOT NULL REFERENCES address_books(guid) ON DELETE CASCADE,
            id       TEXT NOT NULL,
            alias    TEXT NOT NULL DEFAULT '',
            tags     TEXT NOT NULL DEFAULT '[]',
            hash     TEXT NOT NULL DEFAULT '',
            password TEXT NOT NULL DEFAULT '',
            note     TEXT NOT NULL DEFAULT '',
            username TEXT NOT NULL DEFAULT '',
            hostname TEXT NOT NULL DEFAULT '',
            platform TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (guid, id)
        );
        "#,
    )
    .execute(pool)
    .await
    .context("creating ab_peers table")?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS ab_tags (
            guid  TEXT NOT NULL REFERENCES address_books(guid) ON DELETE CASCADE,
            name  TEXT NOT NULL,
            color INTEGER NOT NULL DEFAULT 4288585374,
            PRIMARY KEY (guid, name)
        );
        "#,
    )
    .execute(pool)
    .await
    .context("creating ab_tags table")?;

    Ok(())
}

/// Random 128-bit identifier, hex encoded. Used for address book guids.
pub fn new_guid() -> String {
    use rand_core::{OsRng, RngCore};
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Clone, FromRow)]
pub struct User {
    pub id: i64,
    pub name: String,
    pub password_hash: String,
    pub email: String,
    pub note: String,
    pub is_admin: i64,
    pub status: i64,
    #[allow(dead_code)]
    pub created_at: i64,
}

impl User {
    /// The `UserPayload` shape the RustDesk client parses. `info` must be
    /// present (the Rust-side deserializer has no default for it) and
    /// `is_admin` must be a real JSON boolean.
    pub fn payload(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "email": self.email,
            "note": self.note,
            "status": self.status,
            "is_admin": self.is_admin != 0,
            "info": {},
        })
    }
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Device {
    pub id: String,
    pub uuid: String,
    pub hostname: String,
    pub username: String,
    pub platform: String,
    pub os: String,
    pub version: String,
    pub cpu: String,
    pub memory: String,
    pub first_seen: i64,
    pub last_seen: i64,
}

pub async fn find_user_by_name(pool: &SqlitePool, name: &str) -> Result<Option<User>> {
    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE name = ?")
        .bind(name)
        .fetch_optional(pool)
        .await?;
    Ok(user)
}

pub async fn create_user(
    pool: &SqlitePool,
    name: &str,
    password_hash: &str,
    email: &str,
    is_admin: bool,
) -> Result<i64> {
    let result = sqlx::query(
        "INSERT INTO users (name, password_hash, email, is_admin, status, created_at)
         VALUES (?, ?, ?, ?, 1, ?)",
    )
    .bind(name)
    .bind(password_hash)
    .bind(email)
    .bind(if is_admin { 1 } else { 0 })
    .bind(now())
    .execute(pool)
    .await?;
    Ok(result.last_insert_rowid())
}

pub async fn count_users(pool: &SqlitePool) -> Result<i64> {
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(pool)
        .await?;
    Ok(count)
}

pub async fn list_users(pool: &SqlitePool) -> Result<Vec<User>> {
    Ok(sqlx::query_as::<_, User>("SELECT * FROM users ORDER BY name")
        .fetch_all(pool)
        .await?)
}

pub async fn set_password(pool: &SqlitePool, name: &str, password_hash: &str) -> Result<u64> {
    let result = sqlx::query("UPDATE users SET password_hash = ? WHERE name = ?")
        .bind(password_hash)
        .bind(name)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

pub async fn delete_user(pool: &SqlitePool, name: &str) -> Result<u64> {
    let result = sqlx::query("DELETE FROM users WHERE name = ?")
        .bind(name)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

pub async fn create_token(
    pool: &SqlitePool,
    token: &str,
    user_id: i64,
    device_id: &str,
    device_uuid: &str,
    ttl: std::time::Duration,
) -> Result<()> {
    let issued = now();
    sqlx::query(
        "INSERT INTO tokens (token, user_id, device_id, device_uuid, created_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(token)
    .bind(user_id)
    .bind(device_id)
    .bind(device_uuid)
    .bind(issued)
    .bind(issued + ttl.as_secs() as i64)
    .execute(pool)
    .await?;
    Ok(())
}

/// Resolves a bearer token to its user, rejecting expired tokens.
pub async fn user_for_token(pool: &SqlitePool, token: &str) -> Result<Option<User>> {
    let user = sqlx::query_as::<_, User>(
        "SELECT users.* FROM users
         JOIN tokens ON tokens.user_id = users.id
         WHERE tokens.token = ? AND tokens.expires_at > ?",
    )
    .bind(token)
    .bind(now())
    .fetch_optional(pool)
    .await?;
    Ok(user)
}

pub async fn delete_token(pool: &SqlitePool, token: &str) -> Result<()> {
    sqlx::query("DELETE FROM tokens WHERE token = ?")
        .bind(token)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn purge_expired_tokens(pool: &SqlitePool) -> Result<u64> {
    let result = sqlx::query("DELETE FROM tokens WHERE expires_at <= ?")
        .bind(now())
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// Full registration from a `/api/sysinfo` upload. Overwrites the descriptive
/// fields but preserves `first_seen`.
#[allow(clippy::too_many_arguments)]
pub async fn upsert_device(
    pool: &SqlitePool,
    id: &str,
    uuid: &str,
    hostname: &str,
    username: &str,
    platform: &str,
    os: &str,
    version: &str,
    cpu: &str,
    memory: &str,
) -> Result<()> {
    let ts = now();
    sqlx::query(
        r#"
        INSERT INTO devices (id, uuid, hostname, username, platform, os, version, cpu, memory, first_seen, last_seen)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(id) DO UPDATE SET
            uuid      = excluded.uuid,
            hostname  = excluded.hostname,
            username  = excluded.username,
            platform  = excluded.platform,
            os        = excluded.os,
            version   = excluded.version,
            cpu       = excluded.cpu,
            memory    = excluded.memory,
            last_seen = excluded.last_seen
        "#,
    )
    .bind(id)
    .bind(uuid)
    .bind(hostname)
    .bind(username)
    .bind(platform)
    .bind(os)
    .bind(version)
    .bind(cpu)
    .bind(memory)
    .bind(ts)
    .bind(ts)
    .execute(pool)
    .await?;
    Ok(())
}

/// Heartbeats carry no descriptive fields, so they only bump `last_seen`.
/// Returns true if the device is already known.
pub async fn touch_device(pool: &SqlitePool, id: &str) -> Result<bool> {
    let result = sqlx::query("UPDATE devices SET last_seen = ? WHERE id = ?")
        .bind(now())
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn list_devices(pool: &SqlitePool, stale_days: i64) -> Result<Vec<Device>> {
    let devices = if stale_days > 0 {
        let cutoff = now() - stale_days * 86_400;
        sqlx::query_as::<_, Device>(
            "SELECT * FROM devices WHERE last_seen >= ? ORDER BY hostname, id",
        )
        .bind(cutoff)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as::<_, Device>("SELECT * FROM devices ORDER BY hostname, id")
            .fetch_all(pool)
            .await?
    };
    Ok(devices)
}

pub async fn delete_device(pool: &SqlitePool, id: &str) -> Result<u64> {
    let result = sqlx::query("DELETE FROM devices WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

// ---------------------------------------------------------------------------
// Address books
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRow)]
pub struct AbPeer {
    pub id: String,
    pub alias: String,
    /// JSON array of tag names, as stored.
    pub tags: String,
    pub hash: String,
    pub password: String,
    pub note: String,
    pub username: String,
    pub hostname: String,
    pub platform: String,
}

#[derive(Debug, Clone, FromRow)]
pub struct AbTag {
    pub name: String,
    pub color: i64,
}

/// Returns the caller's personal address book guid, creating it on first use.
pub async fn get_or_create_personal_ab(pool: &SqlitePool, user_id: i64) -> Result<String> {
    if let Some((guid,)) =
        sqlx::query_as::<_, (String,)>("SELECT guid FROM address_books WHERE owner_id = ?")
            .bind(user_id)
            .fetch_optional(pool)
            .await?
    {
        return Ok(guid);
    }

    let guid = new_guid();
    // A concurrent request may have created it first; the unique index on
    // owner_id makes that a no-op and we re-read the winner below.
    sqlx::query(
        "INSERT OR IGNORE INTO address_books (guid, owner_id, name, created_at)
         VALUES (?, ?, 'My address book', ?)",
    )
    .bind(&guid)
    .bind(user_id)
    .bind(now())
    .execute(pool)
    .await?;

    let (guid,) = sqlx::query_as::<_, (String,)>("SELECT guid FROM address_books WHERE owner_id = ?")
        .bind(user_id)
        .fetch_one(pool)
        .await?;
    Ok(guid)
}

/// True if this guid is a personal address book belonging to `user_id`.
pub async fn owns_ab(pool: &SqlitePool, guid: &str, user_id: i64) -> Result<bool> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT 1 FROM address_books WHERE guid = ? AND owner_id = ?")
            .bind(guid)
            .bind(user_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.is_some())
}

pub async fn list_ab_peers(pool: &SqlitePool, guid: &str) -> Result<Vec<AbPeer>> {
    Ok(sqlx::query_as::<_, AbPeer>(
        "SELECT id, alias, tags, hash, password, note, username, hostname, platform
         FROM ab_peers WHERE guid = ? ORDER BY id",
    )
    .bind(guid)
    .fetch_all(pool)
    .await?)
}

pub async fn get_ab_peer(pool: &SqlitePool, guid: &str, id: &str) -> Result<Option<AbPeer>> {
    Ok(sqlx::query_as::<_, AbPeer>(
        "SELECT id, alias, tags, hash, password, note, username, hostname, platform
         FROM ab_peers WHERE guid = ? AND id = ?",
    )
    .bind(guid)
    .bind(id)
    .fetch_optional(pool)
    .await?)
}

/// Insert-or-replace. The client adds peers one per request.
pub async fn put_ab_peer(pool: &SqlitePool, guid: &str, peer: &AbPeer) -> Result<()> {
    sqlx::query(
        "INSERT INTO ab_peers (guid, id, alias, tags, hash, password, note, username, hostname, platform)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(guid, id) DO UPDATE SET
            alias    = excluded.alias,
            tags     = excluded.tags,
            hash     = excluded.hash,
            password = excluded.password,
            note     = excluded.note,
            username = excluded.username,
            hostname = excluded.hostname,
            platform = excluded.platform",
    )
    .bind(guid)
    .bind(&peer.id)
    .bind(&peer.alias)
    .bind(&peer.tags)
    .bind(&peer.hash)
    .bind(&peer.password)
    .bind(&peer.note)
    .bind(&peer.username)
    .bind(&peer.hostname)
    .bind(&peer.platform)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn delete_ab_peers(pool: &SqlitePool, guid: &str, ids: &[String]) -> Result<()> {
    for id in ids {
        sqlx::query("DELETE FROM ab_peers WHERE guid = ? AND id = ?")
            .bind(guid)
            .bind(id)
            .execute(pool)
            .await?;
    }
    Ok(())
}

pub async fn list_ab_tags(pool: &SqlitePool, guid: &str) -> Result<Vec<AbTag>> {
    Ok(
        sqlx::query_as::<_, AbTag>("SELECT name, color FROM ab_tags WHERE guid = ? ORDER BY name")
            .bind(guid)
            .fetch_all(pool)
            .await?,
    )
}

pub async fn put_ab_tag(pool: &SqlitePool, guid: &str, name: &str, color: i64) -> Result<()> {
    sqlx::query(
        "INSERT INTO ab_tags (guid, name, color) VALUES (?, ?, ?)
         ON CONFLICT(guid, name) DO UPDATE SET color = excluded.color",
    )
    .bind(guid)
    .bind(name)
    .bind(color)
    .execute(pool)
    .await?;
    Ok(())
}

/// Renames a tag and rewrites it inside every peer's tag list.
pub async fn rename_ab_tag(pool: &SqlitePool, guid: &str, old: &str, new: &str) -> Result<()> {
    let mut tx = pool.begin().await?;

    sqlx::query("UPDATE OR REPLACE ab_tags SET name = ? WHERE guid = ? AND name = ?")
        .bind(new)
        .bind(guid)
        .bind(old)
        .execute(&mut *tx)
        .await?;

    let peers = sqlx::query_as::<_, (String, String)>(
        "SELECT id, tags FROM ab_peers WHERE guid = ?",
    )
    .bind(guid)
    .fetch_all(&mut *tx)
    .await?;

    for (id, tags_json) in peers {
        let mut tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
        if !tags.iter().any(|t| t == old) {
            continue;
        }
        tags = tags
            .into_iter()
            .map(|t| if t == old { new.to_string() } else { t })
            .collect();
        tags.dedup();
        sqlx::query("UPDATE ab_peers SET tags = ? WHERE guid = ? AND id = ?")
            .bind(serde_json::to_string(&tags).unwrap_or_else(|_| "[]".into()))
            .bind(guid)
            .bind(&id)
            .execute(&mut *tx)
            .await?;
    }

    tx.commit().await?;
    Ok(())
}

/// Deletes tags and strips them from every peer's tag list.
pub async fn delete_ab_tags(pool: &SqlitePool, guid: &str, names: &[String]) -> Result<()> {
    let mut tx = pool.begin().await?;

    for name in names {
        sqlx::query("DELETE FROM ab_tags WHERE guid = ? AND name = ?")
            .bind(guid)
            .bind(name)
            .execute(&mut *tx)
            .await?;
    }

    let peers =
        sqlx::query_as::<_, (String, String)>("SELECT id, tags FROM ab_peers WHERE guid = ?")
            .bind(guid)
            .fetch_all(&mut *tx)
            .await?;

    for (id, tags_json) in peers {
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
        let kept: Vec<String> = tags
            .iter()
            .filter(|t| !names.contains(t))
            .cloned()
            .collect();
        if kept.len() == tags.len() {
            continue;
        }
        sqlx::query("UPDATE ab_peers SET tags = ? WHERE guid = ? AND id = ?")
            .bind(serde_json::to_string(&kept).unwrap_or_else(|_| "[]".into()))
            .bind(guid)
            .bind(&id)
            .execute(&mut *tx)
            .await?;
    }

    tx.commit().await?;
    Ok(())
}
