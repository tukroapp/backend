use anyhow::Context;
use sqlx::PgPool;
use tracing::{info, warn};

use crate::{
    services::slug::create_user_profile, telemetry::spawn_blocking_with_tracing,
    utils::compute_hash,
};

/// Cria o superusuário do primeiro acesso, se ainda não houver nenhum. Roda a
/// cada boot e não faz nada quando já existe um admin, então a variável de
/// ambiente com a senha pode sair do servidor depois da primeira troca.
///
/// Não fica na migration de propósito: SQL puro não lê o ambiente, e um hash
/// versionado no repositório é senha pública.
pub async fn ensure_seed_admin(
    pool: &PgPool,
    email: &str,
    password: Option<&str>,
) -> Result<(), anyhow::Error> {
    let exists = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM auth_user WHERE is_superuser) AS "exists!""#
    )
    .fetch_one(pool)
    .await
    .context("Failed to look for an existing superuser")?;

    if exists {
        return Ok(());
    }

    let Some(password) = password else {
        warn!(
            "no superuser in the database and ADMIN_PASSWORD_FIRST_ACCESS is unset: \
             nobody can reach the moderation panel"
        );
        return Ok(());
    };

    let owned = password.to_string();
    let password_hash = spawn_blocking_with_tracing(move || compute_hash(owned))
        .await
        .context("Failed to spawn the hashing task")?
        .context("Failed to hash the seed admin password")?;

    let mut tx = pool.begin().await.context("Failed to start transaction")?;

    let user_id = sqlx::query_scalar!(
        r#"
        INSERT INTO auth_user (email, password_hash, is_superuser, is_email_verified)
        VALUES ($1, $2, TRUE, TRUE)
        RETURNING id
        "#,
        email,
        password_hash,
    )
    .fetch_one(&mut *tx)
    .await
    .context("Failed to create the seed admin")?;

    create_user_profile(&mut tx, &user_id, "Administrador")
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create the seed admin profile: {e}"))?;

    tx.commit().await.context("Failed to commit transaction")?;

    info!(%email, "seed admin created; change this password on the first login");

    Ok(())
}
