use actix_web::{
    HttpResponse, Responder,
    web::{Data, Json, Path},
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

use crate::{
    routes::admin::moderation::{LogEntry, ModerationAction, log_action},
    services::auth::{AccountStatus, SessionData, delete_all_sessions},
    startup::RedisPool,
    utils::{ApiErrorResponse, db_err, invalid_field},
};

#[derive(Debug, Deserialize, ToSchema, Validate)]
pub struct SuspendRequest {
    /// Motivo registrado na conta, para o próximo moderador entender a decisão.
    #[validate(length(min = 1, max = 500))]
    pub reason: String,
    /// Quando a suspensão vence. Ausente significa permanente.
    pub until: Option<DateTime<Utc>>,
}

fn user_not_found() -> HttpResponse {
    HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
        "user",
        "not_found",
        Some("Usuário não encontrado."),
        vec![],
    ))
}

struct TargetAccount {
    id: Uuid,
    status: AccountStatus,
    is_superuser: bool,
}

/// Busca a conta sem filtrar por status: a moderação precisa enxergar quem já
/// está suspenso ou desativado.
async fn fetch_account(
    pool: &PgPool,
    slug: &str,
) -> Result<Option<TargetAccount>, actix_web::Error> {
    let row = sqlx::query!(
        r#"
        SELECT
            au.id,
            au.status AS "status: AccountStatus",
            au.is_superuser
        FROM auth_user au
        INNER JOIN user_profile up ON up.user_id = au.id
        WHERE up.slug = $1
        "#,
        slug
    )
    .fetch_optional(pool)
    .await
    .map_err(db_err)?;

    Ok(row.map(|row| TargetAccount {
        id: row.id,
        status: row.status,
        is_superuser: row.is_superuser,
    }))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/user/{slug}/suspend",
    description = "Suspende a conta, derrubando todas as sessões abertas dela",
    params(("slug" = String, Path, description = "Slug do perfil")),
    request_body = SuspendRequest,
    responses(
        (status = 204, description = "Conta suspensa"),
        (status = 400, description = "Corpo inválido", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Não é administrador", body = ApiErrorResponse),
        (status = 404, description = "Usuário não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Suspende uma conta", skip_all)]
pub async fn suspend(
    slug: Path<String>,
    payload: Json<SuspendRequest>,
    session: SessionData,
    pool: Data<PgPool>,
    redis: Data<RedisPool>,
) -> Result<impl Responder, actix_web::Error> {
    if let Err(errors) = payload.validate() {
        return Ok(HttpResponse::BadRequest().json(ApiErrorResponse::from(errors)));
    }

    if let Some(until) = payload.until
        && until <= Utc::now()
    {
        return Ok(invalid_field(
            "until",
            "must_be_future",
            "O fim da suspensão precisa ser no futuro.",
        ));
    }

    let Some(target) = fetch_account(pool.get_ref(), &slug).await? else {
        return Ok(user_not_found());
    };

    if target.id == session.user_id || target.is_superuser {
        return Ok(
            HttpResponse::Forbidden().json(ApiErrorResponse::from_single_validation_error(
                "user",
                "cannot_suspend_admin",
                Some("Não é possível suspender uma conta da administração."),
                vec![],
            )),
        );
    }

    let mut tx = pool.begin().await.map_err(db_err)?;

    sqlx::query!(
        r#"
        UPDATE auth_user
        SET status = 'suspended',
            status_reason = $2,
            suspended_until = $3,
            status_changed_at = NOW(),
            status_changed_by = $4
        WHERE id = $1
        "#,
        target.id,
        payload.reason.trim(),
        payload.until,
        session.user_id,
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;

    log_action(
        &mut tx,
        LogEntry {
            moderator_id: session.user_id,
            action: ModerationAction::UserSuspended,
            report_id: None,
            reason: Some(payload.reason.trim()),
            content_snapshot: None,
            target_user_id: Some(target.id),
            target_label: Some(&slug),
            content: None,
        },
    )
    .await?;

    tx.commit().await.map_err(db_err)?;

    // A sessão vive no Redis e não consulta o banco a cada requisição, então a
    // suspensão só vale de imediato se derrubar o que já está aberto.
    delete_all_sessions(redis.get_ref(), &target.id).await?;

    Ok(HttpResponse::NoContent().finish())
}

#[utoipa::path(
    delete,
    path = "/api/v1/admin/user/{slug}/suspend",
    description = "Revoga a suspensão e devolve a conta ao ar",
    params(("slug" = String, Path, description = "Slug do perfil")),
    responses(
        (status = 204, description = "Suspensão revogada"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Não é administrador", body = ApiErrorResponse),
        (status = 404, description = "Usuário não encontrado ou não suspenso", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Revoga a suspensão de uma conta", skip_all)]
pub async fn unsuspend(
    slug: Path<String>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(target) = fetch_account(pool.get_ref(), &slug).await? else {
        return Ok(user_not_found());
    };

    if target.status != AccountStatus::Suspended {
        return Ok(user_not_found());
    }

    let mut tx = pool.begin().await.map_err(db_err)?;

    sqlx::query!(
        r#"
        UPDATE auth_user
        SET status = 'active',
            status_reason = NULL,
            suspended_until = NULL,
            status_changed_at = NOW(),
            status_changed_by = $2
        WHERE id = $1
        "#,
        target.id,
        session.user_id,
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;

    log_action(
        &mut tx,
        LogEntry {
            moderator_id: session.user_id,
            action: ModerationAction::UserUnsuspended,
            report_id: None,
            reason: None,
            content_snapshot: None,
            target_user_id: Some(target.id),
            target_label: Some(&slug),
            content: None,
        },
    )
    .await?;

    tx.commit().await.map_err(db_err)?;

    Ok(HttpResponse::NoContent().finish())
}
