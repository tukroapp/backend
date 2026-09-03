use anyhow::Context;
use deadpool_redis::redis::cmd;

use crate::{
    configuration::Settings,
    startup::{get_connection_pool, get_redis_pool},
};

/// Abre conexão com os dois serviços e conta o que aconteceu. Serve para
/// conferir a configuração de um servidor sem subir o processo inteiro.
pub async fn run(configuration: &Settings) -> Result<(), anyhow::Error> {
    let postgres = ping_postgres(configuration).await;
    report("postgres", &postgres);

    let redis = ping_redis(configuration).await;
    report("redis", &redis);

    if postgres.is_err() || redis.is_err() {
        anyhow::bail!("Nem todos os serviços responderam.");
    }

    Ok(())
}

fn report(service: &str, result: &Result<(), anyhow::Error>) {
    match result {
        Ok(()) => println!("{service}: ok"),
        Err(error) => println!("{service}: falhou ({error:#})"),
    }
}

async fn ping_postgres(configuration: &Settings) -> Result<(), anyhow::Error> {
    let pool = get_connection_pool(&configuration.database);

    sqlx::query_scalar!(r#"SELECT 1 AS "one!""#)
        .fetch_one(&pool)
        .await
        .context("Failed to query Postgres")?;

    Ok(())
}

async fn ping_redis(configuration: &Settings) -> Result<(), anyhow::Error> {
    let pool = get_redis_pool(&configuration.redis)?;
    let mut connection = pool.get().await.context("Failed to connect to Redis")?;

    let pong: String = cmd("PING")
        .query_async(&mut connection)
        .await
        .context("Failed to PING Redis")?;

    if pong != "PONG" {
        anyhow::bail!("PING respondeu {pong}");
    }

    Ok(())
}
