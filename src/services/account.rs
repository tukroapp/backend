use chrono::Utc;
use sqlx::{PgPool, Postgres, Transaction};
use std::{sync::Arc, time::Duration};
use tokio::time::sleep;
use tracing::{error, info, instrument};
use uuid::Uuid;

use crate::{services::storage::StorageService, startup::RedisPool, utils::db_err};

/// Quanto tempo a conta fica desativada antes de a exclusão valer. Qualquer
/// login nesse prazo cancela o pedido.
pub const DELETION_GRACE_DAYS: i64 = 30;

/// De quanto em quanto tempo o worker procura contas vencidas. O prazo é de
/// dias, então uma passada por dia basta.
const DELETION_POLL_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Quantas contas o worker anonimiza por rodada.
const DELETION_BATCH_SIZE: i64 = 20;

/// Passa a posse das comunidades adiante antes de a conta virar tombstone: o
/// admin mais antigo assume, senão o membro mais antigo. Comunidade que fica
/// sem ninguém sai do ar.
#[instrument(skip_all, fields(user_id = %user_id))]
async fn hand_over_communities(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<(), actix_web::Error> {
    let owned = sqlx::query_scalar!(
        r#"
        SELECT community_id
        FROM community_member
        WHERE user_id = $1 AND role = 'owner'
        "#,
        user_id,
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(db_err)?;

    for community_id in owned {
        // O `ORDER BY role DESC` coloca 'admin' na frente de 'member', que é a
        // ordem do enum no banco.
        let heir = sqlx::query_scalar!(
            r#"
            SELECT cm.user_id
            FROM community_member cm
            INNER JOIN auth_user au ON au.id = cm.user_id AND au.status = 'active'
            WHERE cm.community_id = $1 AND cm.user_id <> $2
            ORDER BY cm.role DESC, cm.joined_at
            LIMIT 1
            "#,
            community_id,
            user_id,
        )
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_err)?;

        let Some(heir) = heir else {
            sqlx::query!(
                r#"UPDATE community SET deleted_at = NOW() WHERE id = $1"#,
                community_id,
            )
            .execute(&mut **tx)
            .await
            .map_err(db_err)?;
            continue;
        };

        // O índice único de dono impede dois owners na mesma comunidade, então
        // o antigo sai da posse antes de o novo assumir.
        sqlx::query!(
            r#"
            UPDATE community_member SET role = 'member'
            WHERE community_id = $1 AND user_id = $2
            "#,
            community_id,
            user_id,
        )
        .execute(&mut **tx)
        .await
        .map_err(db_err)?;

        sqlx::query!(
            r#"
            UPDATE community_member SET role = 'owner'
            WHERE community_id = $1 AND user_id = $2
            "#,
            community_id,
            heir,
        )
        .execute(&mut **tx)
        .await
        .map_err(db_err)?;
    }

    Ok(())
}

/// Apaga o que é dado pessoal e desmancha as relações. O que a pessoa escreveu
/// em espaço público (recados, depoimentos, tópicos, comentários, enquetes)
/// fica: some a identidade, não a thread dos outros.
#[instrument(skip_all, fields(user_id = %user_id))]
async fn erase_personal_data(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<(), actix_web::Error> {
    // Conversa privada some inteira: a mensagem é de duas pessoas, e o que
    // sobraria do outro lado seria um monólogo com um fantasma.
    sqlx::query!(
        r#"
        DELETE FROM conversation
        WHERE user_lo = $1 OR user_hi = $1
        "#,
        user_id,
    )
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;

    sqlx::query!(
        r#"DELETE FROM verification_token WHERE user_id = $1"#,
        user_id
    )
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;

    sqlx::query!(
        r#"DELETE FROM friend WHERE user_id = $1 OR friend_id = $1"#,
        user_id,
    )
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;

    sqlx::query!(
        r#"DELETE FROM friend_request WHERE requester_id = $1 OR addressee_id = $1"#,
        user_id,
    )
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;

    sqlx::query!(
        r#"DELETE FROM fan WHERE fan_id = $1 OR idol_id = $1"#,
        user_id
    )
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;

    sqlx::query!(
        r#"DELETE FROM block WHERE blocker_id = $1 OR blocked_id = $1"#,
        user_id,
    )
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;

    sqlx::query!(
        r#"DELETE FROM profile_rating WHERE rater_id = $1 OR rated_id = $1"#,
        user_id,
    )
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;

    sqlx::query!(
        r#"DELETE FROM community_member WHERE user_id = $1"#,
        user_id
    )
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;

    sqlx::query!(
        r#"DELETE FROM community_join_request WHERE user_id = $1"#,
        user_id,
    )
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;

    sqlx::query!(
        r#"DELETE FROM community_poll_vote WHERE user_id = $1"#,
        user_id,
    )
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;

    sqlx::query!(
        r#"DELETE FROM notification WHERE user_id = $1 OR actor_id = $1"#,
        user_id,
    )
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;

    sqlx::query!(
        r#"DELETE FROM activity WHERE actor_id = $1 OR target_user_id = $1"#,
        user_id,
    )
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;

    Ok(())
}

/// Vira a conta em tombstone: e-mail e perfil deixam de identificar alguém e o
/// login para de funcionar de vez.
#[instrument(skip_all, fields(user_id = %user_id))]
async fn write_tombstone(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<(), actix_web::Error> {
    // `.invalid` é reservado justamente para isso e nunca resolve, então nenhum
    // e-mail sai por engano para esse endereço.
    let email = format!("deleted-{}@tukro.invalid", Uuid::new_v4());
    let slug = format!("deleted-{}", &Uuid::new_v4().simple().to_string()[..12]);

    sqlx::query!(
        r#"
        UPDATE auth_user
        SET status = 'deleted',
            status_reason = NULL,
            suspended_until = NULL,
            status_changed_at = NOW(),
            status_changed_by = NULL,
            deletion_scheduled_at = NULL,
            email = $2,
            password_hash = NULL,
            google_id = NULL,
            is_email_verified = FALSE,
            last_login = NULL
        WHERE id = $1
        "#,
        user_id,
        email,
    )
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;

    sqlx::query!(
        r#"
        UPDATE user_profile
        SET slug = $2,
            name = 'Usuário excluído',
            avatar_url = NULL,
            status = NULL,
            bio = NULL,
            city = NULL,
            birthdate = NULL,
            relationship_status = NULL,
            country = NULL,
            state = NULL,
            languages = '{}',
            interests = '{}',
            who_i_want_to_meet = NULL,
            occupation = NULL,
            company = NULL,
            education = NULL,
            music = '{}',
            movies = '{}',
            books = '{}',
            tv_shows = '{}',
            foods = '{}',
            sports = '{}',
            pets = NULL,
            fashion_style = NULL
        WHERE user_id = $1
        "#,
        user_id,
        slug,
    )
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;

    Ok(())
}

/// Executa a exclusão definitiva de uma conta. É irreversível: depois disso não
/// há mais como dizer quem era.
#[instrument(skip_all, fields(user_id = %user_id))]
pub async fn anonymize_account(
    pool: &PgPool,
    redis: &RedisPool,
    storage: &dyn StorageService,
    user_id: Uuid,
) -> Result<(), actix_web::Error> {
    // As keys saem do banco antes da transação para os arquivos poderem ser
    // apagados depois do commit: objeto órfão no bucket é sujeira, linha
    // apontando para arquivo inexistente é erro na tela.
    let keys = sqlx::query_scalar!(
        r#"
        SELECT storage_key FROM user_media WHERE user_id = $1
        UNION ALL
        SELECT avatar_url FROM user_profile WHERE user_id = $1 AND avatar_url IS NOT NULL
        "#,
        user_id,
    )
    .fetch_all(pool)
    .await
    .map_err(db_err)?;

    let mut tx = pool.begin().await.map_err(db_err)?;

    hand_over_communities(&mut tx, user_id).await?;
    erase_personal_data(&mut tx, user_id).await?;

    sqlx::query!(r#"DELETE FROM user_media WHERE user_id = $1"#, user_id)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

    write_tombstone(&mut tx, user_id).await?;

    tx.commit().await.map_err(db_err)?;

    for key in keys.into_iter().flatten() {
        if let Err(error) = storage.delete(&key).await {
            error!(%error, %key, "failed to delete file of a deleted account");
        }
    }

    crate::services::auth::delete_all_sessions(redis, &user_id).await?;

    Ok(())
}

/// Roda a exclusão das contas cujo prazo de arrependimento venceu.
pub struct DeletionWorker {
    pool: PgPool,
    redis: RedisPool,
    storage: Arc<dyn StorageService>,
}

impl DeletionWorker {
    pub fn new(pool: PgPool, redis: RedisPool, storage: Arc<dyn StorageService>) -> Self {
        Self {
            pool,
            redis,
            storage,
        }
    }

    pub async fn run(self) {
        info!(
            grace_days = DELETION_GRACE_DAYS,
            "account deletion worker started"
        );

        loop {
            // O lote cheio significa que ainda há fila, e com uma passada por
            // dia não dá para deixar o resto para amanhã.
            let mut drained = false;

            match self.due_accounts().await {
                Ok(accounts) => {
                    drained = accounts.len() as i64 == DELETION_BATCH_SIZE;

                    for user_id in accounts {
                        match anonymize_account(
                            &self.pool,
                            &self.redis,
                            self.storage.as_ref(),
                            user_id,
                        )
                        .await
                        {
                            Ok(()) => info!(%user_id, "account anonymized"),
                            Err(error) => error!(%error, %user_id, "failed to anonymize account"),
                        }
                    }
                }
                Err(error) => error!(%error, "failed to list accounts due for deletion"),
            }

            if drained {
                continue;
            }

            sleep(DELETION_POLL_INTERVAL).await;
        }
    }

    async fn due_accounts(&self) -> Result<Vec<Uuid>, sqlx::Error> {
        sqlx::query_scalar!(
            r#"
            SELECT id
            FROM auth_user
            WHERE deletion_scheduled_at IS NOT NULL
              AND deletion_scheduled_at <= $1
            ORDER BY deletion_scheduled_at
            LIMIT $2
            "#,
            Utc::now(),
            DELETION_BATCH_SIZE,
        )
        .fetch_all(&self.pool)
        .await
    }
}
