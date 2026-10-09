//! Postgres pool construction, migrations, and worker identity.

use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};

use crate::config::PostgresConfig;

pub const MAX_CONNECTIONS: u32 = 8;

/// Builds a pool from `[postgres]`. The options are assembled field by field,
/// so no connection URL (and no password in a string) ever exists.
pub async fn connect(config: &PostgresConfig) -> Result<PgPool> {
    let options = PgConnectOptions::new()
        .host(&config.host)
        .port(config.port)
        .database(&config.db)
        .username(&config.user)
        .password(config.password.expose());
    PgPoolOptions::new()
        .max_connections(MAX_CONNECTIONS)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(options)
        .await
        .context("connect to Postgres (check [postgres] host, port, and credentials)")
}

/// Applies `db/migrations` (embedded at compile time).
pub async fn migrate(pool: &PgPool) -> Result<()> {
    sqlx::migrate!("../../db/migrations")
        .run(pool)
        .await
        .context("apply database migrations")
}

/// `"<hostname>:<pid>"`, the value stored in `heartbeats.worker` and `leased_by`.
pub fn worker_id() -> String {
    let host = hostname::get()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown-host".to_owned());
    format!("{host}:{}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_id_has_host_and_pid() {
        let id = worker_id();
        let (host, pid) = id.rsplit_once(':').expect("host:pid");
        assert!(!host.is_empty());
        assert_eq!(pid, std::process::id().to_string());
    }
}
