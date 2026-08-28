use actix_web::{HttpResponse, Responder, web::Data};
use askama::Template;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    services::{auth::SessionData, email::EmailClient},
    startup::AppState,
    utils::{ApiErrorResponse, compute_sha_hash, db_err, e500},
};

#[utoipa::path(
    post,
    path = "/api/v1/auth/send-verification-email",
    description = "Envia um e-mail de verificação para o usuário autenticado",
    responses(
        (status = 204, description = "E-mail de verificação enviado com sucesso"),
        (status = 400, description = "Requisição inválida", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Envia e-mail de verificação", skip_all)]
pub async fn post(
    app_state: Data<AppState>,
    pool: Data<PgPool>,
    session: SessionData,
) -> Result<impl Responder, actix_web::Error> {
    let user = sqlx::query!(
        r#"
        SELECT
            au.is_email_verified,
            up.name,
            au.email AS "email!: String"
        FROM auth_user au
        INNER JOIN user_profile up ON up.user_id = au.id
        WHERE au.id = $1
        "#,
        session.user_id
    )
    .fetch_one(pool.get_ref())
    .await
    .map_err(db_err)?;

    if user.is_email_verified {
        return Ok(HttpResponse::BadRequest().json(
            ApiErrorResponse::from_single_validation_error(
                "email",
                "already_verified",
                Some("O e-mail já foi verificado."),
                vec![("email", &user.email)],
            ),
        ));
    }

    queue_verification_email(
        pool.get_ref(),
        &app_state.frontend_url,
        session.user_id,
        &user.email,
        &user.name,
    )
    .await?;

    Ok(HttpResponse::NoContent().finish())
}

/// Gera o token e enfileira o e-mail de confirmação. Fica separado do handler
/// porque o cadastro também dispara isso, sem passar por aqui.
pub async fn queue_verification_email(
    pool: &PgPool,
    frontend_url: &str,
    user_id: Uuid,
    email: &str,
    name: &str,
) -> Result<(), actix_web::Error> {
    let token = Uuid::new_v4();
    let token_hash = compute_sha_hash(&token.to_string());

    sqlx::query!(
        r#"
        INSERT INTO verification_token (user_id, token_hash, kind, expires_at)
        VALUES ($1, $2, 'email_verification', NOW() + INTERVAL '30 minutes')
        "#,
        user_id,
        token_hash,
    )
    .execute(pool)
    .await
    .map_err(db_err)?;

    EmailClient::queue_email_single(
        pool,
        email,
        "Confirmação de e-mail",
        AccountConfirmationEmailTemplate {
            name,
            confirmation_token: &token.to_string(),
            frontend_url,
        },
    )
    .await
    .map_err(e500)?;

    Ok(())
}

#[derive(Template)]
#[template(path = "email/account_confirmation.html")]
pub struct AccountConfirmationEmailTemplate<'a> {
    pub name: &'a str,
    pub confirmation_token: &'a str,
    pub frontend_url: &'a str,
}
