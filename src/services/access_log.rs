//! Registro de acesso do art. 15 do Marco Civil. Só o login é registrado, não
//! cada requisição autenticada: a ordem judicial pergunta de qual IP a conta foi
//! acessada e quando, e isso o login já responde.

use chrono::{Duration as ChronoDuration, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{error, info};
use uuid::Uuid;

use crate::services::auth::SessionMeta;

/// Seis meses, o prazo do art. 15.
pub const ACCESS_LOG_RETENTION_DAYS: i64 = 180;

const PURGE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Espelha o enum `login_method` do banco.
#[derive(Debug, Clone, Copy, sqlx::Type)]
#[sqlx(type_name = "login_method", rename_all = "snake_case")]
pub enum LoginMethod {
    Password,
    Google,
}

/// Grava na mesma transação do login: falhar aqui derruba o login, porque entrar
/// sem deixar registro é o que a lei não quer.
pub async fn record_login(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    method: LoginMethod,
    meta: &SessionMeta,
) -> Result<(), sqlx::Error> {
    if meta.ip.is_none() {
        tracing::warn!(%user_id, "login sem IP identificável, registro de acesso incompleto");
    }

    sqlx::query!(
        r#"
        INSERT INTO access_log (user_id, method, ip, user_agent)
        VALUES ($1, $2, $3, $4)
        "#,
        user_id,
        method as LoginMethod,
        meta.ip,
        meta.user_agent,
    )
    .execute(&mut **tx)
    .await?;

    Ok(())
}

pub struct AccessLogPurgeWorker {
    pool: PgPool,
}

impl AccessLogPurgeWorker {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn run(self) {
        info!(
            retention_days = ACCESS_LOG_RETENTION_DAYS,
            "access log purge worker started"
        );

        loop {
            match self.purge().await {
                Ok(deleted) if deleted > 0 => info!(deleted, "access log entries purged"),
                Ok(_) => {}
                Err(error) => error!(%error, "failed to purge the access log"),
            }

            sleep(PURGE_INTERVAL).await;
        }
    }

    async fn purge(&self) -> Result<u64, sqlx::Error> {
        let cutoff = Utc::now() - ChronoDuration::days(ACCESS_LOG_RETENTION_DAYS);

        let result = sqlx::query!("DELETE FROM access_log WHERE created_at < $1", cutoff)
            .execute(&self.pool)
            .await?;

        Ok(result.rows_affected())
    }
}
