use actix_web::{
    HttpResponse, Responder,
    web::{Data, Json},
};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use validator::Validate;

use crate::{
    services::slug::create_user_profile,
    telemetry::spawn_blocking_with_tracing,
    utils::{ApiErrorResponse, ApiResponse, compute_hash, db_err, e500},
};

#[derive(Serialize, ToSchema)]
pub struct RegisterResponse {
    pub slug: String,
}

#[utoipa::path(
    post,
    path = "/api/v1/auth/register",
    description = "Cadastra um novo usuário por e-mail, criando auth_user e user_profile na mesma transação",
    request_body = RegisterRequest,
    responses(
        (status = 201, description = "Usuário criado", body = ApiResponse<RegisterResponse>),
        (status = 400, description = "Dados inválidos", body = ApiErrorResponse),
        (status = 409, description = "E-mail já cadastrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Cadastra um novo usuário", skip_all)]
pub async fn post(
    pool: Data<PgPool>,
    payload: Json<RegisterRequest>,
) -> Result<impl Responder, actix_web::Error> {
    if let Err(e) = payload.validate() {
        return Ok(HttpResponse::BadRequest().json(ApiErrorResponse::from(e)));
    }

    let payload = payload.into_inner();

    let password_hash = spawn_blocking_with_tracing(move || compute_hash(payload.password))
        .await
        .context("Failed to compute password hash")
        .map_err(e500)?
        .map_err(e500)?;

    let mut tx = pool.begin().await.map_err(db_err)?;

    let created = sqlx::query!(
        r#"
        INSERT INTO auth_user (email, password_hash)
        VALUES ($1, $2)
        RETURNING id
        "#,
        payload.email,
        password_hash,
    )
    .fetch_one(&mut *tx)
    .await;

    let user_id = match created {
        Ok(row) => row.id,
        Err(sqlx::Error::Database(db)) if db.code().as_deref() == Some("23505") => {
            return Ok(HttpResponse::Conflict().json(
                ApiErrorResponse::from_single_validation_error(
                    "email",
                    "email_taken",
                    Some("Este e-mail já está cadastrado."),
                    vec![("email", &payload.email)],
                ),
            ));
        }
        Err(e) => return Err(db_err(e)),
    };

    let slug = create_user_profile(&mut tx, &user_id, &payload.name).await?;

    tx.commit().await.map_err(db_err)?;

    Ok(HttpResponse::Created().json(ApiResponse::new(RegisterResponse { slug })))
}

#[derive(Deserialize, Validate, ToSchema)]
pub struct RegisterRequest {
    #[validate(email(message = "E-mail inválido"))]
    #[schema(example = "novo@example.com")]
    pub email: String,

    #[validate(length(min = 8, message = "A senha deve ter pelo menos 8 caracteres"))]
    #[schema(example = "minhasenhasupersegura")]
    pub password: String,

    #[validate(length(min = 1, max = 80, message = "O nome deve ter entre 1 e 80 caracteres"))]
    #[schema(example = "Fulano de Tal")]
    pub name: String,
}
