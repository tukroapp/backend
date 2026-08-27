use actix_web::{
    HttpResponse, Responder,
    web::{Data, Path, Query},
};
use serde::Serialize;
use sqlx::PgPool;
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
        storage::{StorageService, public_url_of},
    },
    utils::{
        ApiCursorPaginatedResponse, ApiErrorResponse, ApiResponse, CursorPaginationQuery, db_err,
        invalid_field, take_page, unauthorized,
    },
};

#[derive(Serialize, ToSchema)]
pub struct FanStatus {
    pub fans_count: i64,
    pub is_fan: bool,
}

/// Conta os fãs de um perfil, ignorando contas inativas para bater com a lista.
async fn fans_count(pool: &PgPool, idol_id: Uuid) -> Result<i64, actix_web::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT COUNT(*) AS "count!"
        FROM fan
        INNER JOIN auth_user au ON au.id = fan.fan_id AND au.status = 'active'
        WHERE fan.idol_id = $1
        "#,
        idol_id,
    )
    .fetch_one(pool)
    .await
    .map_err(db_err)
}

#[utoipa::path(
    get,
    path = "/api/v1/profile/{slug}/fan",
    description = "Lista paginada (cursor) dos fãs do perfil",
    params(
        ("slug" = String, Path, description = "Slug público do perfil"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Fãs do perfil", body = ApiCursorPaginatedResponse<Vec<ProfileSummary>>),
        (status = 404, description = "Perfil não encontrado ou conta inativa", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Lista os fãs de um perfil", skip_all)]
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

    let limit = pagination.limit();

    let mut fans = sqlx::query_as!(
        ProfileSummary,
        r#"
        SELECT
            up.user_id AS id,
            up.slug,
            up.name,
            up.avatar_url,
            up.status
        FROM fan
        INNER JOIN auth_user au ON au.id = fan.fan_id AND au.status = 'active'
        INNER JOIN user_profile up ON up.user_id = fan.fan_id
        WHERE fan.idol_id = $1
          AND ($2::uuid IS NULL OR fan.fan_id < $2::uuid)
        ORDER BY fan.fan_id DESC
        LIMIT $3
        "#,
        target.user_id,
        pagination.cursor_id(),
        limit + 1,
    )
    .fetch_all(pool.get_ref())
    .await
    .map_err(db_err)?;

    for fan in &mut fans {
        fan.avatar_url = public_url_of(storage.as_ref().as_ref(), fan.avatar_url.take());
    }

    let next_cursor = take_page(&mut fans, limit, |fan| fan.id.to_string());

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(fans, next_cursor)))
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
