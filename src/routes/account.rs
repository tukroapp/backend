use actix_web::{
    HttpResponse, Responder,
    web::{Data, Json},
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;

use crate::{
    services::{
        account::DELETION_GRACE_DAYS,
        auth::{LoginCredentials, SessionData, delete_all_sessions, validate_credentials},
    },
    startup::RedisPool,
    utils::{ApiErrorResponse, ApiResponse, db_err},
};

#[derive(Debug, Deserialize, ToSchema)]
pub struct AccountClosingRequest {
    /// Senha atual. Obrigatória para quem entra com senha, ignorada na conta do
    /// Google, que não tem uma.
    pub password: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct DeletionResponse {
    /// Quando a exclusão definitiva acontece. Até lá, basta entrar de novo para
    /// cancelar.
    pub scheduled_for: DateTime<Utc>,
}

fn wrong_password() -> HttpResponse {
    HttpResponse::BadRequest().json(ApiErrorResponse::from_single_validation_error(
        "password",
        "invalid",
        Some("A senha está incorreta."),
        vec![],
    ))
}

/// Confere a senha e desativa a conta. `scheduled_for` preenchido marca também
/// a exclusão definitiva.
async fn close_account(
    pool: &PgPool,
    redis: &RedisPool,
    user_id: uuid::Uuid,
    password: Option<&str>,
    scheduled_for: Option<DateTime<Utc>>,
) -> Result<HttpResponse, actix_web::Error> {
    let mut tx = pool.begin().await.map_err(db_err)?;

    let user = sqlx::query!(
        r#"
        SELECT email, password_hash
        FROM auth_user
        WHERE id = $1
        FOR UPDATE
        "#,
        user_id,
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(db_err)?;

    // Conta do Google não tem senha para conferir: a sessão já é a prova de que
    // é o dono.
    if user.password_hash.is_some() {
        let Some(password) = password.filter(|password| !password.is_empty()) else {
            return Ok(wrong_password());
        };

        let credentials = LoginCredentials {
            email: user.email,
            password: password.to_string(),
        };

        if validate_credentials(credentials, &mut tx).await.is_err() {
            return Ok(wrong_password());
        }
    }

    sqlx::query!(
        r#"
        UPDATE auth_user
        SET status = 'deactivated',
            status_reason = NULL,
            suspended_until = NULL,
            status_changed_at = NOW(),
            status_changed_by = NULL,
            deletion_scheduled_at = $2
        WHERE id = $1
        "#,
        user_id,
        scheduled_for,
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;

    tx.commit().await.map_err(db_err)?;

    // A sessão vive no Redis e não consulta o banco a cada requisição, então a
    // saída só vale de imediato se derrubar o que já está aberto.
    delete_all_sessions(redis, &user_id).await?;

    Ok(match scheduled_for {
        None => HttpResponse::NoContent().finish(),
        Some(scheduled_for) => {
            HttpResponse::Ok().json(ApiResponse::new(DeletionResponse { scheduled_for }))
        }
    })
}

#[utoipa::path(
    delete,
    path = "/api/v1/me",
    description = "Desativa a própria conta: o perfil some do site e as sessões caem. Entrar de novo devolve tudo, nada é apagado",
    request_body = AccountClosingRequest,
    responses(
        (status = 204, description = "Conta desativada"),
        (status = 400, description = "Senha incorreta", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Desativa a própria conta", skip_all)]
pub async fn deactivate(
    payload: Json<AccountClosingRequest>,
    session: SessionData,
    pool: Data<PgPool>,
    redis: Data<RedisPool>,
) -> Result<impl Responder, actix_web::Error> {
    close_account(
        pool.get_ref(),
        redis.get_ref(),
        session.user_id,
        payload.password.as_deref(),
        None,
    )
    .await
}

#[utoipa::path(
    post,
    path = "/api/v1/me/deletion",
    description = "Pede a exclusão definitiva da conta. Ela é desativada na hora e apagada depois do prazo de arrependimento; entrar de novo antes disso cancela o pedido",
    request_body = AccountClosingRequest,
    responses(
        (status = 200, description = "Exclusão agendada", body = ApiResponse<DeletionResponse>),
        (status = 400, description = "Senha incorreta", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Agenda a exclusão da própria conta", skip_all)]
pub async fn schedule_deletion(
    payload: Json<AccountClosingRequest>,
    session: SessionData,
    pool: Data<PgPool>,
    redis: Data<RedisPool>,
) -> Result<impl Responder, actix_web::Error> {
    let scheduled_for = Utc::now() + Duration::days(DELETION_GRACE_DAYS);

    close_account(
        pool.get_ref(),
        redis.get_ref(),
        session.user_id,
        payload.password.as_deref(),
        Some(scheduled_for),
    )
    .await
}
