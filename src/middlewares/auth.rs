use actix_web::{
    Error, HttpMessage, HttpResponse,
    body::MessageBody,
    dev::{ServiceRequest, ServiceResponse},
    error::InternalError,
    http::header::AUTHORIZATION,
    middleware::Next,
    web::Data,
};

use sqlx::PgPool;

use crate::{
    services::auth::{SessionData, touch_session},
    startup::RedisPool,
    utils::{ApiErrorResponse, compute_sha_hash, db_err},
};

fn unauthorized() -> Error {
    InternalError::from_response(
        "Authentication required",
        HttpResponse::Unauthorized().json(ApiErrorResponse::from_single_validation_error(
            "authentication",
            "authentication_required",
            Some("Autenticação necessária"),
            vec![],
        )),
    )
    .into()
}

fn extract_token(req: &ServiceRequest) -> Option<String> {
    req.headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Token "))
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

#[tracing::instrument(name = "Autenticação de requisição", skip_all)]
pub async fn needs_authentication(
    req: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<impl MessageBody>, Error> {
    let token = extract_token(&req).ok_or_else(unauthorized)?;

    let redis = req.app_data::<Data<RedisPool>>().unwrap();

    let token_hash = compute_sha_hash(&token);

    match touch_session(redis, &token_hash).await? {
        Some(user_id) => {
            let session_data = SessionData {
                hash: token_hash,
                user_id,
            };

            req.extensions_mut().insert(session_data);
            next.call(req).await
        }
        None => Err(unauthorized()),
    }
}

fn forbidden() -> Error {
    InternalError::from_response(
        "Superuser required",
        HttpResponse::Forbidden().json(ApiErrorResponse::from_single_validation_error(
            "authorization",
            "superuser_required",
            Some("Acesso restrito à administração."),
            vec![],
        )),
    )
    .into()
}

/// Exige que a sessão seja de um superusuário. Precisa vir por dentro de
/// `needs_authentication`, que é quem popula a sessão.
#[tracing::instrument(name = "Autorização de administrador", skip_all)]
pub async fn needs_superuser(
    req: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<impl MessageBody>, Error> {
    let user_id = req
        .extensions()
        .get::<SessionData>()
        .map(|session| session.user_id)
        .ok_or_else(unauthorized)?;

    let pool = req.app_data::<Data<PgPool>>().unwrap();

    let is_superuser = sqlx::query_scalar!(
        r#"SELECT is_superuser FROM auth_user WHERE id = $1 AND status = 'active'"#,
        user_id,
    )
    .fetch_optional(pool.get_ref())
    .await
    .map_err(db_err)?
    .unwrap_or(false);

    if !is_superuser {
        return Err(forbidden());
    }

    next.call(req).await
}

/// Popula a sessão quando um token válido é enviado, mas segue adiante sem ela.
/// Usado em endpoints públicos cuja resposta muda conforme quem está olhando.
#[tracing::instrument(name = "Autenticação opcional de requisição", skip_all)]
pub async fn optional_authentication(
    req: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<impl MessageBody>, Error> {
    if let Some(token) = extract_token(&req) {
        let redis = req.app_data::<Data<RedisPool>>().unwrap();
        let token_hash = compute_sha_hash(&token);

        if let Some(user_id) = touch_session(redis, &token_hash).await? {
            req.extensions_mut().insert(SessionData {
                hash: token_hash,
                user_id,
            });
        }
    }

    next.call(req).await
}
