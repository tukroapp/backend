use actix_web::{
    HttpResponse, Responder,
    web::{Data, Json},
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;

use crate::{
    services::auth::{LoginCredentials, SessionData, delete_all_sessions, validate_credentials},
    startup::RedisPool,
    utils::{ApiErrorResponse, db_err},
};

#[utoipa::path(
    post,
    path = "/api/v1/auth/logout-all",
    description = "Desloga o usuário de todos os dispositivos, incluindo a sessão atual. Exige a senha atual para confirmação",
    request_body = LogoutAllRequest,
    responses(
        (status = 204, description = "Todas as sessões foram revogadas"),
        (status = 400, description = "Senha incorreta", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Deslogar o usuário de todos os dispositivos", skip_all)]
pub async fn post(
    session: SessionData,
    payload: Json<LogoutAllRequest>,
    pool: Data<PgPool>,
    redis: Data<RedisPool>,
) -> Result<impl Responder, actix_web::Error> {
    let mut tx = pool.begin().await.map_err(db_err)?;

    let user = sqlx::query!(
        r#"
        SELECT email
        FROM auth_user
        WHERE id = $1
        "#,
        session.user_id
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(db_err)?;

    let credentials = LoginCredentials {
        email: user.email,
        password: payload.password.clone(),
    };

    if validate_credentials(credentials, &mut tx).await.is_err() {
        return Ok(HttpResponse::BadRequest().json(
            ApiErrorResponse::from_single_validation_error(
                "password",
                "invalid",
                Some("Senha incorreta"),
                vec![],
            ),
        ));
    }

    delete_all_sessions(&redis, &session.user_id).await?;

    Ok(HttpResponse::NoContent().finish())
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct LogoutAllRequest {
    #[schema(example = "minhasenhaatual123")]
    pub password: String,
}
