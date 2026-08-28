use actix_web::{
    HttpResponse, Responder,
    web::{Data, Json, Path, Query},
};
use chrono::Utc;
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;
use validator::Validate;

use std::sync::Arc;

use crate::{
    routes::{
        community::slug::{community_not_found, fetch_community_target},
        poll::{
            PollHead, PollRequest, PollResponse, PollViewer, RawOption, assemble_poll,
            build_poll_response, clean_options, fetch_poll_target, poll_moderator_required,
            validate_choice_bounds,
        },
        profile::ProfileSummary,
    },
    services::{
        activity::{NewActivity, record_activity},
        auth::SessionData,
        storage::{StorageService, public_url_of},
    },
    utils::{
        ApiCursorPaginatedResponse, ApiErrorResponse, ApiResponse, CursorPaginationQuery, db_err,
        take_page, unauthorized,
    },
};

#[utoipa::path(
    get,
    path = "/api/v1/community/{slug}/poll",
    description = "Lista paginada (cursor) das enquetes da comunidade, da mais recente para a mais antiga",
    params(
        ("slug" = String, Path, description = "Slug público da comunidade"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Enquetes da comunidade", body = ApiCursorPaginatedResponse<Vec<PollResponse>>),
        (status = 404, description = "Comunidade não encontrada ou privada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Lista as enquetes de uma comunidade", skip_all)]
pub async fn get(
    slug: Path<String>,
    pagination: Query<CursorPaginationQuery>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let viewer = session.map(|session| session.user_id);

    let Some(target) = fetch_community_target(pool.get_ref(), &slug.into_inner(), viewer).await?
    else {
        return Ok(community_not_found());
    };

    if target.is_private && !target.is_member() {
        return Ok(community_not_found());
    }

    let limit = pagination.limit();
    let cursor = pagination.cursor_id();

    let heads = sqlx::query!(
        r#"
        SELECT
            p.id,
            p.question,
            p.min_choices,
            p.max_choices,
            p.is_closed,
            p.closes_at,
            p.created_at,
            up.user_id AS author_id,
            up.slug AS author_slug,
            up.name AS author_name,
            up.avatar_url AS author_avatar_url,
            up.status AS author_status,
            p.edited_at,
            (
                SELECT COUNT(DISTINCT v.user_id) FROM community_poll_vote v WHERE v.poll_id = p.id
            ) AS "total_votes!",
            EXISTS (
                SELECT 1 FROM community_poll_vote v
                WHERE v.poll_id = p.id AND v.user_id = $2::uuid
            ) AS "viewer_voted!"
        FROM community_poll p
        INNER JOIN user_profile up ON up.user_id = p.author_id
        WHERE p.community_id = $1
          AND p.deleted_at IS NULL
          AND ($3::uuid IS NULL OR p.id < $3::uuid)
        ORDER BY p.id DESC
        LIMIT $4
        "#,
        target.id,
        viewer,
        cursor,
        limit + 1,
    )
    .fetch_all(pool.get_ref())
    .await
    .map_err(db_err)?;

    // O superusuário modera qualquer comunidade sem entrar nela. A `CommunityTarget`
    // não carrega isso, então resolvemos aqui para a listagem bater com o GET de
    // uma enquete.
    let is_superuser = match viewer {
        Some(user_id) => sqlx::query_scalar!(
            r#"SELECT is_superuser FROM auth_user WHERE id = $1"#,
            user_id,
        )
        .fetch_optional(pool.get_ref())
        .await
        .map_err(db_err)?
        .unwrap_or(false),
        None => false,
    };

    let poll_ids: Vec<Uuid> = heads.iter().map(|row| row.id).collect();

    let option_rows = sqlx::query!(
        r#"
        SELECT
            o.poll_id,
            o.id,
            o.position,
            o.text,
            COUNT(v.user_id) AS "votes!",
            COALESCE(BOOL_OR(v.user_id = $2::uuid), FALSE) AS "mine!"
        FROM community_poll_option o
        LEFT JOIN community_poll_vote v ON v.option_id = o.id
        WHERE o.poll_id = ANY($1)
        GROUP BY o.poll_id, o.id, o.position, o.text
        ORDER BY o.poll_id, o.position
        "#,
        &poll_ids,
        viewer,
    )
    .fetch_all(pool.get_ref())
    .await
    .map_err(db_err)?;

    let mut options_by_poll: HashMap<Uuid, Vec<RawOption>> = HashMap::new();
    for row in option_rows {
        options_by_poll
            .entry(row.poll_id)
            .or_default()
            .push(RawOption {
                id: row.id,
                position: row.position,
                text: row.text,
                votes: row.votes,
                mine: row.mine,
            });
    }

    let storage = storage.as_ref().as_ref();
    let now = Utc::now();
    let can_moderate = target.can_moderate() || is_superuser;
    let is_member = target.is_member();

    let mut polls: Vec<PollResponse> = heads
        .into_iter()
        .map(|row| {
            let viewer_facts = PollViewer {
                is_author: viewer == Some(row.author_id),
                can_moderate,
                is_member,
                effective_closed: row.is_closed || now >= row.closes_at,
                total_votes: row.total_votes,
                viewer_voted: row.viewer_voted,
            };

            assemble_poll(
                PollHead {
                    id: row.id,
                    community_slug: target.slug.clone(),
                    question: row.question,
                    min_choices: row.min_choices,
                    max_choices: row.max_choices,
                    closes_at: row.closes_at,
                    created_at: row.created_at,
                    edited_at: row.edited_at,
                    author: ProfileSummary {
                        id: row.author_id,
                        slug: row.author_slug,
                        name: row.author_name,
                        avatar_url: public_url_of(storage, row.author_avatar_url),
                        status: row.author_status,
                    },
                },
                options_by_poll.remove(&row.id).unwrap_or_default(),
                &viewer_facts,
            )
        })
        .collect();

    let next_cursor = take_page(&mut polls, limit, |poll| poll.id.to_string());

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(polls, next_cursor)))
}

#[utoipa::path(
    post,
    path = "/api/v1/community/{slug}/poll",
    description = "Cria uma enquete na comunidade. Só moderadores e o dono criam, e o encerramento é fixo em 24h",
    params(("slug" = String, Path, description = "Slug público da comunidade")),
    request_body = PollRequest,
    responses(
        (status = 201, description = "Enquete criada", body = ApiResponse<PollResponse>),
        (status = 400, description = "Dados inválidos", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Apenas moderadores e o dono criam enquetes", body = ApiErrorResponse),
        (status = 404, description = "Comunidade não encontrada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Cria uma enquete na comunidade", skip_all)]
pub async fn post(
    slug: Path<String>,
    payload: Json<PollRequest>,
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

    let Some(target) =
        fetch_community_target(pool.get_ref(), &slug.into_inner(), Some(session.user_id)).await?
    else {
        return Ok(community_not_found());
    };

    if target.is_private && !target.is_member() {
        return Ok(community_not_found());
    }

    if !target.can_moderate() {
        return Ok(poll_moderator_required());
    }

    let options = match clean_options(&payload.options) {
        Ok(options) => options,
        Err(response) => return Ok(response),
    };

    if let Err(response) =
        validate_choice_bounds(payload.min_choices, payload.max_choices, options.len())
    {
        return Ok(response);
    }

    let mut tx = pool.begin().await.map_err(db_err)?;

    let poll_id = sqlx::query_scalar!(
        r#"
        INSERT INTO community_poll (community_id, author_id, question, min_choices, max_choices, closes_at)
        VALUES ($1, $2, $3, $4, $5, NOW() + INTERVAL '24 hours')
        RETURNING id
        "#,
        target.id,
        session.user_id,
        payload.question.trim(),
        payload.min_choices,
        payload.max_choices,
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(db_err)?;

    sqlx::query!(
        r#"
        INSERT INTO community_poll_option (poll_id, position, text)
        SELECT $1, (ordinality - 1)::smallint, opt
        FROM UNNEST($2::text[]) WITH ORDINALITY AS t(opt, ordinality)
        "#,
        poll_id,
        &options,
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;

    tx.commit().await.map_err(db_err)?;

    // Mesma regra do tópico: entra no feed de quem segue, não vira notificação
    // para a comunidade inteira.
    let activity =
        NewActivity::poll_created(session.user_id, target.id, poll_id, payload.question.trim());
    if let Err(error) = record_activity(pool.get_ref(), activity).await {
        tracing::error!(%error, "failed to record the poll activity");
    }

    let poll_target = fetch_poll_target(pool.get_ref(), poll_id, Some(session.user_id))
        .await?
        .ok_or_else(|| db_err(sqlx::Error::RowNotFound))?;

    let poll = build_poll_response(pool.get_ref(), storage.as_ref().as_ref(), &poll_target).await?;

    Ok(HttpResponse::Created().json(ApiResponse::new(poll)))
}
