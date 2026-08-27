//! Visitas de perfil, guardadas só no Redis. Uma sorted set por perfil,
//! `visits:{profile_id}`, com o `visitor_id` como membro e o timestamp da última
//! visita como score. O `ZADD GT` deduplica: revisitar só empurra o score.
//!
//! Nada disso é persistido no Postgres. Se o Redis cair, a gravação é ignorada e
//! a leitura devolve zero.

use chrono::{DateTime, Utc};
use deadpool_redis::redis::{cmd, pipe};
use uuid::Uuid;

use crate::startup::RedisPool;

/// Janela do contador de visitantes distintos.
const MONTH_SECONDS: i64 = 30 * 24 * 60 * 60;
/// Janela da lista de visitantes recentes.
const RECENT_SECONDS: i64 = 7 * 24 * 60 * 60;
/// Teto de nomes devolvidos na lista de recentes.
const RECENT_LIMIT: i64 = 20;

fn visits_key(profile_id: Uuid) -> String {
    format!("visits:{profile_id}")
}

pub struct VisitorEntry {
    pub visitor_id: Uuid,
    pub visited_at: DateTime<Utc>,
}

/// Registra a visita de `visitor_id` ao perfil `profile_id`. Nunca propaga erro:
/// visita é um extra, não pode derrubar o `GET /profile`.
pub async fn record_visit(redis: &RedisPool, profile_id: Uuid, visitor_id: Uuid) {
    if let Err(error) = try_record_visit(redis, profile_id, visitor_id).await {
        tracing::warn!(%error, "falha ao registrar visita ao perfil");
    }
}

async fn try_record_visit(
    redis: &RedisPool,
    profile_id: Uuid,
    visitor_id: Uuid,
) -> Result<(), anyhow::Error> {
    let now = Utc::now().timestamp();
    let cutoff = now - MONTH_SECONDS;
    let key = visits_key(profile_id);

    let mut conn = redis.get().await?;

    pipe()
        .cmd("ZADD")
        .arg(&key)
        .arg("GT")
        .arg(now)
        .arg(visitor_id.to_string())
        .ignore()
        .cmd("ZREMRANGEBYSCORE")
        .arg(&key)
        .arg("-inf")
        .arg(cutoff)
        .ignore()
        .cmd("EXPIRE")
        .arg(&key)
        .arg(MONTH_SECONDS)
        .ignore()
        .query_async::<()>(&mut conn)
        .await?;

    Ok(())
}

/// Devolve o total de visitantes distintos nos últimos 30 dias e a lista dos
/// mais recentes (últimos 7 dias). Redis indisponível vira `(0, vec![])`.
pub async fn recent_visitors(redis: &RedisPool, profile_id: Uuid) -> (i64, Vec<VisitorEntry>) {
    match try_recent_visitors(redis, profile_id).await {
        Ok(result) => result,
        Err(error) => {
            tracing::warn!(%error, "falha ao ler visitantes do perfil");
            (0, Vec::new())
        }
    }
}

async fn try_recent_visitors(
    redis: &RedisPool,
    profile_id: Uuid,
) -> Result<(i64, Vec<VisitorEntry>), anyhow::Error> {
    let now = Utc::now().timestamp();
    let month_cutoff = now - MONTH_SECONDS;
    let recent_cutoff = now - RECENT_SECONDS;
    let key = visits_key(profile_id);

    let mut conn = redis.get().await?;

    // Trim preguiçoso: tira o que passou dos 30 dias antes de contar.
    cmd("ZREMRANGEBYSCORE")
        .arg(&key)
        .arg("-inf")
        .arg(month_cutoff)
        .query_async::<()>(&mut conn)
        .await?;

    let count: i64 = cmd("ZCARD").arg(&key).query_async(&mut conn).await?;

    let raw: Vec<(String, f64)> = cmd("ZREVRANGEBYSCORE")
        .arg(&key)
        .arg("+inf")
        .arg(format!("({recent_cutoff}"))
        .arg("WITHSCORES")
        .arg("LIMIT")
        .arg(0)
        .arg(RECENT_LIMIT)
        .query_async(&mut conn)
        .await?;

    let entries = raw
        .into_iter()
        .filter_map(|(member, score)| {
            Some(VisitorEntry {
                visitor_id: Uuid::parse_str(&member).ok()?,
                visited_at: DateTime::from_timestamp(score as i64, 0)?,
            })
        })
        .collect();

    Ok((count, entries))
}
