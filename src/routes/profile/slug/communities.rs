use actix_web::{
    HttpResponse, Responder,
    web::{Data, Path, Query},
};
use serde::Deserialize;
use sqlx::PgPool;

use std::sync::Arc;

use crate::{
    routes::{
        community::{CommunityCategory, CommunitySummary},
        profile::slug::{fetch_target, profile_not_found},
    },
    services::{
        auth::SessionData,
        storage::{StorageService, public_url_of},
    },
    utils::{
        ApiCursorPaginatedResponse, ApiErrorResponse, CursorPaginationQuery, db_err, take_page,
    },
};

/// Fica separado de `CursorPaginationQuery` porque `#[serde(flatten)]` faria o
/// serde ler todos os valores da query string como texto, quebrando `limit`.
#[derive(Debug, Deserialize)]
pub struct CommunitiesQuery {
    /// `true` deixa só as comunidades em comum com quem está olhando. Ignorado
    /// para visitante anônimo e no próprio perfil.
    pub mutual: Option<bool>,
}

#[utoipa::path(
    get,
    path = "/api/v1/profile/{slug}/community",
    description = "Lista paginada (cursor) das comunidades das quais o perfil participa, respeitando a privacidade do dono",
    params(
        ("slug" = String, Path, description = "Slug público do perfil"),
        ("mutual" = Option<bool>, Query, description = "Só as comunidades em comum com quem está olhando"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Comunidades do perfil", body = ApiCursorPaginatedResponse<Vec<CommunitySummary>>),
        (status = 403, description = "Lista de comunidades privada", body = ApiErrorResponse),
        (status = 404, description = "Perfil não encontrado ou conta inativa", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Lista as comunidades de um perfil", skip_all)]
pub async fn get(
    slug: Path<String>,
    query: Query<CommunitiesQuery>,
    pagination: Query<CursorPaginationQuery>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let viewer = session.map(|session| session.user_id);

    let Some(target) = fetch_target(pool.get_ref(), &slug.into_inner(), viewer).await? else {
        return Ok(profile_not_found());
    };

    if !target.can_view_profile_details {
        return Ok(
            HttpResponse::Forbidden().json(ApiErrorResponse::from_single_validation_error(
                "community",
                "private_community_list",
                Some("As comunidades desse perfil são privadas."),
                vec![],
            )),
        );
    }

    let limit = pagination.limit();

    // No próprio perfil o filtro não teria sentido: toda comunidade seria comum.
    let mutual_with = query
        .mutual
        .unwrap_or(false)
        .then_some(viewer)
        .flatten()
        .filter(|viewer| *viewer != target.user_id);

    let mut results = sqlx::query_as!(
        CommunitySummary,
        r#"
        SELECT
            c.id,
            c.slug,
            c.name,
            c.description,
            c.avatar_url,
            c.category AS "category: CommunityCategory",
            c.is_private,
            c.members_count
        FROM community_member cm
        INNER JOIN community c ON c.id = cm.community_id
        WHERE cm.user_id = $1
          AND c.deleted_at IS NULL
          AND ($2::uuid IS NULL OR c.id < $2::uuid)
          AND (
              $4::uuid IS NULL
              OR EXISTS (
                  SELECT 1 FROM community_member mine
                  WHERE mine.user_id = $4::uuid AND mine.community_id = c.id
              )
          )
        ORDER BY c.id DESC
        LIMIT $3
        "#,
        target.user_id,
        pagination.cursor_id(),
        limit + 1,
        mutual_with,
    )
    .fetch_all(pool.get_ref())
    .await
    .map_err(db_err)?;

    for community in &mut results {
        community.avatar_url =
            public_url_of(storage.as_ref().as_ref(), community.avatar_url.take());
    }

    let next_cursor = take_page(&mut results, limit, |community| community.id.to_string());

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(results, next_cursor)))
}
