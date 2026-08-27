use actix_web::{
    HttpResponse, Responder,
    web::{Data, Path, Query},
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use std::sync::Arc;

use crate::{
    routes::{
        community::{
            is_at_community_limit, member_community_limit_reached,
            slug::{community_not_found, fetch_community_target, moderator_required},
        },
        profile::ProfileSummary,
    },
    services::{
        auth::SessionData,
        notification::{NotificationKind, NotificationTarget, notify},
        storage::{StorageService, public_url_of},
    },
    utils::{
        ApiCursorPaginatedResponse, ApiErrorResponse, CursorPaginationQuery, db_err, take_page,
        unauthorized,
    },
};

#[derive(Serialize, ToSchema)]
pub struct JoinRequestResponse {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub profile: ProfileSummary,
}

fn request_not_found() -> HttpResponse {
    HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
        "join_request",
        "not_found",
        Some("Pedido de entrada não encontrado."),
        vec![],
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/community/{slug}/join-request",
    description = "Lista paginada (cursor) dos pedidos de entrada pendentes da comunidade",
    params(
        ("slug" = String, Path, description = "Slug público da comunidade"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Pedidos pendentes", body = ApiCursorPaginatedResponse<Vec<JoinRequestResponse>>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Apenas o dono e os moderadores veem os pedidos", body = ApiErrorResponse),
        (status = 404, description = "Comunidade não encontrada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Lista os pedidos de entrada da comunidade", skip_all)]
pub async fn get(
    slug: Path<String>,
    pagination: Query<CursorPaginationQuery>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    let Some(target) =
        fetch_community_target(pool.get_ref(), &slug.into_inner(), Some(session.user_id)).await?
    else {
        return Ok(community_not_found());
    };

    if !target.can_moderate() {
        return Ok(moderator_required());
    }

    let limit = pagination.limit();

    let rows = sqlx::query!(
        r#"
        SELECT
            r.id,
            r.created_at,
            up.user_id AS profile_id,
            up.slug,
            up.name,
            up.avatar_url,
            up.status
        FROM community_join_request r
        INNER JOIN auth_user au ON au.id = r.user_id AND au.status = 'active'
        INNER JOIN user_profile up ON up.user_id = r.user_id
        WHERE r.community_id = $1
          AND r.status = 'pending'
          AND ($2::uuid IS NULL OR r.id < $2::uuid)
        ORDER BY r.id DESC
        LIMIT $3
        "#,
        target.id,
        pagination.cursor_id(),
        limit + 1,
    )
    .fetch_all(pool.get_ref())
    .await
    .map_err(db_err)?;

    let mut results = rows
        .into_iter()
        .map(|row| JoinRequestResponse {
            id: row.id,
            created_at: row.created_at,
            profile: ProfileSummary {
                id: row.profile_id,
                slug: row.slug,
                name: row.name,
                avatar_url: public_url_of(storage.as_ref().as_ref(), row.avatar_url),
                status: row.status,
            },
        })
        .collect::<Vec<_>>();

    let next_cursor = take_page(&mut results, limit, |request| request.id.to_string());

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(results, next_cursor)))
}

#[utoipa::path(
    post,
    path = "/api/v1/community/{slug}/join-request/{id}/accept",
    description = "Aceita um pedido de entrada e adiciona o solicitante como membro",
    params(
        ("slug" = String, Path, description = "Slug público da comunidade"),
        ("id" = Uuid, Path, description = "Id do pedido"),
    ),
    responses(
        (status = 204, description = "Pedido aceito"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Apenas o dono e os moderadores respondem", body = ApiErrorResponse),
        (status = 404, description = "Comunidade ou pedido não encontrado", body = ApiErrorResponse),
        (status = 409, description = "O solicitante atingiu o limite de comunidades", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Aceita um pedido de entrada", skip_all)]
pub async fn accept(
    path: Path<(String, Uuid)>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    let (slug, request_id) = path.into_inner();

    let Some(target) = fetch_community_target(pool.get_ref(), &slug, Some(session.user_id)).await?
    else {
        return Ok(community_not_found());
    };

    if !target.can_moderate() {
        return Ok(moderator_required());
    }

    let mut tx = pool.begin().await.map_err(db_err)?;

    let responded = sqlx::query_scalar!(
        r#"
        UPDATE community_join_request
        SET status = 'accepted', responded_at = NOW()
        WHERE id = $1 AND community_id = $2 AND status = 'pending'
        RETURNING user_id
        "#,
        request_id,
        target.id,
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_err)?;

    let Some(new_member_id) = responded else {
        return Ok(request_not_found());
    };

    // O solicitante pode ter enchido a cota entre o pedido e a aprovação. O
    // rollback devolve o pedido para pendente, para ser respondido de novo.
    if is_at_community_limit(&mut *tx, new_member_id).await? {
        tx.rollback().await.map_err(db_err)?;
        return Ok(member_community_limit_reached());
    }

    sqlx::query!(
        r#"
        INSERT INTO community_member (community_id, user_id)
        VALUES ($1, $2)
        ON CONFLICT DO NOTHING
        "#,
        target.id,
        new_member_id,
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;

    notify(
        &mut *tx,
        new_member_id,
        session.user_id,
        NotificationKind::CommunityJoinAccepted,
        NotificationTarget::Community(target.id),
    )
    .await
    .map_err(db_err)?;

    tx.commit().await.map_err(db_err)?;

    Ok(HttpResponse::NoContent().finish())
}

#[utoipa::path(
    post,
    path = "/api/v1/community/{slug}/join-request/{id}/reject",
    description = "Recusa um pedido de entrada. O usuário pode pedir de novo depois",
    params(
        ("slug" = String, Path, description = "Slug público da comunidade"),
        ("id" = Uuid, Path, description = "Id do pedido"),
    ),
    responses(
        (status = 204, description = "Pedido recusado"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Apenas o dono e os moderadores respondem", body = ApiErrorResponse),
        (status = 404, description = "Comunidade ou pedido não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Recusa um pedido de entrada", skip_all)]
pub async fn reject(
    path: Path<(String, Uuid)>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    let (slug, request_id) = path.into_inner();

    let Some(target) = fetch_community_target(pool.get_ref(), &slug, Some(session.user_id)).await?
    else {
        return Ok(community_not_found());
    };

    if !target.can_moderate() {
        return Ok(moderator_required());
    }

    let rejected = sqlx::query!(
        r#"
        UPDATE community_join_request
        SET status = 'rejected', responded_at = NOW()
        WHERE id = $1 AND community_id = $2 AND status = 'pending'
        "#,
        request_id,
        target.id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?
    .rows_affected();

    if rejected == 0 {
        return Ok(request_not_found());
    }

    Ok(HttpResponse::NoContent().finish())
}

#[utoipa::path(
    delete,
    path = "/api/v1/community/{slug}/join-request",
    description = "Cancela o próprio pedido de entrada pendente",
    params(("slug" = String, Path, description = "Slug público da comunidade")),
    responses(
        (status = 204, description = "Pedido cancelado"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Comunidade ou pedido não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Cancela o próprio pedido de entrada", skip_all)]
pub async fn cancel(
    slug: Path<String>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    let Some(target) =
        fetch_community_target(pool.get_ref(), &slug.into_inner(), Some(session.user_id)).await?
    else {
        return Ok(community_not_found());
    };

    let cancelled = sqlx::query!(
        r#"
        DELETE FROM community_join_request
        WHERE community_id = $1 AND user_id = $2 AND status = 'pending'
        "#,
        target.id,
        session.user_id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?
    .rows_affected();

    if cancelled == 0 {
        return Ok(request_not_found());
    }

    Ok(HttpResponse::NoContent().finish())
}
