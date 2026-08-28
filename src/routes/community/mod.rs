pub mod discover;
pub mod slug;

use actix_web::{
    HttpResponse, Responder,
    web::{Data, Json, Query},
};
use serde::{Deserialize, Serialize};
use sqlx::{PgExecutor, PgPool, Postgres, QueryBuilder};
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

use std::sync::Arc;

use crate::{
    constants::MAX_COMMUNITIES_PER_USER,
    routes::{
        community::slug::{CommunityResponse, CommunityTarget, fetch_detail},
        profile::parse_search_cursor,
    },
    services::{
        activity::{NewActivity, record_activity},
        auth::SessionData,
        slug::create_community,
        storage::{StorageService, public_url_of},
    },
    utils::{
        ApiCursorPaginatedResponse, ApiErrorResponse, ApiResponse, CursorPaginationQuery, db_err,
        invalid_field, take_page, unauthorized, validate_trimmed_string,
    },
};

const MIN_SEARCH_LENGTH: usize = 2;

/// Espelha o enum `community_category` do banco, na ordem do índice.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[sqlx(type_name = "community_category", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum CommunityCategory {
    AlunosEExAlunos,
    ArtesEEntretenimento,
    Atividades,
    CidadesEBairros,
    ComputadoresEInternet,
    Culinaria,
    Empresas,
    Escolas,
    EsportesELazer,
    EstiloDeVida,
    FamiliasELar,
    FilosofiaEReligiao,
    GovernoEPolitica,
    HistoriaECiencia,
    Jogos,
    Musica,
    PaisesERegionalismo,
    Pessoas,
    SaudeEBemEstar,
    Viagens,
    Outros,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[sqlx(type_name = "community_role", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum CommunityRole {
    Member,
    Admin,
    Owner,
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct CommunitySummary {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub avatar_url: Option<String>,
    pub category: CommunityCategory,
    pub is_private: bool,
    /// Conta apenas membros de contas ativas, como a listagem de membros.
    pub members_count: i64,
}

#[derive(Debug, Deserialize, ToSchema, Validate)]
pub struct CommunityRequest {
    #[validate(
        length(
            min = 1,
            max = 100,
            message = "O nome deve ter entre 1 e 100 caracteres"
        ),
        custom(function = "validate_trimmed_string")
    )]
    #[schema(example = "Eu odeio acordar cedo")]
    pub name: String,

    #[validate(length(max = 2000))]
    pub description: Option<String>,

    /// Categoria do índice de descoberta. Obrigatória: é por ela que a
    /// comunidade é encontrada por quem não sabe o nome.
    pub category: CommunityCategory,

    /// Comunidade privada esconde a lista de membros de quem não é membro e
    /// exige aprovação para entrar.
    #[serde(default)]
    pub is_private: bool,
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    /// Termo buscado por similaridade no nome ou por prefixo do slug. Sem ele, a
    /// resposta traz o índice, que aceita filtro e ordenação.
    pub q: Option<String>,
    /// Categoria do índice. Ignorada quando há busca por termo.
    pub category: Option<CommunityCategory>,
    /// `created_at_desc` (padrão), `created_at_asc`, `members_desc` ou
    /// `members_asc`. Uma por vez; repetindo o parâmetro, vale o último.
    pub sort: Option<String>,
}

/// Recebe um executor genérico para poder rodar dentro da transação que aprova
/// um pedido de entrada, junto da escrita que cria o membro.
pub async fn is_at_community_limit(
    executor: impl PgExecutor<'_>,
    user_id: Uuid,
) -> Result<bool, actix_web::Error> {
    let count = sqlx::query_scalar!(
        r#"
        SELECT COUNT(*) AS "count!"
        FROM community_member
        WHERE user_id = $1
        "#,
        user_id,
    )
    .fetch_one(executor)
    .await
    .map_err(db_err)?;

    Ok(count >= MAX_COMMUNITIES_PER_USER)
}

pub fn community_limit_reached() -> HttpResponse {
    let message = format!("Você já participa de {MAX_COMMUNITIES_PER_USER} comunidades.");

    HttpResponse::Conflict().json(ApiErrorResponse::from_single_validation_error(
        "community",
        "community_limit_reached",
        Some(&message),
        vec![],
    ))
}

pub fn member_community_limit_reached() -> HttpResponse {
    let message = format!("Esse usuário já participa de {MAX_COMMUNITIES_PER_USER} comunidades.");

    HttpResponse::Conflict().json(ApiErrorResponse::from_single_validation_error(
        "member",
        "community_limit_reached",
        Some(&message),
        vec![],
    ))
}

async fn search_communities(
    pool: &PgPool,
    storage: &dyn StorageService,
    term: &str,
    cursor: Option<&str>,
    limit: i64,
) -> Result<(Vec<CommunitySummary>, Option<String>), actix_web::Error> {
    let (cursor_score, cursor_id) = match parse_search_cursor(cursor) {
        Some((score, id)) => (Some(score), Some(id)),
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
                SELECT COUNT(*) FROM community_member cm
                INNER JOIN auth_user au ON au.id = cm.user_id AND au.status = 'active'
                WHERE cm.community_id = c.id
            ) AS "members_count!",
            similarity(c.name, $1) AS "score!"
        FROM community c
        WHERE (c.name % $1 OR c.slug LIKE $1 || '%')
          AND c.deleted_at IS NULL
          AND (
              $2::real IS NULL
              OR (similarity(c.name, $1), c.id) < ($2::real, $3::uuid)
          )
        ORDER BY similarity(c.name, $1) DESC, c.id DESC
        LIMIT $4
        "#,
        term,
        cursor_score,
        cursor_id,
        limit + 1,
    )
    .fetch_all(pool)
    .await
    .map_err(db_err)?;

    let next_cursor = take_page(&mut rows, limit, |row| format!("{}:{}", row.score, row.id));

    let results = rows
        .into_iter()
        .map(|row| CommunitySummary {
            id: row.id,
            slug: row.slug,
            name: row.name,
            description: row.description,
            avatar_url: public_url_of(storage, row.avatar_url),
            category: row.category,
            is_private: row.is_private,
            members_count: row.members_count,
        })
        .collect();

    Ok((results, next_cursor))
}

/// As ordenações aceitas na listagem. O id é UUIDv7, então ordenar por ele é o
/// mesmo que ordenar por data de criação, sem precisar tocar em `created_at`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum CommunitySort {
    CreatedAtDesc,
    CreatedAtAsc,
    MembersDesc,
    MembersAsc,
}

impl CommunitySort {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "created_at_desc" => Some(CommunitySort::CreatedAtDesc),
            "created_at_asc" => Some(CommunitySort::CreatedAtAsc),
            "members_desc" => Some(CommunitySort::MembersDesc),
            "members_asc" => Some(CommunitySort::MembersAsc),
            _ => None,
        }
    }

    /// `true` quando o cursor precisa carregar a contagem junto do id, porque só
    /// o id não identifica a posição na página.
    fn counts_in_cursor(self) -> bool {
        matches!(self, CommunitySort::MembersDesc | CommunitySort::MembersAsc)
    }
}

/// O cursor da ordenação por membros carrega a contagem junto do id.
fn parse_members_cursor(cursor: Option<&str>) -> Option<(i64, Uuid)> {
    let (count, id) = cursor?.split_once(':')?;
    Some((count.parse().ok()?, id.parse().ok()?))
}

/// Listagem do índice: filtra por categoria e ordena pelos quatro critérios.
/// A consulta é montada à mão porque as macros não constroem `ORDER BY`.
async fn list_communities(
    pool: &PgPool,
    storage: &dyn StorageService,
    category: Option<CommunityCategory>,
    sort: CommunitySort,
    cursor: Option<&str>,
    limit: i64,
) -> Result<(Vec<CommunitySummary>, Option<String>), actix_web::Error> {
    let mut query = QueryBuilder::<Postgres>::new(
        r#"
        SELECT * FROM (
            SELECT
                c.id,
                c.slug,
                c.name,
                c.description,
                c.avatar_url,
                c.category,
                c.is_private,
                (
                    SELECT COUNT(*) FROM community_member cm
                    INNER JOIN auth_user au ON au.id = cm.user_id AND au.status = 'active'
                    WHERE cm.community_id = c.id
                ) AS members_count
            FROM community c
            WHERE c.deleted_at IS NULL
        "#,
    );

    if let Some(category) = category {
        query.push(" AND c.category = ");
        query.push_bind(category);
    }

    query.push(") t WHERE TRUE");

    match sort {
        CommunitySort::CreatedAtDesc | CommunitySort::CreatedAtAsc => {
            if let Some(id) = cursor.and_then(|cursor| cursor.parse::<Uuid>().ok()) {
                let comparison = if sort == CommunitySort::CreatedAtDesc {
                    " AND t.id < "
                } else {
                    " AND t.id > "
                };
                query.push(comparison);
                query.push_bind(id);
            }
        }
        CommunitySort::MembersDesc | CommunitySort::MembersAsc => {
            if let Some((count, id)) = parse_members_cursor(cursor) {
                let comparison = if sort == CommunitySort::MembersDesc {
                    " AND (t.members_count, t.id) < ("
                } else {
                    " AND (t.members_count, t.id) > ("
                };
                query.push(comparison);
                query.push_bind(count);
                query.push(", ");
                query.push_bind(id);
                query.push(")");
            }
        }
    }

    query.push(match sort {
        CommunitySort::CreatedAtDesc => " ORDER BY t.id DESC",
        CommunitySort::CreatedAtAsc => " ORDER BY t.id ASC",
        CommunitySort::MembersDesc => " ORDER BY t.members_count DESC, t.id DESC",
        CommunitySort::MembersAsc => " ORDER BY t.members_count ASC, t.id ASC",
    });

    query.push(" LIMIT ");
    query.push_bind(limit + 1);

    let mut results: Vec<CommunitySummary> = query
        .build_query_as()
        .fetch_all(pool)
        .await
        .map_err(db_err)?;

    for community in &mut results {
        community.avatar_url = public_url_of(storage, community.avatar_url.take());
    }

    let counts_in_cursor = sort.counts_in_cursor();
    let next_cursor = take_page(&mut results, limit, |community| {
        if counts_in_cursor {
            format!("{}:{}", community.members_count, community.id)
        } else {
            community.id.to_string()
        }
    });

    Ok((results, next_cursor))
}

#[utoipa::path(
    get,
    path = "/api/v1/community",
    description = "Busca comunidades por nome (similaridade) ou prefixo do slug. Sem o termo, devolve o índice, que aceita filtro por categoria e ordenação",
    params(
        ("q" = Option<String>, Query, description = "Termo buscado, mínimo de 2 caracteres"),
        ("category" = Option<CommunityCategory>, Query, description = "Categoria do índice, ignorada quando há busca por termo"),
        ("sort" = Option<String>, Query, description = "created_at_desc (padrão), created_at_asc, members_desc ou members_asc"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Comunidades encontradas", body = ApiCursorPaginatedResponse<Vec<CommunitySummary>>),
        (status = 400, description = "Termo de busca muito curto", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Busca comunidades por nome ou slug", skip_all)]
pub async fn get(
    query: Query<SearchQuery>,
    pagination: Query<CursorPaginationQuery>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let limit = pagination.limit();
    let term = query
        .q
        .as_deref()
        .map(|term| term.trim().to_lowercase())
        .filter(|term| !term.is_empty());

    let sort = match query.sort.as_deref() {
        None => CommunitySort::CreatedAtDesc,
        Some(value) => match CommunitySort::parse(value) {
            Some(sort) => sort,
            None => {
                return Ok(invalid_field(
                    "sort",
                    "invalid_sort",
                    "A ordenação precisa ser 'created_at_desc', 'created_at_asc', 'members_desc' ou 'members_asc'.",
                ));
            }
        },
    };

    let (results, next_cursor) = match term {
        Some(term) if term.chars().count() >= MIN_SEARCH_LENGTH => {
            search_communities(
                pool.get_ref(),
                storage.as_ref().as_ref(),
                &term,
                pagination.cursor.as_deref(),
                limit,
            )
            .await?
        }
        Some(_) => {
            return Ok(invalid_field(
                "q",
                "too_short",
                "A busca precisa de pelo menos 2 caracteres.",
            ));
        }
        None => {
            list_communities(
                pool.get_ref(),
                storage.as_ref().as_ref(),
                query.category,
                sort,
                pagination.cursor.as_deref(),
                limit,
            )
            .await?
        }
    };

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(results, next_cursor)))
}

#[utoipa::path(
    post,
    path = "/api/v1/community",
    description = "Cria uma comunidade. O slug é derivado do nome e não muda depois, e quem cria vira o dono",
    request_body = CommunityRequest,
    responses(
        (status = 201, description = "Comunidade criada", body = ApiResponse<CommunityResponse>),
        (status = 400, description = "Dados inválidos", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 409, description = "Limite de comunidades atingido", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Cria uma comunidade", skip_all)]
pub async fn post(
    payload: Json<CommunityRequest>,
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

    if is_at_community_limit(pool.get_ref(), session.user_id).await? {
        return Ok(community_limit_reached());
    }

    let description = payload
        .description
        .as_deref()
        .map(str::trim)
        .filter(|description| !description.is_empty());

    let mut tx = pool.begin().await.map_err(db_err)?;

    let (id, slug) = create_community(
        &mut tx,
        payload.name.trim(),
        description,
        payload.category,
        payload.is_private,
    )
    .await?;

    sqlx::query!(
        r#"
        INSERT INTO community_member (community_id, user_id, role)
        VALUES ($1, $2, 'owner')
        "#,
        id,
        session.user_id,
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;

    // Comunidade privada não entra no feed: quem não é membro não veria mesmo.
    if !payload.is_private {
        record_activity(
            &mut *tx,
            NewActivity::community_created(session.user_id, id),
        )
        .await
        .map_err(db_err)?;
    }

    tx.commit().await.map_err(db_err)?;

    let target = CommunityTarget {
        id,
        slug,
        is_private: payload.is_private,
        viewer: Some(session.user_id),
        role: Some(CommunityRole::Owner),
        has_pending_request: false,
    };

    let community = fetch_detail(pool.get_ref(), storage.as_ref().as_ref(), &target).await?;

    Ok(HttpResponse::Created().json(ApiResponse::new(community)))
}
