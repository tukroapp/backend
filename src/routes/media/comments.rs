use actix_web::{
    HttpResponse, Responder,
    web::{Data, Json, Path, Query},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

use std::sync::Arc;

use crate::{
    routes::{album::visible_media_owner, profile::ProfileSummary},
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

#[derive(Debug, Deserialize, ToSchema, Validate)]
pub struct CommentRequest {
    #[validate(length(
        min = 1,
        max = 1000,
        message = "O comentário deve ter entre 1 e 1000 caracteres"
    ))]
    pub body: String,
}

#[derive(Serialize, ToSchema)]
pub struct CommentResponse {
    pub id: Uuid,
    pub body: String,
    pub author: ProfileSummary,
    pub created_at: DateTime<Utc>,
}

fn comment_not_found() -> HttpResponse {
    HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
        "comment",
        "not_found",
        Some("Comentário não encontrado."),
        vec![],
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/media/{id}/comment",
    description = "Comentários de uma foto, do mais antigo para o mais novo, como a conversa embaixo da imagem",
    params(
        ("id" = Uuid, Path, description = "Id da foto"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Comentários", body = ApiCursorPaginatedResponse<Vec<CommentResponse>>),
        (status = 404, description = "Foto não encontrada ou fora do alcance de quem olha", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Lista os comentários de uma foto", skip_all)]
pub async fn get(
    id: Path<Uuid>,
    pagination: Query<CursorPaginationQuery>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let media_id = id.into_inner();
    let viewer = session.map(|session| session.user_id);

    if visible_media_owner(pool.get_ref(), media_id, viewer)
        .await?
        .is_none()
    {
        return Ok(crate::routes::media::media_not_found());
    }

    let limit = pagination.limit();
    let rows = sqlx::query!(
        r#"
        SELECT
            c.id,
            c.body,
            c.created_at,
            up.user_id AS author_id,
            up.slug AS author_slug,
            up.name AS author_name,
            up.avatar_url AS author_avatar_url,
            up.status AS author_status
        FROM media_comment c
        INNER JOIN auth_user au ON au.id = c.author_id AND au.status IN ('active', 'deleted')
        INNER JOIN user_profile up ON up.user_id = c.author_id
        WHERE c.media_id = $1
          AND c.deleted_at IS NULL
          AND ($2::uuid IS NULL OR c.id > $2::uuid)
        ORDER BY c.id
        LIMIT $3
        "#,
        media_id,
        pagination.cursor_id(),
        limit + 1,
    )
    .fetch_all(pool.get_ref())
    .await
    .map_err(db_err)?;

    let storage = storage.as_ref().as_ref();
    let mut results = rows
        .into_iter()
        .map(|row| CommentResponse {
            id: row.id,
            body: row.body,
            author: ProfileSummary {
                id: row.author_id,
                slug: row.author_slug,
                name: row.author_name,
                avatar_url: public_url_of(storage, row.author_avatar_url),
                status: row.author_status,
            },
            created_at: row.created_at,
        })
        .collect::<Vec<_>>();

    let next_cursor = take_page(&mut results, limit, |comment| comment.id.to_string());

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(results, next_cursor)))
}

#[utoipa::path(
    post,
    path = "/api/v1/media/{id}/comment",
    description = "Comenta uma foto. Quem enxerga a foto pode comentar",
    params(("id" = Uuid, Path, description = "Id da foto")),
    request_body = CommentRequest,
    responses(
        (status = 201, description = "Comentário publicado", body = ApiResponse<CommentResponse>),
        (status = 400, description = "Corpo inválido", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "E-mail não confirmado", body = ApiErrorResponse),
        (status = 404, description = "Foto não encontrada ou fora do alcance de quem olha", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Comenta uma foto", skip_all)]
pub async fn post(
    id: Path<Uuid>,
    payload: Json<CommentRequest>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };
    session.require_verified_email(pool.get_ref()).await?;

    if let Err(errors) = payload.validate() {
        return Ok(HttpResponse::BadRequest().json(ApiErrorResponse::from(errors)));
    }

    let media_id = id.into_inner();
    let Some(owner_id) =
        visible_media_owner(pool.get_ref(), media_id, Some(session.user_id)).await?
    else {
        return Ok(crate::routes::media::media_not_found());
    };

    let mut tx = pool.begin().await.map_err(db_err)?;

    let created = sqlx::query!(
        r#"
        WITH inserted AS (
            INSERT INTO media_comment (media_id, author_id, body)
            VALUES ($1, $2, $3)
            RETURNING id, body, created_at, author_id
        )
        SELECT
            i.id,
            i.body,
            i.created_at,
            up.user_id AS author_id,
            up.slug AS author_slug,
            up.name AS author_name,
            up.avatar_url AS author_avatar_url,
            up.status AS author_status
        FROM inserted i
        INNER JOIN user_profile up ON up.user_id = i.author_id
        "#,
        media_id,
        session.user_id,
        payload.body.trim(),
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(db_err)?;

    // Um comentário, uma notificação para o dono da foto: é um insert por
    // evento, não um por espectador.
    notify(
        &mut *tx,
        owner_id,
        session.user_id,
        NotificationKind::MediaComment,
        NotificationTarget::Media(media_id),
    )
    .await
    .map_err(db_err)?;

    tx.commit().await.map_err(db_err)?;

    let storage = storage.as_ref().as_ref();

    Ok(
        HttpResponse::Created().json(ApiResponse::new(CommentResponse {
            id: created.id,
            body: created.body,
            author: ProfileSummary {
                id: created.author_id,
                slug: created.author_slug,
                name: created.author_name,
                avatar_url: public_url_of(storage, created.author_avatar_url),
                status: created.author_status,
            },
            created_at: created.created_at,
        })),
    )
}

#[utoipa::path(
    delete,
    path = "/api/v1/media/{id}/comment/{comment_id}",
    description = "Apaga um comentário. Pode o autor do comentário e o dono da foto",
    params(
        ("id" = Uuid, Path, description = "Id da foto"),
        ("comment_id" = Uuid, Path, description = "Id do comentário"),
    ),
    responses(
        (status = 204, description = "Comentário apagado"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Comentário não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Apaga um comentário de foto", skip_all)]
pub async fn delete(
    path: Path<(Uuid, Uuid)>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    let (media_id, comment_id) = path.into_inner();

    let deleted = sqlx::query!(
        r#"
        UPDATE media_comment c
        SET deleted_at = NOW()
        WHERE c.id = $1
          AND c.media_id = $2
          AND c.deleted_at IS NULL
          AND (
              c.author_id = $3
              OR EXISTS (
                  SELECT 1 FROM user_media m
                  WHERE m.id = c.media_id AND m.user_id = $3
              )
          )
        "#,
        comment_id,
        media_id,
        session.user_id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?
    .rows_affected();

    if deleted == 0 {
        return Ok(comment_not_found());
    }

    Ok(HttpResponse::NoContent().finish())
}
