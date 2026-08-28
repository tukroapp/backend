use anyhow::anyhow;
use sqlx::{Acquire, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    routes::community::CommunityCategory,
    utils::{db_err, e500, gen_random_string},
};

const SLUG_MAX_LENGTH: usize = 30;
const SLUG_MIN_LENGTH: usize = 3;
const SLUG_SUFFIX_LENGTH: usize = 4;
/// Comprimento máximo da base ao anexar um sufixo `-xxxx`, mantendo o total dentro de 30.
const SLUG_BASE_WITH_SUFFIX_LENGTH: usize = SLUG_MAX_LENGTH - SLUG_SUFFIX_LENGTH - 1;
const SLUG_MAX_ATTEMPTS: usize = 8;

/// O CHECK da `community` aceita até 50 caracteres, contra 30 do perfil.
const COMMUNITY_SLUG_MAX_LENGTH: usize = 50;
const COMMUNITY_SLUG_BASE_WITH_SUFFIX_LENGTH: usize =
    COMMUNITY_SLUG_MAX_LENGTH - SLUG_SUFFIX_LENGTH - 1;

/// Normaliza para o formato do CHECK: minúsculas, apenas `[a-z0-9-]`, começando e terminando em
/// alfanumérico, truncado em `max_len`. Devolve `None` se sobrar menos de 3 caracteres.
fn normalize(name: &str, max_len: usize) -> Option<String> {
    let mut slug = slugrs::slugify(name);
    slug.truncate(max_len);
    let slug = slug.trim_matches('-');

    if slug.len() >= SLUG_MIN_LENGTH {
        Some(slug.to_string())
    } else {
        None
    }
}

fn random_suffix() -> String {
    gen_random_string(SLUG_SUFFIX_LENGTH).to_lowercase()
}

/// Cria o perfil do usuário gerando um slug único a partir do nome. Em caso de colisão (unique
/// 23505) tenta um sufixo aleatório, usando savepoints para não abortar a transação externa.
/// Não faz SELECT prévio de existência: a constraint decide. Devolve o slug efetivamente usado.
pub async fn create_user_profile(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    name: &str,
) -> Result<String, actix_web::Error> {
    let base = normalize(name, SLUG_MAX_LENGTH);
    let base_for_suffix = normalize(name, SLUG_BASE_WITH_SUFFIX_LENGTH);

    for attempt in 0..SLUG_MAX_ATTEMPTS {
        let candidate = match (&base, &base_for_suffix, attempt) {
            (Some(base), _, 0) => base.clone(),
            (_, Some(base), _) => format!("{}-{}", base, random_suffix()),
            // Nome curto demais ou só com símbolos/emoji: fallback determinístico.
            _ => format!("user-{}", random_suffix()),
        };

        let mut savepoint = tx.begin().await.map_err(e500)?;

        let result = sqlx::query!(
            r#"
            INSERT INTO user_profile (user_id, slug, name)
            VALUES ($1, $2, $3)
            "#,
            user_id,
            candidate,
            name
        )
        .execute(&mut *savepoint)
        .await;

        match result {
            Ok(_) => {
                savepoint.commit().await.map_err(e500)?;
                return Ok(candidate);
            }
            Err(sqlx::Error::Database(db)) if db.code().as_deref() == Some("23505") => {
                savepoint.rollback().await.map_err(e500)?;
                continue;
            }
            Err(e) => {
                savepoint.rollback().await.ok();
                return Err(db_err(e));
            }
        }
    }

    Err(e500(anyhow!(
        "could not generate a unique slug after {SLUG_MAX_ATTEMPTS} attempts"
    )))
}

/// Cria a comunidade com a mesma estratégia de slug do perfil. Nome repetido é a regra aqui
/// ("eu odeio acordar cedo" existe às centenas), então a colisão cai no sufixo aleatório quase
/// sempre. Devolve o id e o slug efetivamente usados.
pub async fn create_community(
    tx: &mut Transaction<'_, Postgres>,
    name: &str,
    description: Option<&str>,
    category: CommunityCategory,
    is_private: bool,
) -> Result<(Uuid, String), actix_web::Error> {
    let base = normalize(name, COMMUNITY_SLUG_MAX_LENGTH);
    let base_for_suffix = normalize(name, COMMUNITY_SLUG_BASE_WITH_SUFFIX_LENGTH);

    for attempt in 0..SLUG_MAX_ATTEMPTS {
        let candidate = match (&base, &base_for_suffix, attempt) {
            (Some(base), _, 0) => base.clone(),
            (_, Some(base), _) => format!("{}-{}", base, random_suffix()),
            // Nome curto demais ou só com símbolos/emoji: fallback determinístico.
            _ => format!("comunidade-{}", random_suffix()),
        };

        let mut savepoint = tx.begin().await.map_err(e500)?;

        let result = sqlx::query_scalar!(
            r#"
            INSERT INTO community (slug, name, description, category, is_private)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING id
            "#,
            candidate,
            name,
            description,
            category as CommunityCategory,
            is_private,
        )
        .fetch_one(&mut *savepoint)
        .await;

        match result {
            Ok(id) => {
                savepoint.commit().await.map_err(e500)?;
                return Ok((id, candidate));
            }
            Err(sqlx::Error::Database(db)) if db.code().as_deref() == Some("23505") => {
                savepoint.rollback().await.map_err(e500)?;
                continue;
            }
            Err(e) => {
                savepoint.rollback().await.ok();
                return Err(db_err(e));
            }
        }
    }

    Err(e500(anyhow!(
        "could not generate a unique community slug after {SLUG_MAX_ATTEMPTS} attempts"
    )))
}
