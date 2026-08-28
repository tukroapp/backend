use actix_web::{
    HttpResponse, Responder,
    web::{Data, Query},
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use std::sync::Arc;

use crate::{
    routes::{media::MediaKind, profile::ProfileSummary},
    services::{activity::ActivityKind, auth::SessionData, storage::StorageService},
    utils::{
        ApiCursorPaginatedResponse, ApiErrorResponse, CursorPaginationQuery, db_err, take_page,
    },
};

#[derive(Serialize, ToSchema)]
pub struct FeedCommunity {
    pub slug: String,
    pub name: String,
}

#[derive(Serialize, ToSchema)]
pub struct FeedMedia {
    pub id: Uuid,
    pub kind: MediaKind,
    pub url: String,
}

#[derive(Serialize, ToSchema)]
pub struct FeedItem {
    pub id: Uuid,
    pub kind: ActivityKind,
    pub created_at: DateTime<Utc>,
    pub actor: ProfileSummary,
    /// Outra pessoa envolvida: autor do depoimento, novo amigo, ídolo. `null`
    /// quando o `kind` não usa.
    pub target_user: Option<ProfileSummary>,
    pub community: Option<FeedCommunity>,
    pub media: Option<FeedMedia>,
    /// Texto do novo recado, no `status_changed`.
    pub text: Option<String>,
}

fn parse_cursor(cursor: Option<&str>) -> Option<(DateTime<Utc>, Uuid)> {
    let (at, id) = cursor?.rsplit_once(':')?;
    let at = DateTime::parse_from_rfc3339(at).ok()?.with_timezone(&Utc);
    Some((at, id.parse().ok()?))
}

#[utoipa::path(
    get,
    path = "/api/v1/me/feed",
    description = "Feed da página inicial: atividades dos amigos e dos ídolos do usuário. Puro, seguro de pollar",
    params(CursorPaginationQuery),
    responses(
        (status = 200, description = "Feed", body = ApiCursorPaginatedResponse<Vec<FeedItem>>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Feed da página inicial", skip_all)]
pub async fn list(
    pagination: Query<CursorPaginationQuery>,
    session: SessionData,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let me = session.user_id;
    let limit = pagination.limit();
    let (cursor_at, cursor_id) = match parse_cursor(pagination.cursor.as_deref()) {
        Some((at, id)) => (Some(at), Some(id)),
        None => (None, None),
    };

    let rows = sqlx::query!(
        r#"
        SELECT
            a.id,
            a.kind AS "kind: ActivityKind",
            a.created_at,
            a.text_value,
            ap.user_id AS actor_id,
            ap.slug AS actor_slug,
            ap.name AS actor_name,
            ap.avatar_url AS actor_avatar,
            ap.status AS actor_status,
            tu.user_id AS "tu_id?",
            tu.slug AS "tu_slug?",
            tu.name AS "tu_name?",
            tu.avatar_url AS "tu_avatar?",
            tu.status AS "tu_status?",
            c.slug AS "community_slug?",
            c.name AS "community_name?",
            m.id AS "media_id?",
            m.kind AS "media_kind?: MediaKind",
            m.storage_key AS "media_key?"
        FROM activity a
        INNER JOIN auth_user au ON au.id = a.actor_id AND au.status = 'active'
        INNER JOIN user_profile ap ON ap.user_id = a.actor_id
        LEFT JOIN user_profile tu ON tu.user_id = a.target_user_id
        LEFT JOIN community c ON c.id = a.community_id
        LEFT JOIN user_media m ON m.id = a.media_id
        WHERE a.actor_id <> $1
          -- Comunidade removida pela moderação some do feed junto.
          AND (a.community_id IS NULL OR c.deleted_at IS NULL)
          AND (
              (EXISTS (SELECT 1 FROM friend f WHERE f.user_id = $1 AND f.friend_id = a.actor_id)
                  AND ap.profile_visibility <> 'nobody')
              OR (EXISTS (SELECT 1 FROM fan WHERE fan.fan_id = $1 AND fan.idol_id = a.actor_id)
                  AND ap.profile_visibility = 'everyone')
          )
          AND (
              $2::timestamptz IS NULL
              OR (a.created_at, a.id) < ($2::timestamptz, $3::uuid)
          )
        ORDER BY a.created_at DESC, a.id DESC
        LIMIT $4
        "#,
        me,
        cursor_at,
        cursor_id,
        limit + 1,
    )
    .fetch_all(pool.get_ref())
    .await
    .map_err(db_err)?;

    let storage = storage.as_ref().as_ref();

    let mut items: Vec<FeedItem> = rows
        .into_iter()
        .map(|row| {
            let target_user = row.tu_id.map(|id| ProfileSummary {
                id,
                slug: row.tu_slug.unwrap_or_default(),
                name: row.tu_name.unwrap_or_default(),
                avatar_url: crate::services::storage::public_url_of(storage, row.tu_avatar),
                status: row.tu_status,
            });

            let community = match (row.community_slug, row.community_name) {
                (Some(slug), Some(name)) => Some(FeedCommunity { slug, name }),
                _ => None,
            };

            let media = match (row.media_id, row.media_kind, row.media_key) {
                (Some(id), Some(kind), Some(key)) => Some(FeedMedia {
                    id,
                    kind,
                    url: storage.public_url(&key),
                }),
                _ => None,
            };

            FeedItem {
                id: row.id,
                kind: row.kind,
                created_at: row.created_at,
                actor: ProfileSummary {
                    id: row.actor_id,
                    slug: row.actor_slug,
                    name: row.actor_name,
                    avatar_url: crate::services::storage::public_url_of(storage, row.actor_avatar),
                    status: row.actor_status,
                },
                target_user,
                community,
                media,
                text: row.text_value,
            }
        })
        .collect();

    let next_cursor = take_page(&mut items, limit, |item| {
        format!("{}:{}", item.created_at.to_rfc3339(), item.id)
    });

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(items, next_cursor)))
}

#[utoipa::path(
    post,
    path = "/api/v1/me/feed/seen",
    description = "Marca o feed como visto até agora, zerando o contador de novidades",
    responses(
        (status = 204, description = "Feed marcado como visto"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Marca o feed como visto", skip_all)]
pub async fn seen(
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    sqlx::query!(
        r#"UPDATE user_profile SET feed_seen_at = NOW() WHERE user_id = $1"#,
        session.user_id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?;

    Ok(HttpResponse::NoContent().finish())
}
