//! Cache de leitura no Redis para respostas globais e estáticas, guardadas como
//! JSON com TTL. É um cache burro de propósito: não invalida na escrita, só
//! espera o TTL vencer. Só serve para número que ninguém confere.
//!
//! Redis fora do ar nunca derruba a requisição: a leitura vira `None` e o
//! handler cai no Postgres.

use deadpool_redis::redis::cmd;
use serde::{Serialize, de::DeserializeOwned};

use crate::startup::RedisPool;

/// Índice de categorias de comunidade (`GET /api/v1/community/category`).
pub const COMMUNITY_CATEGORY_COUNTS: &str = "cache:community:category_counts";
/// Cinco minutos: a contagem de comunidades por categoria muda devagar e a tela
/// de descoberta não precisa do número exato.
pub const CATEGORY_COUNTS_TTL: i64 = 300;

pub async fn get_json<T: DeserializeOwned>(redis: &RedisPool, key: &str) -> Option<T> {
    match try_get_json(redis, key).await {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, key, "falha ao ler cache");
            None
        }
    }
}

async fn try_get_json<T: DeserializeOwned>(
    redis: &RedisPool,
    key: &str,
) -> Result<Option<T>, anyhow::Error> {
    let mut conn = redis.get().await?;
    let raw: Option<String> = cmd("GET").arg(key).query_async(&mut conn).await?;

    match raw {
        Some(raw) => Ok(Some(serde_json::from_str(&raw)?)),
        None => Ok(None),
    }
}

pub async fn set_json<T: Serialize>(redis: &RedisPool, key: &str, value: &T, ttl_seconds: i64) {
    if let Err(error) = try_set_json(redis, key, value, ttl_seconds).await {
        tracing::warn!(%error, key, "falha ao gravar cache");
    }
}

async fn try_set_json<T: Serialize>(
    redis: &RedisPool,
    key: &str,
    value: &T,
    ttl_seconds: i64,
) -> Result<(), anyhow::Error> {
    let raw = serde_json::to_string(value)?;
    let mut conn = redis.get().await?;

    cmd("SET")
        .arg(key)
        .arg(raw)
        .arg("EX")
        .arg(ttl_seconds)
        .query_async::<()>(&mut conn)
        .await?;

    Ok(())
}
