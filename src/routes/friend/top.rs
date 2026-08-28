use actix_web::{
    HttpResponse, Responder,
    web::{Data, Json},
};
use serde::Deserialize;
use sqlx::PgPool;
use utoipa::ToSchema;
use validator::Validate;

use crate::{
    services::auth::SessionData,
    utils::{ApiErrorResponse, ApiResponse, db_err, invalid_field},
};

/// A grade 3x3 do Orkut. O banco também limita, no check de `top_position`.
const MAX_TOP_FRIENDS: usize = 9;

#[derive(Debug, Deserialize, ToSchema, Validate)]
pub struct TopFriendsRequest {
    /// Slugs dos amigos destacados, na ordem em que devem aparecer. Lista vazia
    /// desfaz o destaque de todo mundo.
    #[validate(length(max = 9, message = "No máximo 9 amigos em destaque"))]
    pub slugs: Vec<String>,
}

#[derive(serde::Serialize, ToSchema)]
pub struct TopFriendsResponse {
    /// Quantos amigos ficaram destacados.
    pub count: usize,
}

#[utoipa::path(
    put,
    path = "/api/v1/friend/top",
    description = "Define os amigos em destaque do próprio perfil, na ordem informada. Substitui a seleção anterior inteira",
    request_body = TopFriendsRequest,
    responses(
        (status = 200, description = "Destaque atualizado", body = ApiResponse<TopFriendsResponse>),
        (status = 400, description = "Mais de 9 amigos, slug repetido ou alguém que não é seu amigo", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Define os amigos em destaque", skip_all)]
pub async fn put(
    payload: Json<TopFriendsRequest>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    if let Err(errors) = payload.validate() {
        return Ok(HttpResponse::BadRequest().json(ApiErrorResponse::from(errors)));
    }

    let slugs = &payload.slugs;

    let mut seen = slugs.clone();
    seen.sort();
    seen.dedup();
    if seen.len() != slugs.len() {
        return Ok(invalid_field(
            "slugs",
            "duplicated",
            "Cada amigo só pode aparecer uma vez no destaque.",
        ));
    }

    let mut tx = pool.begin().await.map_err(db_err)?;

    sqlx::query!(
        r#"UPDATE friend SET top_position = NULL WHERE user_id = $1 AND top_position IS NOT NULL"#,
        session.user_id,
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;

    // A ordem da lista é a posição, então a atualização vai uma a uma: são no
    // máximo nove, e um único UPDATE com CASE seria bem menos legível.
    for (index, slug) in slugs.iter().take(MAX_TOP_FRIENDS).enumerate() {
        let position = (index + 1) as i16;

        let updated = sqlx::query!(
            r#"
            UPDATE friend
            SET top_position = $3
            WHERE user_id = $1
              AND friend_id = (
                  SELECT up.user_id
                  FROM user_profile up
                  INNER JOIN auth_user au ON au.id = up.user_id AND au.status = 'active'
                  WHERE up.slug = $2
              )
            "#,
            session.user_id,
            slug,
            position,
        )
        .execute(&mut *tx)
        .await
        .map_err(db_err)?
        .rows_affected();

        if updated == 0 {
            return Ok(HttpResponse::BadRequest().json(
                ApiErrorResponse::from_single_validation_error(
                    "slugs",
                    "not_a_friend",
                    Some("Só dá para destacar quem já é seu amigo."),
                    vec![("slug", slug)],
                ),
            ));
        }
    }

    tx.commit().await.map_err(db_err)?;

    Ok(HttpResponse::Ok().json(ApiResponse::new(TopFriendsResponse { count: slugs.len() })))
}
