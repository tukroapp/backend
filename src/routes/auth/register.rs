use actix_web::{
    HttpRequest, HttpResponse, Responder,
    web::{Data, Json},
};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use validator::{Validate, ValidationError};

use crate::{
    constants::PASSWORD_MIN_LENGTH,
    routes::auth::send_verification_email::queue_verification_email,
    services::{auth::SessionMeta, slug::create_user_profile, terms::record_acceptance},
    startup::AppState,
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
    app_state: Data<AppState>,
    request: HttpRequest,
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

    record_acceptance(&mut tx, &user_id, &SessionMeta::from_http_request(&request))
        .await
        .map_err(db_err)?;

    tx.commit().await.map_err(db_err)?;

    // A conta já existe; falhar aqui só custaria o e-mail automático, e o
    // usuário ainda pode pedir o reenvio.
    if let Err(error) = queue_verification_email(
        pool.get_ref(),
        &app_state.from_email,
        &app_state.frontend_url,
        user_id,
        &payload.email,
        &payload.name,
    )
    .await
    {
        tracing::error!(%error, "failed to queue the verification email on register");
    }

    Ok(HttpResponse::Created().json(ApiResponse::new(RegisterResponse { slug })))
}

#[derive(Deserialize, Validate, ToSchema)]
pub struct RegisterRequest {
    #[validate(email(message = "E-mail inválido"))]
    #[schema(example = "novo@example.com")]
    pub email: String,

    #[validate(length(min = PASSWORD_MIN_LENGTH, message = "A senha deve ter pelo menos 8 caracteres"))]
    #[schema(example = "minhasenhasupersegura")]
    pub password: String,

    #[validate(length(min = 1, max = 80, message = "O nome deve ter entre 1 e 80 caracteres"))]
    #[schema(example = "Fulano de Tal")]
    pub name: String,

    /// Aceite dos termos e da política de privacidade. Só `true` cadastra, e o
    /// campo é obrigatório de propósito: ausente é recusa, não omissão.
    #[validate(custom(function = "must_accept"))]
    #[schema(example = true)]
    pub accepted_terms: bool,
}

fn must_accept(accepted: &bool) -> Result<(), ValidationError> {
    if *accepted {
        return Ok(());
    }

    let mut error = ValidationError::new("terms_not_accepted");
    error.message = Some("É preciso aceitar os termos de uso e a política de privacidade.".into());

    Err(error)
}
