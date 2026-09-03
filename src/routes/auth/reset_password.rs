use actix_web::{
    HttpResponse, Responder,
    web::{Data, Json, Query},
};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

use crate::{
    constants::PASSWORD_MIN_LENGTH,
    services::auth::delete_all_sessions,
    startup::RedisPool,
    telemetry::spawn_blocking_with_tracing,
    utils::{ApiErrorResponse, compute_hash, compute_sha_hash, db_err, e500},
};

#[utoipa::path(
    get,
    path = "/api/v1/auth/reset-password",
    description = "Checa a validade do token de reset de senha",
    params(
        ("token" = String, Query, description = "O token de reset de senha enviado por e-mail")
    ),
    responses(
        (status = 204, description = "Token válido"),
        (status = 400, description = "Token inválido ou expirado", body = ApiErrorResponse)
    ),
)]
#[tracing::instrument(name = "Checa validade do token de reset de senha", skip_all)]
pub async fn get(
    query: Query<ResetPasswordQuery>,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    match query.validate() {
        Ok(_) => {}
        Err(e) => {
            return Ok(HttpResponse::BadRequest().json(ApiErrorResponse::from(e)));
        }
    };

    // Safe to unwrap after validation
    let token = query.token.clone().unwrap();
    let maybe_token_id = sqlx::query!(
        r#"
            SELECT id
            FROM verification_token
            WHERE token_hash = $1
                AND kind = 'password_reset'
                AND expires_at > NOW()
                AND used_at IS NULL
        "#,
        compute_sha_hash(&token)
    )
    .fetch_optional(pool.get_ref())
    .await
    .map_err(db_err)?;

    if maybe_token_id.is_some() {
        Ok(HttpResponse::NoContent().finish())
    } else {
        Ok(
            HttpResponse::BadRequest().json(ApiErrorResponse::from_single_validation_error(
                "token",
                "invalid",
                Some("Token inválido ou expirado"),
                vec![],
            )),
        )
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/auth/reset-password",
    description = "Reseta a senha do usuário utilizando o token de reset de senha",
    request_body = ResetPasswordPayload,
    responses(
        (status = 204, description = "Senha resetada com sucesso"),
        (status = 400, description = "Requisição inválida", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Reseta a senha", skip_all)]
pub async fn post(
    payload: Json<ResetPasswordPayload>,
    pool: Data<PgPool>,
    redis: Data<RedisPool>,
) -> Result<impl Responder, actix_web::Error> {
    match payload.validate() {
        Ok(_) => {}
        Err(e) => {
            return Ok(HttpResponse::BadRequest().json(ApiErrorResponse::from(e)));
        }
    };

    // Safe to unwrap after validation
    let token = payload.token.clone().unwrap();
    let token_hash = compute_sha_hash(&token);

    let new_password = payload.new_password.clone();

    let mut tx = pool
        .begin()
        .await
        .context("Failed to begin tx for resetting password")
        .map_err(e500)?;

    let maybe_token_info = sqlx::query_as!(
        TokenInfo,
        r#"
            SELECT id, user_id
            FROM verification_token
            WHERE token_hash = $1
                AND kind = 'password_reset'
                AND expires_at > NOW()
                AND used_at IS NULL
        "#,
        token_hash
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_err)?;

    if maybe_token_info.is_none() {
        return Ok(HttpResponse::BadRequest().json(
            ApiErrorResponse::from_single_validation_error(
                "token",
                "invalid",
                Some("Token inválido ou expirado"),
                vec![],
            ),
        ));
    }

    // Safe to unwrap after validation
    let token_info = maybe_token_info.unwrap();

    let password_hash = spawn_blocking_with_tracing(move || compute_hash(new_password))
        .await
        .unwrap()
        .context("Failed to compute password hash")
        .map_err(e500)?;

    sqlx::query!(
        r#"UPDATE auth_user SET password_hash = $1 WHERE id = $2"#,
        password_hash,
        token_info.user_id
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;

    sqlx::query!(
        r#"UPDATE verification_token SET used_at = NOW() WHERE id = $1"#,
        token_info.id
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;

    tx.commit()
        .await
        .context("Failed to commit tx for resetting password")
        .map_err(e500)?;

    delete_all_sessions(&redis, &token_info.user_id).await?;

    Ok(HttpResponse::NoContent().finish())
}

#[derive(Serialize, Deserialize, Validate, ToSchema)]
pub struct ResetPasswordPayload {
    #[validate(required(message = "O token é obrigatório"))]
    #[schema(example = "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6")]
    pub token: Option<String>,

    #[validate(
        length(min = PASSWORD_MIN_LENGTH, message = "A senha deve ter pelo menos 8 caracteres"),
        must_match(
            other = "new_password_confirmation",
            message = "As senhas devem coincidir"
        )
    )]
    #[schema(example = "minhanovasenha123")]
    pub new_password: String,
    #[schema(example = "minhanovasenha123")]
    pub new_password_confirmation: String,
}

#[derive(Deserialize, Validate, ToSchema)]
pub struct ResetPasswordQuery {
    #[validate(
        required(message = "Token inválido ou expirado"),
        length(min = 1, message = "Invalid or expired token")
    )]
    #[schema(example = "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6")]
    pub token: Option<String>,
}

struct TokenInfo {
    id: Uuid,
    user_id: Uuid,
}
