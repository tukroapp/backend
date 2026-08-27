use actix_web::{
    HttpResponse, Responder,
    web::{Data, Json, Path, Query},
};
use sqlx::PgPool;
use uuid::Uuid;
use validator::Validate;

use std::sync::Arc;

use crate::{
    routes::profile::{
        ProfileSummary,
        slug::{
            fetch_target, profile_not_found,
            scraps::{
                ScrapRequest, ScrapResponse, create_scrap, fetch_scraps, private_scrap_wall,
                scrap_write_forbidden,
            },
        },
    },
    services::{
        auth::SessionData,
        notification::{NotificationKind, NotificationTarget, notify},
        storage::{StorageService, public_url_of},
    },
    utils::{
        ApiCursorPaginatedResponse, ApiErrorResponse, ApiResponse, CursorPaginationQuery, db_err,
        take_page, unauthorized,
    },
};

/// Dono do mural e autor de um recado, junto do slug do perfil, que é o que
/// resolve a privacidade.
struct ScrapOwners {
    profile_id: Uuid,
    profile_slug: String,
    author_id: Uuid,
    parent_id: Option<Uuid>,
}

fn scrap_not_found() -> HttpResponse {
    HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
        "scrap",
        "not_found",
        Some("Recado não encontrado."),
        vec![],
    ))
}

async fn fetch_scrap(pool: &PgPool, id: Uuid) -> Result<Option<ScrapOwners>, actix_web::Error> {
    let row = sqlx::query!(
        r#"
        SELECT s.profile_id, s.author_id, s.parent_id, up.slug AS profile_slug
        FROM scrap s
        INNER JOIN user_profile up ON up.user_id = s.profile_id
        WHERE s.id = $1 AND s.deleted_at IS NULL
        "#,
        id,
    )
    .fetch_optional(pool)
    .await
    .map_err(db_err)?;

    Ok(row.map(|row| ScrapOwners {
        profile_id: row.profile_id,
        profile_slug: row.profile_slug,
        author_id: row.author_id,
        parent_id: row.parent_id,
    }))
}

#[utoipa::path(
    get,
    path = "/api/v1/scrap/{id}/reply",
    description = "Lista paginada (cursor) das respostas de um recado, respeitando a privacidade do mural",
    params(
        ("id" = Uuid, Path, description = "Id do recado"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Respostas do recado", body = ApiCursorPaginatedResponse<Vec<ScrapResponse>>),
        (status = 403, description = "Mural privado", body = ApiErrorResponse),
        (status = 404, description = "Recado não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Lista as respostas de um recado", skip_all)]
pub async fn get_replies(
    id: Path<Uuid>,
    pagination: Query<CursorPaginationQuery>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let viewer = session.map(|session| session.user_id);
    let id = id.into_inner();

    let Some(scrap) = fetch_scrap(pool.get_ref(), id).await? else {
        return Ok(scrap_not_found());
    };

    let Some(target) = fetch_target(pool.get_ref(), &scrap.profile_slug, viewer).await? else {
        return Ok(profile_not_found());
    };

    if !target.can_read_scrap {
        return Ok(private_scrap_wall());
    }

    let limit = pagination.limit();
    let mut replies = fetch_scraps(
        pool.get_ref(),
        storage.as_ref().as_ref(),
        scrap.profile_id,
        Some(id),
        pagination.cursor_id(),
        limit + 1,
    )
    .await?;

    let next_cursor = take_page(&mut replies, limit, |reply| reply.id.to_string());

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(replies, next_cursor)))
}

#[utoipa::path(
    post,
    path = "/api/v1/scrap/{id}/reply",
    description = "Responde um recado, respeitando a privacidade de escrita do dono do mural",
    params(("id" = Uuid, Path, description = "Id do recado respondido")),
    request_body = ScrapRequest,
    responses(
        (status = 201, description = "Resposta publicada", body = ApiResponse<ScrapResponse>),
        (status = 400, description = "Corpo inválido ou resposta de resposta", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Sem permissão para escrever nesse mural", body = ApiErrorResponse),
        (status = 404, description = "Recado não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Responde um recado", skip_all)]
pub async fn post_reply(
    id: Path<Uuid>,
    payload: Json<ScrapRequest>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    if let Err(errors) = payload.validate() {
        return Ok(HttpResponse::BadRequest().json(ApiErrorResponse::from(errors)));
    }

    let id = id.into_inner();

    // Só recados de primeiro nível aceitam resposta: a thread tem um nível só.
    let Some(scrap) = fetch_scrap(pool.get_ref(), id)
        .await?
        .filter(|scrap| scrap.parent_id.is_none())
    else {
        return Ok(scrap_not_found());
    };

    let Some(target) =
        fetch_target(pool.get_ref(), &scrap.profile_slug, Some(session.user_id)).await?
    else {
        return Ok(profile_not_found());
    };

    if !target.can_write_scrap {
        return Ok(scrap_write_forbidden());
    }

    let reply = create_scrap(
        pool.get_ref(),
        storage.as_ref().as_ref(),
        scrap.profile_id,
        session.user_id,
        Some(id),
        payload.body.trim(),
    )
    .await?;

    // Quem é avisado é o autor do recado respondido, não o dono do mural.
    notify(
        pool.get_ref(),
        scrap.author_id,
        session.user_id,
        NotificationKind::ScrapReply,
        NotificationTarget::Scrap(reply.id),
    )
    .await
    .map_err(db_err)?;

    Ok(HttpResponse::Created().json(ApiResponse::new(reply)))
}

#[utoipa::path(
    put,
    path = "/api/v1/scrap/{id}",
    description = "Edita um recado. Só o autor pode editar",
    params(("id" = Uuid, Path, description = "Id do recado")),
    request_body = ScrapRequest,
    responses(
        (status = 200, description = "Recado editado", body = ApiResponse<ScrapResponse>),
        (status = 400, description = "Corpo inválido", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Recado não encontrado ou de outro autor", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Edita um recado", skip_all)]
pub async fn put(
    id: Path<Uuid>,
    payload: Json<ScrapRequest>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    if let Err(errors) = payload.validate() {
        return Ok(HttpResponse::BadRequest().json(ApiErrorResponse::from(errors)));
    }

    let row = sqlx::query!(
        r#"
        WITH updated AS (
            UPDATE scrap
            SET body = $1
            WHERE id = $2 AND author_id = $3 AND deleted_at IS NULL
            RETURNING id, body, created_at, updated_at, author_id, parent_id
        )
        SELECT
            u.id,
            u.body,
            u.created_at,
            u.updated_at,
            up.user_id AS author_id,
            up.slug AS author_slug,
            up.name AS author_name,
            up.avatar_url AS author_avatar_url,
            up.status AS author_status,
            (
                SELECT COUNT(*) FROM scrap r
                WHERE r.parent_id = u.id AND r.deleted_at IS NULL
            ) AS "reply_count!"
        FROM updated u
        INNER JOIN user_profile up ON up.user_id = u.author_id
        "#,
        payload.body.trim(),
        id.into_inner(),
        session.user_id,
    )
    .fetch_optional(pool.get_ref())
    .await
    .map_err(db_err)?;

    let Some(row) = row else {
        return Ok(scrap_not_found());
    };

    Ok(HttpResponse::Ok().json(ApiResponse::new(ScrapResponse {
        id: row.id,
        body: row.body,
        created_at: row.created_at,
        updated_at: row.updated_at,
        author: ProfileSummary {
            id: row.author_id,
            slug: row.author_slug,
            name: row.author_name,
            avatar_url: public_url_of(storage.as_ref().as_ref(), row.author_avatar_url),
            status: row.author_status,
        },
        reply_count: row.reply_count,
    })))
}

#[utoipa::path(
    delete,
    path = "/api/v1/scrap/{id}",
    description = "Apaga um recado. Podem apagar o autor do recado e o dono do mural",
    params(("id" = Uuid, Path, description = "Id do recado")),
    responses(
        (status = 204, description = "Recado apagado"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Recado não encontrado ou sem permissão", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Apaga um recado", skip_all)]
pub async fn delete(
    id: Path<Uuid>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    // Soft delete: o recado some de todas as leituras, mas continua no banco
    // para a moderação. As respostas somem junto, como faziam pelo CASCADE.
    let removed = sqlx::query_scalar!(
        r#"
        WITH target AS (
            UPDATE scrap
            SET deleted_at = NOW()
            WHERE id = $1
              AND (author_id = $2 OR profile_id = $2)
              AND deleted_at IS NULL
            RETURNING id
        ), replies AS (
            UPDATE scrap
            SET deleted_at = NOW()
            WHERE parent_id = (SELECT id FROM target) AND deleted_at IS NULL
            RETURNING id
        )
        SELECT COUNT(*) AS "removed!" FROM target
        "#,
        id.into_inner(),
        session.user_id,
    )
    .fetch_one(pool.get_ref())
    .await
    .map_err(db_err)?;

    if removed == 0 {
        return Ok(scrap_not_found());
    }

    Ok(HttpResponse::NoContent().finish())
}
