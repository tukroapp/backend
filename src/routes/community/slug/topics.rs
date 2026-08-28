use actix_web::{
    HttpResponse, Responder,
    web::{Data, Json, Path, Query},
};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;
use validator::Validate;

use std::sync::Arc;

use crate::{
    routes::{
        community::slug::{CommunityTarget, community_not_found, fetch_community_target},
        profile::ProfileSummary,
        topic::{TopicRequest, TopicResponse, member_required},
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

/// Os fixados vêm inteiros na primeira página, fora do cursor. São poucos por
/// natureza, e colocá-los no keyset complicaria a ordenação sem ganho.
const PINNED_LIMIT: i64 = 20;

/// O cursor carrega a data da última atividade junto do id, porque a ordenação
/// da listagem é por atividade e só o id não identifica a posição na página.
pub fn parse_topic_cursor(cursor: Option<&str>) -> Option<(DateTime<Utc>, Uuid)> {
    let (activity, id) = cursor?.rsplit_once(':')?;
    let activity = DateTime::parse_from_rfc3339(activity)
        .ok()?
        .with_timezone(&Utc);
    Some((activity, id.parse().ok()?))
}

fn topic_cursor_of(topic: &TopicResponse) -> String {
    format!("{}:{}", topic.last_activity_at.to_rfc3339(), topic.id)
}

/// Página de tópicos de uma comunidade, fixados ou não conforme `pinned`. O
/// `target` entra para resolver as permissões de cada linha.
async fn fetch_topics(
    pool: &PgPool,
    storage: &dyn StorageService,
    target: &CommunityTarget,
    pinned: bool,
    cursor: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<TopicResponse>, actix_web::Error> {
    let viewer = target.viewer;
    let can_moderate = target.can_moderate();

    let (cursor_activity, cursor_id) = match cursor {
        Some((activity, id)) => (Some(activity), Some(id)),
        None => (None, None),
    };

    let rows = sqlx::query!(
        r#"
        SELECT
            t.id,
            t.title,
            t.body,
            t.is_pinned,
            t.is_locked,
            t.created_at,
            t.edited_at,
            t.last_activity_at,
            up.user_id AS author_id,
            up.slug AS author_slug,
            up.name AS author_name,
            up.avatar_url AS author_avatar_url,
            up.status AS author_status,
            (
                SELECT COUNT(*) FROM community_topic_comment tc
                WHERE tc.topic_id = t.id AND tc.deleted_at IS NULL
            ) AS "comment_count!"
        FROM community_topic t
        INNER JOIN user_profile up ON up.user_id = t.author_id
        WHERE t.community_id = $1
          AND t.deleted_at IS NULL
          AND t.is_pinned = $2
          AND (
              $3::timestamptz IS NULL
              OR (t.last_activity_at, t.id) < ($3::timestamptz, $4::uuid)
          )
        ORDER BY t.last_activity_at DESC, t.id DESC
        LIMIT $5
        "#,
        target.id,
        pinned,
        cursor_activity,
        cursor_id,
        limit,
    )
    .fetch_all(pool)
    .await
    .map_err(db_err)?;

    Ok(rows
        .into_iter()
        .map(|row| TopicResponse {
            id: row.id,
            community_slug: target.slug.clone(),
            title: row.title,
            body: row.body,
            is_pinned: row.is_pinned,
            is_locked: row.is_locked,
            created_at: row.created_at,
            edited_at: row.edited_at,
            last_activity_at: row.last_activity_at,
            can_edit: viewer == Some(row.author_id),
            can_delete: viewer == Some(row.author_id) || can_moderate,
            can_moderate,
            author: ProfileSummary {
                id: row.author_id,
                slug: row.author_slug,
                name: row.author_name,
                avatar_url: public_url_of(storage, row.author_avatar_url),
                status: row.author_status,
            },
            comment_count: row.comment_count,
        })
        .collect())
}

#[utoipa::path(
    get,
    path = "/api/v1/community/{slug}/topic",
    description = "Lista paginada (cursor) dos tópicos do fórum, com os fixados na primeira página",
    params(
        ("slug" = String, Path, description = "Slug público da comunidade"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Tópicos do fórum", body = ApiCursorPaginatedResponse<Vec<TopicResponse>>),
        (status = 404, description = "Comunidade não encontrada ou privada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Lista os tópicos do fórum de uma comunidade", skip_all)]
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

    // Fórum de comunidade privada não aparece para quem está de fora, e o 404
    // evita confirmar o que existe lá dentro.
    if target.is_private && !target.is_member() {
        return Ok(community_not_found());
    }

    let limit = pagination.limit();
    let cursor = parse_topic_cursor(pagination.cursor.as_deref());
    let storage = storage.as_ref().as_ref();

    let mut topics = if cursor.is_none() {
        fetch_topics(pool.get_ref(), storage, &target, true, None, PINNED_LIMIT).await?
    } else {
        Vec::new()
    };

    // Um tópico a mais do que o pedido: a existência dele é o que diz se há próxima
    // página. Página cheia não serve como sinal, porque ela pode ser o fim da lista.
    let mut rest = fetch_topics(pool.get_ref(), storage, &target, false, cursor, limit + 1).await?;

    // O cursor anda só sobre os não fixados, que são a parte infinita da lista.
    let next_cursor = take_page(&mut rest, limit, topic_cursor_of);

    topics.extend(rest);

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(topics, next_cursor)))
}

#[utoipa::path(
    post,
    path = "/api/v1/community/{slug}/topic",
    description = "Abre um tópico no fórum da comunidade. Exige ser membro",
    params(("slug" = String, Path, description = "Slug público da comunidade")),
    request_body = TopicRequest,
    responses(
        (status = 201, description = "Tópico criado", body = ApiResponse<TopicResponse>),
        (status = 400, description = "Dados inválidos", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Apenas membros abrem tópicos", body = ApiErrorResponse),
        (status = 404, description = "Comunidade não encontrada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Abre um tópico no fórum", skip_all)]
pub async fn post(
    slug: Path<String>,
    payload: Json<TopicRequest>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    // O escopo /community usa autenticação opcional porque a leitura é pública,
    // então a escrita precisa exigir a sessão aqui.
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

    if !target.is_member() {
        return Ok(member_required());
    }

    let row = sqlx::query!(
        r#"
        WITH inserted AS (
            INSERT INTO community_topic (community_id, author_id, title, body)
            VALUES ($1, $2, $3, $4)
            RETURNING id, title, body, is_pinned, is_locked, created_at, edited_at, last_activity_at, author_id
        )
        SELECT
            i.id,
            i.title,
            i.body,
            i.is_pinned,
            i.is_locked,
            i.created_at,
            i.edited_at,
            i.last_activity_at,
            up.user_id AS author_id,
            up.slug AS author_slug,
            up.name AS author_name,
            up.avatar_url AS author_avatar_url,
            up.status AS author_status
        FROM inserted i
        INNER JOIN user_profile up ON up.user_id = i.author_id
        "#,
        target.id,
        session.user_id,
        payload.title.trim(),
        payload.body.trim(),
    )
    .fetch_one(pool.get_ref())
    .await
    .map_err(db_err)?;

    // Só o feed sabe do tópico novo: notificar cada membro da comunidade seria
    // um insert por membro numa requisição só.
    let activity = NewActivity::topic_created(session.user_id, target.id, row.id, &row.title);
    if let Err(error) = record_activity(pool.get_ref(), activity).await {
        tracing::error!(%error, "failed to record the topic activity");
    }

    let topic = TopicResponse {
        id: row.id,
        community_slug: target.slug.clone(),
        title: row.title,
        body: row.body,
        is_pinned: row.is_pinned,
        is_locked: row.is_locked,
        created_at: row.created_at,
        edited_at: row.edited_at,
        last_activity_at: row.last_activity_at,
        author: ProfileSummary {
            id: row.author_id,
            slug: row.author_slug,
            name: row.author_name,
            avatar_url: public_url_of(storage.as_ref().as_ref(), row.author_avatar_url),
            status: row.author_status,
        },
        comment_count: 0,
        can_edit: true,
        can_delete: true,
        can_moderate: target.can_moderate(),
    };

    Ok(HttpResponse::Created().json(ApiResponse::new(topic)))
}
