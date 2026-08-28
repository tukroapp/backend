use actix_web::{
    HttpResponse, Responder,
    web::{Data, Path, Query},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use std::sync::Arc;

use crate::{
    routes::profile::ProfileSummary,
    services::{
        auth::SessionData,
        storage::{StorageService, public_url_of},
    },
    utils::{
        ApiCursorPaginatedResponse, ApiErrorResponse, ApiResponse, CursorPaginationQuery, db_err,
        take_page,
    },
};

#[derive(Serialize, ToSchema)]
pub struct NotificationResponse {
    pub id: Uuid,
    /// `friend_request`, `friend_accepted`, `scrap_received`, `scrap_reply`,
    /// `testimonial_received`, `testimonial_accepted`, `community_join_request`,
    /// `community_join_accepted`, `topic_comment` ou `topic_reply`.
    #[schema(example = "scrap_received")]
    pub kind: String,
    pub created_at: DateTime<Utc>,
    /// `null` enquanto não lida.
    pub read_at: Option<DateTime<Utc>>,
    /// Quem causou a notificação. Sempre presente hoje, porque todos os tipos
    /// emitidos vêm de uma ação de outra pessoa.
    pub actor: Option<ProfileSummary>,
    /// Slug do perfil para onde o link deve levar: o mural do recado, o perfil
    /// do depoimento ou o perfil de quem agiu. Nas notificações de comunidade o
    /// link é o `community_slug`.
    pub target_slug: String,
    pub scrap_id: Option<Uuid>,
    pub testimonial_id: Option<Uuid>,
    pub friend_request_id: Option<Uuid>,
    pub community_id: Option<Uuid>,
    pub community_slug: Option<String>,
    pub topic_id: Option<Uuid>,
    /// Preenchido em `topic_reply`. O link abre o comentário pela rota de
    /// contexto do tópico.
    pub comment_id: Option<Uuid>,
}

/// Fica separado de `CursorPaginationQuery` porque `#[serde(flatten)]` faria o
/// serde ler todos os valores da query string como texto, quebrando `limit`.
#[derive(Debug, Deserialize)]
pub struct NotificationQuery {
    /// `true` traz só as não lidas.
    pub unread: Option<bool>,
}

#[derive(Serialize, ToSchema)]
pub struct UnreadCount {
    pub unread_count: i64,
}

fn notification_not_found() -> HttpResponse {
    HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
        "notification",
        "not_found",
        Some("Notificação não encontrada."),
        vec![],
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/notification",
    description = "Lista paginada (cursor) das notificações do usuário autenticado, da mais nova para a mais antiga",
    params(
        ("unread" = Option<bool>, Query, description = "true traz só as não lidas"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Notificações", body = ApiCursorPaginatedResponse<Vec<NotificationResponse>>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Lista as notificações do usuário", skip_all)]
pub async fn get(
    query: Query<NotificationQuery>,
    pagination: Query<CursorPaginationQuery>,
    session: SessionData,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let limit = pagination.limit();
    let unread_only = query.unread.unwrap_or(false);

    // O bloqueio é aplicado na leitura, não no momento em que acontece: uma
    // notificação antiga simplesmente para de aparecer para os dois lados.
    let rows = sqlx::query!(
        r#"
        SELECT
            n.id,
            n.kind::TEXT AS "kind!",
            n.created_at,
            n.read_at,
            n.scrap_id,
            n.testimonial_id,
            n.friend_request_id,
            n.community_id,
            n.topic_id,
            n.comment_id,
            COALESCE(c.slug, topic_community.slug) AS "community_slug?",
            actor.user_id AS "actor_id?",
            actor.slug AS "actor_slug?",
            actor.name AS "actor_name?",
            actor.avatar_url AS "actor_avatar_url?",
            actor.status AS "actor_status?",
            COALESCE(wall.slug, subject.slug, actor.slug) AS "target_slug!"
        FROM notification n
        LEFT JOIN user_profile actor ON actor.user_id = n.actor_id
        LEFT JOIN scrap s ON s.id = n.scrap_id
        LEFT JOIN user_profile wall ON wall.user_id = s.profile_id
        LEFT JOIN testimonial t ON t.id = n.testimonial_id
        LEFT JOIN user_profile subject ON subject.user_id = t.profile_id
        LEFT JOIN community c ON c.id = n.community_id
        LEFT JOIN community_topic topic ON topic.id = n.topic_id
        LEFT JOIN community topic_community ON topic_community.id = topic.community_id
        LEFT JOIN auth_user actor_account ON actor_account.id = n.actor_id
        WHERE n.user_id = $1
          AND (n.community_id IS NULL OR c.deleted_at IS NULL)
          AND (topic.community_id IS NULL OR topic_community.deleted_at IS NULL)
          -- Notificação de quem saiu do ar some da caixa, como o bloqueio faz.
          AND (n.actor_id IS NULL OR actor_account.status = 'active')
          AND (NOT $2 OR n.read_at IS NULL)
          -- Conteúdo apagado leva a notificação junto, como fazia o CASCADE
          -- antes do soft delete.
          AND s.deleted_at IS NULL
          AND t.deleted_at IS NULL
          AND topic.deleted_at IS NULL
          AND ($3::uuid IS NULL OR n.id < $3::uuid)
          AND NOT EXISTS (
              SELECT 1 FROM block b
              WHERE (b.blocker_id = n.user_id AND b.blocked_id = n.actor_id)
                 OR (b.blocker_id = n.actor_id AND b.blocked_id = n.user_id)
          )
        ORDER BY n.id DESC
        LIMIT $4
        "#,
        session.user_id,
        unread_only,
        pagination.cursor_id(),
        limit + 1,
    )
    .fetch_all(pool.get_ref())
    .await
    .map_err(db_err)?;

    let mut results = rows
        .into_iter()
        .map(|row| NotificationResponse {
            id: row.id,
            kind: row.kind,
            created_at: row.created_at,
            read_at: row.read_at,
            actor: row.actor_id.map(|id| ProfileSummary {
                id,
                slug: row.actor_slug.unwrap_or_default(),
                name: row.actor_name.unwrap_or_default(),
                avatar_url: public_url_of(storage.as_ref().as_ref(), row.actor_avatar_url),
                status: row.actor_status,
            }),
            target_slug: row.target_slug,
            scrap_id: row.scrap_id,
            testimonial_id: row.testimonial_id,
            friend_request_id: row.friend_request_id,
            community_id: row.community_id,
            community_slug: row.community_slug,
            topic_id: row.topic_id,
            comment_id: row.comment_id,
        })
        .collect::<Vec<_>>();

    let next_cursor = take_page(&mut results, limit, |item| item.id.to_string());

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(results, next_cursor)))
}

#[utoipa::path(
    post,
    path = "/api/v1/notification/{id}/read",
    description = "Marca uma notificação como lida. Repetir a chamada não muda a data já registrada",
    params(("id" = Uuid, Path, description = "Id da notificação")),
    responses(
        (status = 204, description = "Notificação marcada como lida"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Notificação não encontrada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Marca uma notificação como lida", skip_all)]
pub async fn read(
    id: Path<Uuid>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    // `read_at IS NULL` no SET evita reescrever a data quando a tela reenvia.
    let updated = sqlx::query!(
        r#"
        UPDATE notification
        SET read_at = COALESCE(read_at, NOW())
        WHERE id = $1 AND user_id = $2
        "#,
        id.into_inner(),
        session.user_id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?
    .rows_affected();

    if updated == 0 {
        return Ok(notification_not_found());
    }

    Ok(HttpResponse::NoContent().finish())
}

#[utoipa::path(
    post,
    path = "/api/v1/notification/read-all",
    description = "Marca todas as notificações não lidas do usuário como lidas",
    responses(
        (status = 200, description = "Notificações restantes não lidas, sempre 0", body = ApiResponse<UnreadCount>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Marca todas as notificações como lidas", skip_all)]
pub async fn read_all(
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    sqlx::query!(
        r#"
        UPDATE notification
        SET read_at = NOW()
        WHERE user_id = $1 AND read_at IS NULL
        "#,
        session.user_id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?;

    Ok(HttpResponse::Ok().json(ApiResponse::new(UnreadCount { unread_count: 0 })))
}

#[utoipa::path(
    delete,
    path = "/api/v1/notification/{id}",
    description = "Apaga uma notificação do usuário autenticado",
    params(("id" = Uuid, Path, description = "Id da notificação")),
    responses(
        (status = 204, description = "Notificação apagada"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Notificação não encontrada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Apaga uma notificação", skip_all)]
pub async fn delete(
    id: Path<Uuid>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let removed = sqlx::query!(
        r#"
        DELETE FROM notification
        WHERE id = $1 AND user_id = $2
        "#,
        id.into_inner(),
        session.user_id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?
    .rows_affected();

    if removed == 0 {
        return Ok(notification_not_found());
    }

    Ok(HttpResponse::NoContent().finish())
}
