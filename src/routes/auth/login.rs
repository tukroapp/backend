use actix_web::{
    HttpRequest, HttpResponse, Responder,
    error::InternalError,
    web::{Data, Json},
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use utoipa::ToSchema;

use crate::{
    services::auth::{LoginCredentials, LoginError, LoginType, SessionMeta, login},
    startup::RedisPool,
    utils::{ApiErrorResponse, ApiResponse},
};

#[derive(Serialize, ToSchema)]
pub struct LoginResponse {
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

#[utoipa::path(
    post,
    path = "/api/v1/auth/login",
    description = "Realiza o login do usuário com e-mail e senha, retornando o token de sessão",
    request_body = LoginCredentials,
    responses(
        (status = 200, description = "Login feito com sucesso, token de sessão retornado", body = ApiResponse<LoginResponse>),
        (status = 401, description = "Credenciais inválidas", body = ApiErrorResponse),
        (status = 403, description = "Conta suspensa", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Realiza o login do usuário", skip_all)]
pub async fn post(
    pool: Data<PgPool>,
    redis: Data<RedisPool>,
    request: HttpRequest,
    credentials: Json<LoginCredentials>,
) -> Result<impl Responder, actix_web::Error> {
    let meta = SessionMeta::from_http_request(&request);

    let session = login(
        &pool,
        &redis,
        LoginType::Standard(credentials.into_inner()),
        meta,
    )
    .await
    .map_err(|e| {
        // Conta suspensa responde com o próprio 403: quem já provou a senha
        // precisa saber por que não entra. O resto vira o 401 genérico, que
        // não revela se o e-mail existe.
        let LoginError::Other(e) = e else {
            return actix_web::Error::from(e);
        };

        InternalError::from_response(
            e,
            HttpResponse::Unauthorized().json(ApiErrorResponse::from_single_validation_error(
                "credentials",
                "invalid_credentials",
                Some("Credenciais inválidas fornecidas."),
                vec![],
            )),
        )
        .into()
    })?;

    Ok(HttpResponse::Ok().json(ApiResponse::new(LoginResponse {
        token: session.token,
        expires_at: session.expires_at,
    })))
}
