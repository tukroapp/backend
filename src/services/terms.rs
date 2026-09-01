//! Aceite dos termos de uso e da política de privacidade. O ônus de provar o
//! consentimento é de quem trata os dados (LGPD, art. 8º §1º), então cada aceite
//! vira uma linha com versão, data e origem.

use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::{constants::CURRENT_TERMS_VERSION, services::auth::SessionMeta};

/// Grava o aceite da versão em vigor. Recebe a transação porque no cadastro o
/// aceite tem que nascer junto da conta, ou nenhum dos dois existe.
pub async fn record_acceptance(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    meta: &SessionMeta,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO terms_acceptance (user_id, version, ip, user_agent)
        VALUES ($1, $2, $3, $4)
        "#,
        user_id,
        CURRENT_TERMS_VERSION,
        meta.ip,
        meta.user_agent,
    )
    .execute(&mut **tx)
    .await?;

    Ok(())
}
