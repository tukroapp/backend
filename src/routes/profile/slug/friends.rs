use actix_web::{
    HttpResponse, Responder,
    web::{Data, Path, Query},
};
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

use std::sync::Arc;

use crate::{
    routes::profile::{
        ProfileSummary,
        slug::{fetch_target, profile_not_found},
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
pub struct FriendsQuery {
    /// Filtro opcional pelo nome (trecho) ou pelo início do slug do amigo.
    pub q: Option<String>,
    /// `true` deixa só os amigos em comum com quem está olhando. Ignorado para
    /// visitante anônimo e no próprio perfil, onde todo amigo é comum.
    pub mutual: Option<bool>,
}

/// Amigos ordenados pelo id (UUIDv7) do amigo, em ordem decrescente, que é o
/// mesmo critério usado pelo cursor. `term` filtra por nome ou slug quando vier,
/// e `mutual_with` restringe a quem também é amigo de quem está olhando.
pub async fn fetch_friends(
    pool: &PgPool,
    storage: &dyn StorageService,
    user_id: Uuid,
    term: Option<&str>,
    cursor: Option<Uuid>,
    limit: i64,
    mutual_with: Option<Uuid>,
) -> Result<Vec<ProfileSummary>, actix_web::Error> {
    let mut friends = sqlx::query_as!(
        ProfileSummary,
        r#"
        SELECT
            up.user_id AS id,
            up.slug,
            up.name,
            up.avatar_url,
            up.status
        FROM friend f
        INNER JOIN auth_user au ON au.id = f.friend_id AND au.status = 'active'
        INNER JOIN user_profile up ON up.user_id = f.friend_id
        WHERE f.user_id = $1
          AND ($2::uuid IS NULL OR f.friend_id < $2::uuid)
          AND (
              $3::text IS NULL
              OR up.name ILIKE '%' || $3 || '%'
              OR up.slug LIKE $3 || '%'
          )
          AND (
              $5::uuid IS NULL
              OR EXISTS (
                  SELECT 1 FROM friend mine
                  WHERE mine.user_id = $5::uuid AND mine.friend_id = f.friend_id
              )
          )
        ORDER BY f.friend_id DESC
        LIMIT $4
        "#,
        user_id,
        cursor,
        term,
        limit,
        mutual_with,
    )
    .fetch_all(pool)
    .await
    .map_err(db_err)?;

    for friend in &mut friends {
        friend.avatar_url = public_url_of(storage, friend.avatar_url.take());
    }

    Ok(friends)
}

#[utoipa::path(
    get,
    path = "/api/v1/profile/{slug}/friend",
    description = "Lista paginada (cursor) dos amigos do perfil, respeitando a privacidade do dono",
    params(
        ("slug" = String, Path, description = "Slug público do perfil"),
        ("q" = Option<String>, Query, description = "Filtra os amigos por trecho do nome ou início do slug"),
        ("mutual" = Option<bool>, Query, description = "Só os amigos em comum com quem está olhando"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Amigos do perfil", body = ApiCursorPaginatedResponse<Vec<ProfileSummary>>),
        (status = 403, description = "Lista de amigos privada", body = ApiErrorResponse),
        (status = 404, description = "Perfil não encontrado ou conta inativa", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Lista os amigos de um perfil", skip_all)]
pub async fn get(
    slug: Path<String>,
    query: Query<FriendsQuery>,
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
                "friends",
                "private_friend_list",
                Some("A lista de amigos desse perfil é privada."),
                vec![],
            )),
        );
    }

    let limit = pagination.limit();
    // `_` e `%` viram literais para o filtro não virar curinga de LIKE.
    let term = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .map(|term| {
            term.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        });

    // No próprio perfil o filtro não teria sentido: todo amigo seria comum.
    let mutual_with = query
        .mutual
        .unwrap_or(false)
        .then_some(viewer)
        .flatten()
        .filter(|viewer| *viewer != target.user_id);

    let mut friends = fetch_friends(
        pool.get_ref(),
        storage.as_ref().as_ref(),
        target.user_id,
        term.as_deref(),
        pagination.cursor_id(),
        limit + 1,
        mutual_with,
    )
    .await?;

    let next_cursor = take_page(&mut friends, limit, |friend| friend.id.to_string());

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(friends, next_cursor)))
}
