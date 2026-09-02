use actix_web::{
    HttpResponse, Responder,
    web::{Data, Path, Query},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, QueryBuilder};
use utoipa::ToSchema;
use uuid::Uuid;

use std::sync::Arc;

use crate::{
    routes::profile::{
        ProfileSummary,
        slug::{fetch_target, profile_not_found},
    },
    services::{
        activity::{NewActivity, record_activity},
        auth::SessionData,
        notification::notify_new_fan,
        storage::{StorageService, public_url_of},
    },
    utils::{
        ApiCursorPaginatedResponse, ApiErrorResponse, ApiResponse, CursorPaginationQuery, db_err,
        invalid_field, take_page, unauthorized,
    },
};

/// Um fã da listagem: o perfil dele, desde quando é fã e quantos fãs ele
/// próprio tem, que é o número usado pela ordenação por popularidade.
#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct FanSummary {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub avatar_url: Option<String>,
    pub status: Option<String>,
    /// Quando virou fã desse perfil.
    pub fan_since: DateTime<Utc>,
    /// Quantos fãs esse fã tem, contando só contas ativas.
    pub fans_count: i64,
}

/// Fica separado de `CursorPaginationQuery` porque `#[serde(flatten)]` faria o
/// serde ler todos os valores da query string como texto, quebrando `limit`.
#[derive(Debug, Deserialize)]
pub struct FansQuery {
    /// Filtro opcional pelo nome (trecho) ou pelo início do slug do fã.
    pub q: Option<String>,
    /// `created_at_desc` (padrão), `created_at_asc`, `fans_desc` ou `fans_asc`.
    pub sort: Option<String>,
}

/// As ordenações aceitas na lista de fãs.
#[derive(Debug, Clone, Copy, PartialEq)]
enum FanSort {
    CreatedAtDesc,
    CreatedAtAsc,
    FansDesc,
    FansAsc,
}

impl FanSort {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "created_at_desc" => Some(FanSort::CreatedAtDesc),
            "created_at_asc" => Some(FanSort::CreatedAtAsc),
            "fans_desc" => Some(FanSort::FansDesc),
            "fans_asc" => Some(FanSort::FansAsc),
            _ => None,
        }
    }

    fn is_descending(self) -> bool {
        matches!(self, FanSort::CreatedAtDesc | FanSort::FansDesc)
    }
}

/// O cursor carrega a chave da ordenação junto do id, porque nenhuma das quatro
/// ordena pelo id sozinho. A chave da data vai em microssegundos para caber num
/// inteiro e não brigar com o `:` que separa os dois campos.
fn parse_fan_cursor(cursor: Option<&str>) -> Option<(i64, Uuid)> {
    let (key, id) = cursor?.split_once(':')?;
    Some((key.parse().ok()?, id.parse().ok()?))
}

#[derive(Serialize, ToSchema)]
pub struct FanStatus {
    pub fans_count: i64,
    pub is_fan: bool,
}

/// A contagem desnormalizada de fãs do perfil, mantida por trigger.
async fn fans_count(pool: &PgPool, idol_id: Uuid) -> Result<i64, actix_web::Error> {
    sqlx::query_scalar!(
        r#"SELECT fans_count FROM user_profile WHERE user_id = $1"#,
        idol_id,
    )
    .fetch_one(pool)
    .await
    .map_err(db_err)
}

#[utoipa::path(
    get,
    path = "/api/v1/profile/{slug}/fan",
    description = "Lista paginada (cursor) dos fãs do perfil, com filtro por nome e ordenação",
    params(
        ("slug" = String, Path, description = "Slug público do perfil"),
        ("q" = Option<String>, Query, description = "Filtra os fãs por trecho do nome ou início do slug"),
        ("sort" = Option<String>, Query, description = "created_at_desc (padrão), created_at_asc, fans_desc ou fans_asc"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Fãs do perfil", body = ApiCursorPaginatedResponse<Vec<FanSummary>>),
        (status = 400, description = "Ordenação inválida", body = ApiErrorResponse),
        (status = 404, description = "Perfil não encontrado ou conta inativa", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Lista os fãs de um perfil", skip_all)]
pub async fn get(
    slug: Path<String>,
    query: Query<FansQuery>,
    pagination: Query<CursorPaginationQuery>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let viewer = session.map(|session| session.user_id);

    let Some(target) = fetch_target(pool.get_ref(), &slug.into_inner(), viewer).await? else {
        return Ok(profile_not_found());
    };

    let sort = match query.sort.as_deref() {
        None => FanSort::CreatedAtDesc,
        Some(value) => match FanSort::parse(value) {
            Some(sort) => sort,
            None => {
                return Ok(invalid_field(
                    "sort",
                    "invalid_sort",
                    "A ordenação precisa ser 'created_at_desc', 'created_at_asc', 'fans_desc' ou 'fans_asc'.",
                ));
            }
        },
    };

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

    // A consulta é montada à mão porque as macros não constroem `ORDER BY`.
    let mut sql = QueryBuilder::<Postgres>::new(
        r#"
        SELECT * FROM (
            SELECT
                up.user_id AS id,
                up.slug,
                up.name,
                up.avatar_url,
                up.status,
                fan.created_at AS fan_since,
                up.fans_count
            FROM fan
            INNER JOIN auth_user au ON au.id = fan.fan_id AND au.status = 'active'
            INNER JOIN user_profile up ON up.user_id = fan.fan_id
            WHERE fan.idol_id = "#,
    );
    sql.push_bind(target.user_id);

    if let Some(term) = &term {
        sql.push(" AND (up.name ILIKE '%' || ");
        sql.push_bind(term);
        sql.push(" || '%' OR up.slug LIKE ");
        sql.push_bind(term);
        sql.push(" || '%')");
    }

    sql.push(") t WHERE TRUE");

    if let Some((key, id)) = parse_fan_cursor(pagination.cursor.as_deref()) {
        let comparison = if sort.is_descending() { " < (" } else { " > (" };

        match sort {
            FanSort::CreatedAtDesc | FanSort::CreatedAtAsc => {
                sql.push(" AND (t.fan_since, t.id)");
                sql.push(comparison);
                sql.push_bind(DateTime::from_timestamp_micros(key).unwrap_or(DateTime::UNIX_EPOCH));
            }
            FanSort::FansDesc | FanSort::FansAsc => {
                sql.push(" AND (t.fans_count, t.id)");
                sql.push(comparison);
                sql.push_bind(key);
            }
        }

        sql.push(", ");
        sql.push_bind(id);
        sql.push(")");
    }

    sql.push(match sort {
        FanSort::CreatedAtDesc => " ORDER BY t.fan_since DESC, t.id DESC",
        FanSort::CreatedAtAsc => " ORDER BY t.fan_since ASC, t.id ASC",
        FanSort::FansDesc => " ORDER BY t.fans_count DESC, t.id DESC",
        FanSort::FansAsc => " ORDER BY t.fans_count ASC, t.id ASC",
    });

    sql.push(" LIMIT ");
    sql.push_bind(limit + 1);

    let mut fans: Vec<FanSummary> = sql
        .build_query_as()
        .fetch_all(pool.get_ref())
        .await
        .map_err(db_err)?;

    for fan in &mut fans {
        fan.avatar_url = public_url_of(storage.as_ref().as_ref(), fan.avatar_url.take());
    }

    let next_cursor = take_page(&mut fans, limit, |fan| match sort {
        FanSort::CreatedAtDesc | FanSort::CreatedAtAsc => {
            format!("{}:{}", fan.fan_since.timestamp_micros(), fan.id)
        }
        FanSort::FansDesc | FanSort::FansAsc => format!("{}:{}", fan.fans_count, fan.id),
    });

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(fans, next_cursor)))
}

#[utoipa::path(
    get,
    path = "/api/v1/profile/{slug}/idol",
    description = "Lista paginada (cursor) de quem esse perfil é fã, o outro lado do /fan",
    params(
        ("slug" = String, Path, description = "Slug público do perfil"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Ídolos do perfil", body = ApiCursorPaginatedResponse<Vec<ProfileSummary>>),
        (status = 404, description = "Perfil não encontrado ou conta inativa", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Lista os ídolos de um perfil", skip_all)]
pub async fn idols(
    slug: Path<String>,
    pagination: Query<CursorPaginationQuery>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let viewer = session.map(|session| session.user_id);

    let Some(target) = fetch_target(pool.get_ref(), &slug.into_inner(), viewer).await? else {
        return Ok(profile_not_found());
    };

    let limit = pagination.limit();

    let mut idols = sqlx::query_as!(
        ProfileSummary,
        r#"
        SELECT
            up.user_id AS id,
            up.slug,
            up.name,
            up.avatar_url,
            up.status
        FROM fan
        INNER JOIN auth_user au ON au.id = fan.idol_id AND au.status = 'active'
        INNER JOIN user_profile up ON up.user_id = fan.idol_id
        WHERE fan.fan_id = $1
          AND ($2::uuid IS NULL OR fan.idol_id < $2::uuid)
        ORDER BY fan.idol_id DESC
        LIMIT $3
        "#,
        target.user_id,
        pagination.cursor_id(),
        limit + 1,
    )
    .fetch_all(pool.get_ref())
    .await
    .map_err(db_err)?;

    for idol in &mut idols {
        idol.avatar_url = public_url_of(storage.as_ref().as_ref(), idol.avatar_url.take());
    }

    let next_cursor = take_page(&mut idols, limit, |idol| idol.id.to_string());

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(idols, next_cursor)))
}

#[utoipa::path(
    post,
    path = "/api/v1/profile/{slug}/fan",
    description = "Torna quem está logado fã do perfil. Idempotente, não exige amizade",
    params(("slug" = String, Path, description = "Slug público do perfil")),
    responses(
        (status = 200, description = "Agora é fã, com a contagem atualizada", body = ApiResponse<FanStatus>),
        (status = 400, description = "Fã de si mesmo", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Perfil não encontrado ou conta inativa", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Torna-se fã de um perfil", skip_all)]
pub async fn post(
    slug: Path<String>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };
    session.require_verified_email(pool.get_ref()).await?;

    let Some(target) =
        fetch_target(pool.get_ref(), &slug.into_inner(), Some(session.user_id)).await?
    else {
        return Ok(profile_not_found());
    };

    if target.user_id == session.user_id {
        return Ok(invalid_field(
            "fan",
            "self_fan",
            "Você não pode ser fã de si mesmo.",
        ));
    }

    let inserted = sqlx::query!(
        r#"
        INSERT INTO fan (fan_id, idol_id)
        VALUES ($1, $2)
        ON CONFLICT DO NOTHING
        "#,
        session.user_id,
        target.user_id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?
    .rows_affected();

    // Só a virada real vira atividade, não o clique repetido.
    if inserted == 1 {
        record_activity(
            pool.get_ref(),
            NewActivity::became_fan(session.user_id, target.user_id),
        )
        .await
        .map_err(db_err)?;

        notify_new_fan(pool.get_ref(), target.user_id, session.user_id)
            .await
            .map_err(db_err)?;
    }

    let fans_count = fans_count(pool.get_ref(), target.user_id).await?;

    Ok(HttpResponse::Ok().json(ApiResponse::new(FanStatus {
        fans_count,
        is_fan: true,
    })))
}

#[utoipa::path(
    delete,
    path = "/api/v1/profile/{slug}/fan",
    description = "Deixa de ser fã do perfil. Idempotente",
    params(("slug" = String, Path, description = "Slug público do perfil")),
    responses(
        (status = 200, description = "Não é mais fã, com a contagem atualizada", body = ApiResponse<FanStatus>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Perfil não encontrado ou conta inativa", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Deixa de ser fã de um perfil", skip_all)]
pub async fn delete(
    slug: Path<String>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    let Some(target) =
        fetch_target(pool.get_ref(), &slug.into_inner(), Some(session.user_id)).await?
    else {
        return Ok(profile_not_found());
    };

    sqlx::query!(
        r#"DELETE FROM fan WHERE fan_id = $1 AND idol_id = $2"#,
        session.user_id,
        target.user_id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?;

    let fans_count = fans_count(pool.get_ref(), target.user_id).await?;

    Ok(HttpResponse::Ok().json(ApiResponse::new(FanStatus {
        fans_count,
        is_fan: false,
    })))
}
