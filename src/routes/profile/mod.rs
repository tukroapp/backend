pub mod slug;

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
    services::{
        auth::SessionData,
        storage::{StorageService, public_url_of},
    },
    utils::{
        ApiCursorPaginatedResponse, ApiErrorResponse, CursorPaginationQuery, db_err, invalid_field,
        take_page,
    },
};

/// Tamanho mínimo do termo de busca, para não varrer a tabela inteira.
const MIN_SEARCH_LENGTH: usize = 2;

#[derive(Serialize, ToSchema)]
pub struct ProfileSummary {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub avatar_url: Option<String>,
    pub status: Option<String>,
}

/// Fica separado de `CursorPaginationQuery` porque `#[serde(flatten)]` faria o
/// serde ler todos os valores da query string como texto, quebrando `limit`.
#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    /// Termo buscado por similaridade no nome ou por prefixo do slug.
    pub q: String,
}

/// O cursor da busca carrega a relevância junto do id, porque a ordenação é por
/// similaridade e só o id não identifica a posição na página.
pub fn parse_search_cursor(cursor: Option<&str>) -> Option<(f32, Uuid)> {
    let (score, id) = cursor?.split_once(':')?;
    Some((score.parse().ok()?, id.parse().ok()?))
}

#[utoipa::path(
    get,
    path = "/api/v1/profile",
    description = "Busca perfis ativos por nome (similaridade) ou prefixo do slug, escondendo bloqueios e o próprio usuário",
    params(
        ("q" = String, Query, description = "Termo buscado, mínimo de 2 caracteres"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Perfis encontrados", body = ApiCursorPaginatedResponse<Vec<ProfileSummary>>),
        (status = 400, description = "Termo de busca muito curto", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Busca perfis por nome ou slug", skip_all)]
pub async fn get(
    query: Query<SearchQuery>,
    pagination: Query<CursorPaginationQuery>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let viewer = session.map(|session| session.user_id);
    let term = query.q.trim().to_lowercase();
    let limit = pagination.limit();
    let (cursor_score, cursor_id) = match parse_search_cursor(pagination.cursor.as_deref()) {
        Some((score, id)) => (Some(score), Some(id)),
        None => (None, None),
    };

    if term.chars().count() < MIN_SEARCH_LENGTH {
        return Ok(invalid_field(
            "q",
            "too_short",
            "A busca precisa de pelo menos 2 caracteres.",
        ));
    }

    let mut rows = sqlx::query!(
        r#"
        SELECT
            up.user_id AS id,
            up.slug,
            up.name,
            up.avatar_url,
            up.status,
            similarity(up.name, $1) AS "score!"
        FROM user_profile up
        INNER JOIN auth_user au ON au.id = up.user_id
        WHERE au.status = 'active'
          AND up.user_id IS DISTINCT FROM $2::uuid
          AND (up.name % $1 OR up.slug LIKE $1 || '%')
          AND NOT EXISTS (
              SELECT 1 FROM block b
              WHERE (b.blocker_id = up.user_id AND b.blocked_id = $2::uuid)
                 OR (b.blocker_id = $2::uuid AND b.blocked_id = up.user_id)
          )
          AND (
              $3::real IS NULL
              OR (similarity(up.name, $1), up.user_id) < ($3::real, $4::uuid)
          )
        ORDER BY similarity(up.name, $1) DESC, up.user_id DESC
        LIMIT $5
        "#,
        term,
        viewer,
        cursor_score,
        cursor_id,
        limit + 1,
    )
    .fetch_all(pool.get_ref())
    .await
    .map_err(db_err)?;

    let next_cursor = take_page(&mut rows, limit, |row| format!("{}:{}", row.score, row.id));

    let results = rows
        .into_iter()
        .map(|row| ProfileSummary {
            id: row.id,
            slug: row.slug,
            name: row.name,
            avatar_url: public_url_of(storage.as_ref().as_ref(), row.avatar_url),
            status: row.status,
        })
        .collect::<Vec<_>>();

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(results, next_cursor)))
}
