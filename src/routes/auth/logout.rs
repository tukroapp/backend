use actix_web::{HttpResponse, Responder, web::Data};

use crate::{
    services::auth::{SessionData, delete_session},
    startup::RedisPool,
};

#[utoipa::path(
    post,
    path = "/api/v1/auth/logout",
    description = "Desloga o usuário, removendo a sessão do Redis",
    responses(
        (status = 204, description = "Logout feito com sucesso"),
    ),
)]
#[tracing::instrument(name = "Deslogar o usuário", skip_all)]
pub async fn post(
    redis: Data<RedisPool>,
    session: SessionData,
) -> Result<impl Responder, actix_web::Error> {
    delete_session(&redis, &session.user_id, &session.hash).await?;

    Ok(HttpResponse::NoContent().finish())
}
