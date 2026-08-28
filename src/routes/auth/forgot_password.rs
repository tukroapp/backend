use actix_web::{
    HttpResponse, Responder,
    web::{Data, Json},
};
use askama::Template;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

use crate::{
    services::email::EmailClient,
    startup::AppState,
    utils::{ApiErrorResponse, compute_sha_hash, db_err, e500},
};

#[utoipa::path(
    post,
    path = "/api/v1/auth/forgot-password",
    description = "Envia um e-mail de redefinição de senha para o usuário",
    request_body = ForgotPasswordPayload,
    responses(
        (status = 204, description = "E-mail de redefinição de senha enviado com sucesso"),
        (status = 400, description = "Requisição inválida", body = ApiErrorResponse)
    ),
)]
#[tracing::instrument(name = "Envia e-mail de redefinição de senha", skip_all)]
pub async fn post(
    payload: Json<ForgotPasswordPayload>,
    pool: Data<PgPool>,
    app_state: Data<AppState>,
) -> Result<impl Responder, actix_web::Error> {
    match payload.validate() {
        Ok(_) => {}
        Err(e) => {
            return Ok(HttpResponse::BadRequest().json(ApiErrorResponse::from(e)));
        }
    };

    let email = payload.email.as_ref().unwrap();

    let user_db = sqlx::query!(
        r#"SELECT
            au.id,
            up.name
        FROM auth_user au
        INNER JOIN user_profile up ON up.user_id = au.id
        WHERE
            au.email = $1
            AND au.status <> 'suspended'
            AND au.google_id IS NULL
        "#,
        email
    )
    .fetch_optional(pool.get_ref())
    .await
    .map_err(db_err)?;

    let token = Uuid::new_v4().to_string();
    let token_hash = compute_sha_hash(&token);

    if let Some(user) = user_db {
        sqlx::query!(
            r#"
            INSERT INTO verification_token
                (user_id, token_hash, kind, expires_at)
            VALUES
                ($1, $2, 'password_reset', NOW() + INTERVAL '15 minutes')
            "#,
            user.id,
            token_hash,
        )
        .execute(pool.get_ref())
        .await
        .map_err(db_err)?;

        EmailClient::queue_email_single(
            &pool,
            &app_state.from_email,
            &email.clone(),
            "Redefinição de senha",
            ForgotPasswordTemplate {
                token: &token,
                name: &user.name,
                frontend_url: &app_state.frontend_url,
            },
        )
        .await
        .map_err(e500)?;
    }

    Ok(HttpResponse::NoContent().finish())
}

#[derive(Template)]
#[template(path = "email/reset_token.html")]
pub struct ForgotPasswordTemplate<'a> {
    pub name: &'a str,
    pub token: &'a str,
    pub frontend_url: &'a str,
}

#[derive(Deserialize, Serialize, Validate, ToSchema)]
pub struct ForgotPasswordPayload {
    #[validate(
        required(message = "O e-mail é obrigatório"),
        email(message = "Formato de e-mail inválido")
    )]
    #[schema(example = "test@example.com")]
    pub email: Option<String>,
}
