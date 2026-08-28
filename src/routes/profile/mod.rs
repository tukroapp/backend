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

#[derive(Serialize, ToSchema)]
pub struct ProfileSearchResult {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub avatar_url: Option<String>,
    pub status: Option<String>,
    pub city: Option<String>,
    /// Idade em anos completos, `null` quando o perfil não informou nascimento.
    pub age: Option<i32>,
}

/// Fica separado de `CursorPaginationQuery` porque `#[serde(flatten)]` faria o
/// serde ler todos os valores da query string como texto, quebrando `limit`.
#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    /// Termo buscado por similaridade no nome ou por prefixo do slug. Sem ele, a
    /// resposta traz os perfis mais recentes que passarem nos filtros.
    pub q: Option<String>,
    /// Casa parte do nome da cidade.
    pub city: Option<String>,
    /// Idade mínima e máxima em anos completos. Quem não informou nascimento fica
    /// de fora quando um dos dois é usado.
    pub min_age: Option<i32>,
    pub max_age: Option<i32>,
    /// Casa parte de um dos interesses do perfil.
    pub interest: Option<String>,
}

/// Normaliza um filtro de texto: sem espaço nas pontas e sem string vazia, que
/// chega quando o formulário manda o campo em branco.
fn text_filter(value: Option<&String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
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
    description = "Busca perfis ativos por nome (similaridade) ou prefixo do slug, com filtros de cidade, idade e interesse. Esconde bloqueios e o próprio usuário",
    params(
        ("q" = Option<String>, Query, description = "Termo buscado, mínimo de 2 caracteres. Sem ele, vêm os perfis mais recentes"),
        ("city" = Option<String>, Query, description = "Casa parte do nome da cidade"),
        ("min_age" = Option<i32>, Query, description = "Idade mínima em anos completos"),
        ("max_age" = Option<i32>, Query, description = "Idade máxima em anos completos"),
        ("interest" = Option<String>, Query, description = "Casa parte de um dos interesses do perfil"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Perfis encontrados", body = ApiCursorPaginatedResponse<Vec<ProfileSearchResult>>),
        (status = 400, description = "Termo de busca muito curto", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Busca perfis por nome, slug e filtros de perfil", skip_all)]
pub async fn get(
    query: Query<SearchQuery>,
    pagination: Query<CursorPaginationQuery>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let viewer = session.map(|session| session.user_id);
    let term = text_filter(query.q.as_ref()).map(|term| term.to_lowercase());
    let city = text_filter(query.city.as_ref());
    let interest = text_filter(query.interest.as_ref());
    // O teto evita um `make_interval` com valor absurdo vindo da query string.
    let min_age = query.min_age.map(|age| age.clamp(0, 120));
    let max_age = query.max_age.map(|age| age.clamp(0, 120));
    let limit = pagination.limit();
    let (cursor_score, cursor_id) = match parse_search_cursor(pagination.cursor.as_deref()) {
        Some((score, id)) => (Some(score), Some(id)),
        None => (None, None),
    };

    if let Some(term) = &term
        && term.chars().count() < MIN_SEARCH_LENGTH
    {
        return Ok(invalid_field(
            "q",
            "too_short",
            "A busca precisa de pelo menos 2 caracteres.",
        ));
    }

    // Sem termo a similaridade é 0 para todo mundo, então a mesma consulta e o
    // mesmo cursor ("score:id") servem para a busca e para a lista de recentes.
    let mut rows = sqlx::query!(
        r#"
        SELECT
            up.user_id AS id,
            up.slug,
            up.name,
            up.avatar_url,
            up.status,
            up.city,
            EXTRACT(YEAR FROM AGE(up.birthdate))::int AS age,
            COALESCE(similarity(up.name, $1::text), 0::real) AS "score!"
        FROM user_profile up
        INNER JOIN auth_user au ON au.id = up.user_id
        WHERE au.status = 'active'
          AND up.user_id IS DISTINCT FROM $2::uuid
          AND ($1::text IS NULL OR up.name % $1 OR up.slug LIKE $1 || '%')
          AND ($6::text IS NULL OR up.city ILIKE '%' || $6 || '%')
          AND ($7::int IS NULL OR up.birthdate <= CURRENT_DATE - make_interval(years => $7))
          AND ($8::int IS NULL OR up.birthdate > CURRENT_DATE - make_interval(years => $8 + 1))
          AND ($9::text IS NULL OR EXISTS (
              SELECT 1 FROM unnest(up.interests) AS interest
              WHERE interest ILIKE '%' || $9 || '%'
          ))
          AND NOT EXISTS (
              SELECT 1 FROM block b
              WHERE (b.blocker_id = up.user_id AND b.blocked_id = $2::uuid)
                 OR (b.blocker_id = $2::uuid AND b.blocked_id = up.user_id)
          )
          AND (
              $3::real IS NULL
              OR (COALESCE(similarity(up.name, $1::text), 0::real), up.user_id) < ($3::real, $4::uuid)
          )
        ORDER BY COALESCE(similarity(up.name, $1::text), 0::real) DESC, up.user_id DESC
        LIMIT $5
        "#,
        term,
        viewer,
        cursor_score,
        cursor_id,
        limit + 1,
        city,
        min_age,
        max_age,
        interest,
    )
    .fetch_all(pool.get_ref())
    .await
    .map_err(db_err)?;

    let next_cursor = take_page(&mut rows, limit, |row| format!("{}:{}", row.score, row.id));

    let results = rows
        .into_iter()
        .map(|row| ProfileSearchResult {
            id: row.id,
            slug: row.slug,
            name: row.name,
            avatar_url: public_url_of(storage.as_ref().as_ref(), row.avatar_url),
            status: row.status,
            city: row.city,
            age: row.age,
        })
        .collect::<Vec<_>>();

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(results, next_cursor)))
}
