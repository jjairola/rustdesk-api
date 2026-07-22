use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: String,
    pub database_url: String,
    pub token_ttl: Duration,
    /// Devices not seen for this many days are hidden from the address book.
    /// 0 disables the filter (every device ever registered stays listed).
    pub device_stale_days: i64,
    /// Optional bootstrap admin, applied once on startup if the user table is empty.
    pub admin_user: Option<String>,
    pub admin_password: Option<String>,
}

impl Config {
    pub fn from_env() -> Self {
        let bind = env_or("RDAPI_BIND", "0.0.0.0:21114");
        let database_url = env_or("RDAPI_DB", "sqlite://rustdesk-api.db");
        let token_ttl_days = env_parse("RDAPI_TOKEN_TTL_DAYS", 30i64).max(1);
        let device_stale_days = env_parse("RDAPI_DEVICE_STALE_DAYS", 0i64).max(0);

        Self {
            bind,
            database_url,
            token_ttl: Duration::from_secs(token_ttl_days as u64 * 86_400),
            device_stale_days,
            admin_user: std::env::var("RDAPI_ADMIN_USER").ok().filter(|s| !s.is_empty()),
            admin_password: std::env::var("RDAPI_ADMIN_PASSWORD")
                .ok()
                .filter(|s| !s.is_empty()),
        }
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}
