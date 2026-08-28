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
    routes::profile::{
        ProfileSummary,
        slug::{fetch_target, profile_not_found},
    },
    services::{
        auth::SessionData,
        notification::{NotificationKind, NotificationTarget, notify},
        storage::{StorageService, public_url_of},
    },
    utils::{
        ApiCursorPaginatedResponse, ApiErrorResponse, ApiResponse, CursorPaginationQuery, db_err,
        take_page, unauthorized, validate_markup_body,
    },
};

#[derive(Serialize, ToSchema)]
pub struct ScrapResponse {
    pub id: Uuid,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub author: ProfileSummary,
    /// Quantidade de respostas do recado. Sempre 0 nas próprias respostas,
    /// porque a thread tem um único nível.
    pub reply_count: i64,
}

#[derive(Debug, Deserialize, ToSchema, Validate)]
pub struct ScrapRequest {
    #[validate(
        length(
            min = 1,
            max = 2000,
            message = "O recado deve ter entre 1 e 2000 caracteres"
        ),
        custom(function = "validate_markup_body")
    )]
    pub body: String,
}

pub fn private_scrap_wall() -> HttpResponse {
    HttpResponse::Forbidden().json(ApiErrorResponse::from_single_validation_error(
        "scrap",
        "private_scrap_wall",
        Some("O mural de recados desse perfil é privado."),
        vec![],
    ))
}

pub fn scrap_write_forbidden() -> HttpResponse {
    HttpResponse::Forbidden().json(ApiErrorResponse::from_single_validation_error(
        "scrap",
        "scrap_write_forbidden",
        Some("Você não pode deixar recados nesse perfil."),
        vec![],
    ))
}

/// Recados de um mural ou de uma thread, já com o autor e a contagem de
/// respostas. `parent` é `None` para o mural (só recados de primeiro nível).
pub async fn fetch_scraps(
    pool: &PgPool,
    storage: &dyn StorageService,
    profile_id: Uuid,
    parent: Option<Uuid>,
    cursor: Option<Uuid>,
    limit: i64,
) -> Result<Vec<ScrapResponse>, actix_web::Error> {
    // Mural e thread usam a mesma ordem, do mais novo para o mais antigo. O id
    // é UUIDv7, então ordenar por ele é o mesmo que ordenar por data.
    let rows = sqlx::query!(
        r#"
        SELECT
            s.id,
            s.body,
            s.created_at,
            s.updated_at,
            up.user_id AS author_id,
            up.slug AS author_slug,
            up.name AS author_name,
            up.avatar_url AS author_avatar_url,
            up.status AS author_status,
            (
                SELECT COUNT(*) FROM scrap r
                WHERE r.parent_id = s.id AND r.deleted_at IS NULL
            ) AS "reply_count!"
        FROM scrap s
        INNER JOIN user_profile up ON up.user_id = s.author_id
        -- Recado de quem está suspenso ou desativado some do mural alheio.
        -- Recado de conta desativada some do mural; o de conta excluída fica,
        -- assinado pelo perfil anonimizado, para a conversa não perder o fio.
        INNER JOIN auth_user au ON au.id = s.author_id AND au.status IN ('active', 'deleted')
        WHERE s.profile_id = $1
          AND s.deleted_at IS NULL
          AND s.parent_id IS NOT DISTINCT FROM $2::uuid
          AND ($3::uuid IS NULL OR s.id < $3::uuid)
        ORDER BY s.id DESC
        LIMIT $4
        "#,
        profile_id,
        parent,
        cursor,
        limit,
    )
    .fetch_all(pool)
    .await
    .map_err(db_err)?;

    Ok(rows
        .into_iter()
        .map(|row| ScrapResponse {
            id: row.id,
            body: row.body,
            created_at: row.created_at,
            updated_at: row.updated_at,
            author: ProfileSummary {
                id: row.author_id,
                slug: row.author_slug,
                name: row.author_name,
                avatar_url: public_url_of(storage, row.author_avatar_url),
                status: row.author_status,
            },
            reply_count: row.reply_count,
        })
        .collect())
}

#[utoipa::path(
    get,
    path = "/api/v1/profile/{slug}/scrap",
    description = "Lista paginada (cursor) do mural de recados do perfil, respeitando a privacidade do dono",
    params(
        ("slug" = String, Path, description = "Slug público do perfil"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Recados do mural", body = ApiCursorPaginatedResponse<Vec<ScrapResponse>>),
        (status = 403, description = "Mural privado", body = ApiErrorResponse),
        (status = 404, description = "Perfil não encontrado ou conta inativa", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Lista o mural de recados de um perfil", skip_all)]
pub async fn get(
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

    if !target.can_read_scrap {
        return Ok(private_scrap_wall());
    }

    let limit = pagination.limit();
    let mut scraps = fetch_scraps(
        pool.get_ref(),
        storage.as_ref().as_ref(),
        target.user_id,
        None,
        pagination.cursor_id(),
        limit + 1,
    )
    .await?;

    let next_cursor = take_page(&mut scraps, limit, |scrap| scrap.id.to_string());

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(scraps, next_cursor)))
}

#[utoipa::path(
    post,
    path = "/api/v1/profile/{slug}/scrap",
    description = "Publica um recado no mural do perfil, respeitando a privacidade de escrita do dono",
    params(("slug" = String, Path, description = "Slug público do perfil")),
    request_body = ScrapRequest,
    responses(
        (status = 201, description = "Recado publicado", body = ApiResponse<ScrapResponse>),
        (status = 400, description = "Corpo inválido", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Sem permissão para escrever nesse mural", body = ApiErrorResponse),
        (status = 404, description = "Perfil não encontrado ou conta inativa", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Publica um recado no mural", skip_all)]
pub async fn post(
    slug: Path<String>,
    payload: Json<ScrapRequest>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    // O escopo /profile usa autenticação opcional porque a leitura é pública,
    // então a escrita precisa exigir a sessão aqui.
    let Some(session) = session else {
        return Ok(unauthorized());
    };
    session.require_verified_email(pool.get_ref()).await?;

    if let Err(errors) = payload.validate() {
        return Ok(HttpResponse::BadRequest().json(ApiErrorResponse::from(errors)));
    }

    let Some(target) =
        fetch_target(pool.get_ref(), &slug.into_inner(), Some(session.user_id)).await?
    else {
        return Ok(profile_not_found());
    };

    if !target.can_write_scrap {
        return Ok(scrap_write_forbidden());
    }

    let scrap = create_scrap(
        pool.get_ref(),
        storage.as_ref().as_ref(),
        target.user_id,
        session.user_id,
        None,
        payload.body.trim(),
    )
    .await?;

    notify(
        pool.get_ref(),
        target.user_id,
        session.user_id,
        NotificationKind::ScrapReceived,
        NotificationTarget::Scrap(scrap.id),
    )
    .await
    .map_err(db_err)?;

    Ok(HttpResponse::Created().json(ApiResponse::new(scrap)))
}

/// Insere o recado e devolve ele já montado com o autor, evitando um SELECT extra.
pub async fn create_scrap(
    pool: &PgPool,
    storage: &dyn StorageService,
    profile_id: Uuid,
    author_id: Uuid,
    parent_id: Option<Uuid>,
    body: &str,
) -> Result<ScrapResponse, actix_web::Error> {
    let row = sqlx::query!(
        r#"
        WITH inserted AS (
            INSERT INTO scrap (profile_id, author_id, parent_id, body)
            VALUES ($1, $2, $3, $4)
            RETURNING id, body, created_at, updated_at, author_id
        )
        SELECT
            i.id,
            i.body,
            i.created_at,
            i.updated_at,
            up.user_id AS author_id,
            up.slug AS author_slug,
            up.name AS author_name,
            up.avatar_url AS author_avatar_url,
            up.status AS author_status
        FROM inserted i
        INNER JOIN user_profile up ON up.user_id = i.author_id
        "#,
        profile_id,
        author_id,
        parent_id,
        body,
    )
    .fetch_one(pool)
    .await
    .map_err(db_err)?;

    Ok(ScrapResponse {
        id: row.id,
        body: row.body,
        created_at: row.created_at,
        updated_at: row.updated_at,
        author: ProfileSummary {
            id: row.author_id,
            slug: row.author_slug,
            name: row.author_name,
            avatar_url: public_url_of(storage, row.author_avatar_url),
            status: row.author_status,
        },
        reply_count: 0,
    })
}
