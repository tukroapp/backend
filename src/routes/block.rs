use actix_web::{
    HttpResponse, Responder,
    web::{Data, Path, Query},
};
use sqlx::PgPool;
use std::sync::Arc;

use crate::{
    routes::{
        friend::{resolve_slug, user_not_found},
        profile::{ProfileSummary, slug::rating::delete_ratings_between},
    },
    services::{
        auth::SessionData,
        storage::{StorageService, public_url_of},
    },
    utils::{
        ApiCursorPaginatedResponse, ApiErrorResponse, CursorPaginationQuery, db_err, take_page,
    },
};

#[utoipa::path(
    get,
    path = "/api/v1/block",
    description = "Lista paginada (cursor) dos perfis bloqueados pelo usuário autenticado",
    params(CursorPaginationQuery),
    responses(
        (status = 200, description = "Perfis bloqueados", body = ApiCursorPaginatedResponse<Vec<ProfileSummary>>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Lista os perfis bloqueados", skip_all)]
pub async fn get(
    pagination: Query<CursorPaginationQuery>,
    session: SessionData,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let limit = pagination.limit();

    let mut rows = sqlx::query_as!(
        ProfileSummary,
        r#"
        SELECT
            up.user_id AS id,
            up.slug,
            up.name,
            up.avatar_url,
            up.status
        FROM block b
        INNER JOIN user_profile up ON up.user_id = b.blocked_id
        WHERE b.blocker_id = $1
          AND ($2::uuid IS NULL OR b.blocked_id < $2::uuid)
        ORDER BY b.blocked_id DESC
        LIMIT $3
        "#,
        session.user_id,
        pagination.cursor_id(),
        limit + 1,
    )
    .fetch_all(pool.get_ref())
    .await
    .map_err(db_err)?;

    for row in &mut rows {
        row.avatar_url = public_url_of(storage.as_ref().as_ref(), row.avatar_url.take());
    }

    let next_cursor = take_page(&mut rows, limit, |row| row.id.to_string());

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(rows, next_cursor)))
}

#[utoipa::path(
    post,
    path = "/api/v1/block/{slug}",
    description = "Bloqueia o usuário do slug informado, desfazendo a amizade, o fã e removendo pedidos pendentes entre os dois",
    params(("slug" = String, Path, description = "Slug público do perfil a bloquear")),
    responses(
        (status = 204, description = "Perfil bloqueado"),
        (status = 400, description = "Bloqueio de si mesmo", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Perfil não encontrado ou conta inativa", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Bloqueia um perfil", skip_all)]
pub async fn post(
    slug: Path<String>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(blocked_id) = resolve_slug(pool.get_ref(), &slug.into_inner()).await? else {
        return Ok(user_not_found());
    };

    if blocked_id == session.user_id {
        return Ok(HttpResponse::BadRequest().json(
            ApiErrorResponse::from_single_validation_error(
                "block",
                "self_block",
                Some("Você não pode bloquear a si mesmo."),
                vec![],
            ),
        ));
    }

    let mut tx = pool.begin().await.map_err(db_err)?;

    sqlx::query!(
        r#"
        INSERT INTO block (blocker_id, blocked_id)
        VALUES ($1, $2)
        ON CONFLICT DO NOTHING
        "#,
        session.user_id,
        blocked_id,
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;

    sqlx::query!(
        r#"
        DELETE FROM friend
        WHERE (user_id = $1 AND friend_id = $2)
           OR (user_id = $2 AND friend_id = $1)
        "#,
        session.user_id,
        blocked_id,
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;

    sqlx::query!(
        r#"
        DELETE FROM friend_request
        WHERE status = 'pending'
          AND ((requester_id = $1 AND addressee_id = $2)
            OR (requester_id = $2 AND addressee_id = $1))
        "#,
        session.user_id,
        blocked_id,
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;

    sqlx::query!(
        r#"
        DELETE FROM fan
        WHERE (fan_id = $1 AND idol_id = $2)
           OR (fan_id = $2 AND idol_id = $1)
        "#,
        session.user_id,
        blocked_id,
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;

    delete_ratings_between(&mut tx, session.user_id, blocked_id).await?;

    tx.commit().await.map_err(db_err)?;

    Ok(HttpResponse::NoContent().finish())
}

#[utoipa::path(
    delete,
    path = "/api/v1/block/{slug}",
    description = "Desfaz o bloqueio do usuário do slug informado",
    params(("slug" = String, Path, description = "Slug público do perfil bloqueado")),
    responses(
        (status = 204, description = "Bloqueio desfeito"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Perfil não encontrado ou não estava bloqueado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Desfaz o bloqueio de um perfil", skip_all)]
pub async fn delete(
    slug: Path<String>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(blocked_id) = resolve_slug(pool.get_ref(), &slug.into_inner()).await? else {
        return Ok(user_not_found());
    };

    let removed = sqlx::query!(
        r#"
        DELETE FROM block
        WHERE blocker_id = $1 AND blocked_id = $2
        "#,
        session.user_id,
        blocked_id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?
    .rows_affected();

    if removed == 0 {
        return Ok(
            HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
                "block",
                "not_blocked",
                Some("Esse perfil não está bloqueado."),
                vec![],
            )),
        );
    }

    Ok(HttpResponse::NoContent().finish())
}
