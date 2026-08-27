use crate::{
    services::auth::SessionData,
    utils::{ApiErrorResponse, compute_sha_hash, db_err, e500},
};
use actix_web::{
    HttpResponse, Responder,
    web::{Data, Json},
};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use validator::Validate;

#[utoipa::path(
    post,
    path = "/api/v1/auth/verify-email",
    description = "Verifica o email do usuário usando um token de verificação",
    request_body = VerifyEmailPayload,
    responses(
        (status = 204, description = "Email verificado com sucesso"),
        (status = 400, description = "Requisição inválida", body = ApiErrorResponse)
    ),
)]
#[tracing::instrument(name = "Verificar email do usuário", skip_all)]
pub async fn post(
    pool: Data<PgPool>,
    payload: Json<VerifyEmailPayload>,
    session: SessionData,
) -> Result<impl Responder, actix_web::Error> {
    let error_message = "Token inválido ou expirado";

    match payload.validate() {
        Ok(_) => {}
        Err(e) => {
            return Ok(HttpResponse::BadRequest().json(ApiErrorResponse::from(e)));
        }
    }

    let token = payload.token.clone().unwrap();
    let token_clone = token.clone();
    let token_hash = compute_sha_hash(&token);

    let mut tx = pool
        .begin()
        .await
        .context("Failed to begin transaction")
        .map_err(e500)?;

    let now = chrono::Utc::now();

    let token_db = sqlx::query!(
        r#"
        SELECT id, user_id
        FROM verification_token
        WHERE token_hash = $1
            AND kind = 'email_verification'
            AND expires_at > $2
            AND used_at IS NULL
        FOR UPDATE
        "#,
        token_hash,
        now
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_err)?;

    let token = if let Some(token) = token_db {
        token
    } else {
        return Ok(HttpResponse::BadRequest().json(
            ApiErrorResponse::from_single_validation_error(
                "token",
                "invalid_or_expired_token",
                Some(error_message),
                vec![("token", &token_clone)],
            ),
        ));
    };

    if token.user_id != session.user_id {
        return Ok(HttpResponse::BadRequest().json(
            ApiErrorResponse::from_single_validation_error(
                "token",
                "invalid_or_expired_token",
                Some(error_message),
                vec![("token", &token_clone)],
            ),
        ));
    }

    sqlx::query!(
        r#"
        UPDATE auth_user SET is_email_verified = true WHERE id = $1
        "#,
        token.user_id
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;

    sqlx::query!(
        r#"
        UPDATE verification_token
        SET used_at = $1
        WHERE id = $2
        "#,
        now,
        token.id
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;

    tx.commit()
        .await
        .context("Failed to commit transaction")
        .map_err(e500)?;

    Ok(HttpResponse::NoContent().finish())
}

#[derive(Deserialize, Serialize, Debug, Validate, ToSchema)]
pub struct VerifyEmailPayload {
    #[validate(
        length(min = 1, message = "O token não pode estar vazio"),
        required(message = "O token é obrigatório")
    )]
    #[schema(example = "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6")]
    pub token: Option<String>,
}
