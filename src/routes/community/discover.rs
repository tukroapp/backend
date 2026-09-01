use actix_web::{
    HttpResponse, Responder,
    web::{Data, Query},
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use std::sync::Arc;

use crate::{
    routes::community::{CommunityCategory, CommunitySummary},
    services::{
        auth::SessionData,
        cache,
        storage::{StorageService, public_url_of},
    },
    startup::RedisPool,
    utils::{
        ApiCursorPaginatedResponse, ApiErrorResponse, ApiResponse, CursorPaginationQuery, db_err,
        take_page,
    },
};

#[derive(Serialize, Deserialize, ToSchema)]
pub struct CategoryCount {
    pub category: CommunityCategory,
    /// Comunidades ativas nessa categoria.
    pub count: i64,
}

/// Uma comunidade de amigo, com quantos amigos já estão dentro: é o que faz a
/// sugestão valer mais que uma lista qualquer.
#[derive(Serialize, ToSchema)]
pub struct FriendCommunity {
    #[serde(flatten)]
    pub community: CommunitySummary,
    pub friends_count: i64,
}

#[utoipa::path(
    get,
    path = "/api/v1/community/category",
    description = "Índice de categorias com quantas comunidades cada uma tem, para montar a tela de descoberta",
    responses(
        (status = 200, description = "Categorias e contagens", body = ApiResponse<Vec<CategoryCount>>),
    ),
)]
#[tracing::instrument(name = "Índice de categorias", skip_all)]
pub async fn categories(
    pool: Data<PgPool>,
    redis: Data<RedisPool>,
) -> Result<impl Responder, actix_web::Error> {
    let redis = redis.get_ref();

    if let Some(cached) =
        cache::get_json::<Vec<CategoryCount>>(redis, cache::COMMUNITY_CATEGORY_COUNTS).await
    {
        return Ok(HttpResponse::Ok().json(ApiResponse::new(cached)));
    }

    let rows = sqlx::query!(
        r#"
        SELECT
            category AS "category: CommunityCategory",
            COUNT(*) AS "count!"
        FROM community
        WHERE deleted_at IS NULL
        GROUP BY category
        ORDER BY COUNT(*) DESC, category
        "#,
    )
    .fetch_all(pool.get_ref())
    .await
    .map_err(db_err)?;

    let results = rows
        .into_iter()
        .map(|row| CategoryCount {
            category: row.category,
            count: row.count,
        })
        .collect::<Vec<_>>();

    cache::set_json(
        redis,
        cache::COMMUNITY_CATEGORY_COUNTS,
        &results,
        cache::CATEGORY_COUNTS_TTL,
    )
    .await;

    Ok(HttpResponse::Ok().json(ApiResponse::new(results)))
}

#[utoipa::path(
    get,
    path = "/api/v1/me/community/friend",
    description = "Comunidades em que os seus amigos estão e você não, das mais frequentadas por eles para as menos",
    params(CursorPaginationQuery),
    responses(
        (status = 200, description = "Comunidades dos amigos", body = ApiCursorPaginatedResponse<Vec<FriendCommunity>>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Comunidades dos meus amigos", skip_all)]
pub async fn from_friends(
    pagination: Query<CursorPaginationQuery>,
    session: SessionData,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let limit = pagination.limit();

    // O cursor carrega a contagem de amigos junto do id, porque a ordem é por
    // ela e só o id não identifica a posição na página.
    let (cursor_friends, cursor_id) = match pagination
        .cursor
        .as_deref()
        .and_then(|cursor| cursor.split_once(':'))
    {
        Some((friends, id)) => (friends.parse::<i64>().ok(), id.parse::<Uuid>().ok()),
        None => (None, None),
    };

    let mut rows = sqlx::query!(
        r#"
        SELECT
            c.id,
            c.slug,
            c.name,
            c.description,
            c.avatar_url,
            c.category AS "category: CommunityCategory",
            c.is_private,
            (
                SELECT COUNT(*) FROM community_member m
                INNER JOIN auth_user au ON au.id = m.user_id AND au.status = 'active'
                WHERE m.community_id = c.id
            ) AS "members_count!",
            COUNT(*) AS "friends_count!"
        FROM friend f
        INNER JOIN auth_user au ON au.id = f.friend_id AND au.status = 'active'
        INNER JOIN community_member cm ON cm.user_id = f.friend_id
        INNER JOIN community c ON c.id = cm.community_id AND c.deleted_at IS NULL
        WHERE f.user_id = $1
          -- Comunidade privada não se descobre por fora: ou te convidam, ou não.
          AND NOT c.is_private
          AND NOT EXISTS (
              SELECT 1 FROM community_member mine
              WHERE mine.user_id = $1 AND mine.community_id = c.id
          )
        GROUP BY c.id
        HAVING (
            $2::bigint IS NULL
            OR (COUNT(*), c.id) < ($2::bigint, $3::uuid)
        )
        ORDER BY COUNT(*) DESC, c.id DESC
        LIMIT $4
        "#,
        session.user_id,
        cursor_friends,
        cursor_id,
        limit + 1,
    )
    .fetch_all(pool.get_ref())
    .await
    .map_err(db_err)?;

    let storage = storage.as_ref().as_ref();
    let mut results = rows
        .drain(..)
        .map(|row| FriendCommunity {
            community: CommunitySummary {
                id: row.id,
                slug: row.slug,
                name: row.name,
                description: row.description,
                avatar_url: public_url_of(storage, row.avatar_url),
                category: row.category,
                is_private: row.is_private,
                members_count: row.members_count,
            },
            friends_count: row.friends_count,
        })
        .collect::<Vec<_>>();

    let next_cursor = take_page(&mut results, limit, |item| {
        format!("{}:{}", item.friends_count, item.community.id)
    });

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(results, next_cursor)))
}
