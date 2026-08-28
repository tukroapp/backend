pub mod request;
pub mod top;

use actix_web::{
    HttpResponse, Responder,
    web::{Data, Path},
};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    routes::profile::slug::rating::delete_ratings_between,
    services::auth::SessionData,
    utils::{ApiErrorResponse, db_err},
};

/// Resolve o slug para o id do usuário, ignorando contas inativas.
pub async fn resolve_slug(pool: &PgPool, slug: &str) -> Result<Option<Uuid>, actix_web::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT up.user_id
        FROM user_profile up
        INNER JOIN auth_user au ON au.id = up.user_id
        WHERE up.slug = $1 AND au.status = 'active'
        "#,
        slug,
    )
    .fetch_optional(pool)
    .await
    .map_err(db_err)
}

pub fn user_not_found() -> HttpResponse {
    HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
        "profile",
        "not_found",
        Some("Perfil não encontrado."),
        vec![],
    ))
}

#[utoipa::path(
    delete,
    path = "/api/v1/friend/{slug}",
    description = "Desfaz a amizade com o usuário do slug informado, removendo os dois lados da relação",
    params(("slug" = String, Path, description = "Slug público do amigo")),
    responses(
        (status = 204, description = "Amizade desfeita"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Perfil não encontrado ou vocês não são amigos", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Desfaz uma amizade", skip_all)]
pub async fn delete(
    slug: Path<String>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(friend_id) = resolve_slug(pool.get_ref(), &slug.into_inner()).await? else {
        return Ok(user_not_found());
    };

    let mut tx = pool.begin().await.map_err(db_err)?;

    let removed = sqlx::query!(
        r#"
        DELETE FROM friend
        WHERE (user_id = $1 AND friend_id = $2)
           OR (user_id = $2 AND friend_id = $1)
        "#,
        session.user_id,
        friend_id,
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?
    .rows_affected();

    if removed == 0 {
        return Ok(
            HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
                "friend",
                "not_friends",
                Some("Vocês não são amigos."),
                vec![],
            )),
        );
    }

    delete_ratings_between(&mut tx, session.user_id, friend_id).await?;

    tx.commit().await.map_err(db_err)?;

    Ok(HttpResponse::NoContent().finish())
}
