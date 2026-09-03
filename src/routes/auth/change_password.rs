use actix_web::{
    HttpResponse, Responder,
    web::{Data, Json},
};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use validator::{Validate, ValidationError, ValidationErrors};

use crate::{
    constants::PASSWORD_MIN_LENGTH,
    services::auth::{LoginCredentials, SessionData, delete_other_sessions, validate_credentials},
    startup::RedisPool,
    telemetry::spawn_blocking_with_tracing,
    utils::{ApiErrorResponse, compute_hash, db_err, e500},
};

#[utoipa::path(
    post,
    path = "/api/v1/auth/change-password",
    description = "Alterar a senha do usuário autenticado informando a senha antiga",
    request_body = ChangePasswordRequest,
    responses(
        (status = 204, description = "Senha alterada com sucesso"),
        (status = 400, description = "Requisição inválida", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Alterar senha do usuário informando a senha antiga", skip_all)]
pub async fn post(
    session: SessionData,
    payload: Json<ChangePasswordRequest>,
    pool: Data<PgPool>,
    redis: Data<RedisPool>,
) -> Result<impl Responder, actix_web::Error> {
    let mut errors = ValidationErrors::new();

    match payload.validate() {
        Ok(_) => {}
        Err(e) => {
            errors = e;
        }
    }
    let mut tx = pool.begin().await.map_err(db_err)?;

    let user = sqlx::query!(
        r#"
        SELECT email, password_hash
        FROM auth_user
        WHERE id = $1
        FOR UPDATE
        "#,
        &session.user_id
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(db_err)?;

    // Conta do Google nunca tem senha: definir uma aqui a trancaria para fora do
    // próprio Google, que recusa login quando encontra `password_hash`.
    if user.password_hash.is_none() {
        return Ok(HttpResponse::BadRequest().json(
            ApiErrorResponse::from_single_validation_error(
                "account",
                "google_account",
                Some("Esta conta entra pelo Google e não usa senha."),
                vec![],
            ),
        ));
    }

    let credentials = LoginCredentials {
        email: user.email,
        password: payload.old_password.clone(),
    };

    match validate_credentials(credentials, &mut tx).await {
        Ok(_) => {}
        Err(_) => {
            let mut error = ValidationError::new("invalid");
            error.message = Some("A senha antiga está incorreta".into());
            errors.add("old_password", error);
        }
    }

    if !errors.is_empty() {
        return Ok(HttpResponse::BadRequest().json(ApiErrorResponse::from(errors)));
    }

    let new_password_hash =
        spawn_blocking_with_tracing(move || compute_hash(payload.new_password.clone()))
            .await
            .unwrap()
            .context("Failed to compute password hash")
            .map_err(e500)?;

    sqlx::query!(
        r#"
        UPDATE auth_user
        SET password_hash = $1
        WHERE id = $2
        "#,
        new_password_hash,
        session.user_id
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;

    tx.commit().await.map_err(db_err)?;

    delete_other_sessions(&redis, &session.user_id, &session.hash).await?;

    Ok(HttpResponse::NoContent().finish())
}

#[derive(Serialize, Deserialize, Debug, Validate, ToSchema)]
pub struct ChangePasswordRequest {
    #[schema(example = "minhasenhaantiga123")]
    pub old_password: String,
    #[validate(
        length(min = PASSWORD_MIN_LENGTH, message = "A senha deve ter pelo menos 8 caracteres"),
        must_match(
            other = "new_password_confirmation",
            message = "As senhas não coincidem"
        )
    )]
    #[schema(example = "minhasenhanova123")]
    pub new_password: String,
    #[schema(example = "minhasenhanova123")]
    pub new_password_confirmation: String,
}
