use actix_web::{
    HttpResponse, Responder,
    web::{Data, Path, Query},
};
use chrono::{DateTime, Utc};
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
        auth::SessionData,
        storage::{StorageService, public_url_of},
    },
    utils::{
        ApiCursorPaginatedResponse, ApiErrorResponse, ApiResponse, CursorPaginationQuery, db_err,
        markup_preview, take_page,
    },
};

const PREVIEW_MAX_CHARS: usize = 140;

#[derive(Serialize, ToSchema)]
pub struct LastMessage {
    pub preview: String,
    pub from_me: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
pub struct ConversationSummary {
    pub id: Uuid,
    pub counterpart: ProfileSummary,
    pub last_message: LastMessage,
    pub unread_count: i64,
    pub last_message_at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
pub struct MessageResponse {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub body: String,
    pub from_me: bool,
    pub created_at: DateTime<Utc>,
}

pub fn conversation_not_found() -> HttpResponse {
    HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
        "conversation",
        "not_found",
        Some("Conversa não encontrada."),
        vec![],
    ))
}

/// O cursor carrega a data (de atividade da conversa ou de criação da mensagem)
/// junto do id, porque a ordenação é por data e o id sozinho não fixa a posição.
fn parse_cursor(cursor: Option<&str>) -> Option<(DateTime<Utc>, Uuid)> {
    let (at, id) = cursor?.rsplit_once(':')?;
    let at = DateTime::parse_from_rfc3339(at).ok()?.with_timezone(&Utc);
    Some((at, id.parse().ok()?))
}

async fn is_participant(
    pool: &PgPool,
    conversation_id: Uuid,
    user_id: Uuid,
) -> Result<bool, actix_web::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT EXISTS (
            SELECT 1 FROM conversation_participant
            WHERE conversation_id = $1 AND user_id = $2
        ) AS "e!"
        "#,
        conversation_id,
        user_id,
    )
    .fetch_one(pool)
    .await
    .map_err(db_err)
}

/// Uma conversa, do ponto de vista de `me`. `None` quando `me` não participa dela.
async fn fetch_summary(
    pool: &PgPool,
    storage: &dyn StorageService,
    me: Uuid,
    conversation_id: Uuid,
) -> Result<Option<ConversationSummary>, actix_web::Error> {
    let row = sqlx::query!(
        r#"
        SELECT
            c.id,
            c.last_message_at,
            lm.body AS last_body,
            (lm.sender_id = $1) AS "last_from_me!",
            lm.created_at AS last_created_at,
            other.user_id AS counterpart_id,
            other.slug AS counterpart_slug,
            other.name AS counterpart_name,
            other.avatar_url AS counterpart_avatar_url,
            other.status AS counterpart_status,
            (
                SELECT COUNT(*) FROM message m
                WHERE m.conversation_id = c.id
                  AND m.sender_id <> $1
                  AND m.created_at > cp.last_read_at
            ) AS "unread_count!"
        FROM conversation_participant cp
        INNER JOIN conversation c ON c.id = cp.conversation_id
        INNER JOIN message lm ON lm.id = c.last_message_id
        INNER JOIN user_profile other
            ON other.user_id = CASE WHEN c.user_lo = $1 THEN c.user_hi ELSE c.user_lo END
        INNER JOIN auth_user au ON au.id = other.user_id AND au.status = 'active'
        WHERE cp.user_id = $1 AND c.id = $2
        "#,
        me,
        conversation_id,
    )
    .fetch_optional(pool)
    .await
    .map_err(db_err)?;

    Ok(row.map(|row| ConversationSummary {
        id: row.id,
        counterpart: ProfileSummary {
            id: row.counterpart_id,
            slug: row.counterpart_slug,
            name: row.counterpart_name,
            avatar_url: public_url_of(storage, row.counterpart_avatar_url),
            status: row.counterpart_status,
        },
        last_message: LastMessage {
            preview: markup_preview(&row.last_body, PREVIEW_MAX_CHARS),
            from_me: row.last_from_me,
            created_at: row.last_created_at,
        },
        unread_count: row.unread_count,
        last_message_at: row.last_message_at,
    }))
}

#[utoipa::path(
    get,
    path = "/api/v1/me/conversation",
    description = "Lista as conversas do usuário, da mais recente para a mais antiga. Seguro de pollar",
    params(CursorPaginationQuery),
    responses(
        (status = 200, description = "Conversas", body = ApiCursorPaginatedResponse<Vec<ConversationSummary>>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Lista as conversas do usuário", skip_all)]
pub async fn list(
    pagination: Query<CursorPaginationQuery>,
    session: SessionData,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let me = session.user_id;
    let limit = pagination.limit();
    let cursor = parse_cursor(pagination.cursor.as_deref());
    let (cursor_at, cursor_id) = match cursor {
        Some((at, id)) => (Some(at), Some(id)),
        None => (None, None),
    };

    let rows = sqlx::query!(
        r#"
        SELECT
            c.id,
            c.last_message_at,
            lm.body AS last_body,
            (lm.sender_id = $1) AS "last_from_me!",
            lm.created_at AS last_created_at,
            other.user_id AS counterpart_id,
            other.slug AS counterpart_slug,
            other.name AS counterpart_name,
            other.avatar_url AS counterpart_avatar_url,
            other.status AS counterpart_status,
            (
                SELECT COUNT(*) FROM message m
                WHERE m.conversation_id = c.id
                  AND m.sender_id <> $1
                  AND m.created_at > cp.last_read_at
            ) AS "unread_count!"
        FROM conversation_participant cp
        INNER JOIN conversation c ON c.id = cp.conversation_id
        INNER JOIN message lm ON lm.id = c.last_message_id
        INNER JOIN user_profile other
            ON other.user_id = CASE WHEN c.user_lo = $1 THEN c.user_hi ELSE c.user_lo END
        INNER JOIN auth_user au ON au.id = other.user_id AND au.status = 'active'
        WHERE cp.user_id = $1
          AND (cp.hidden_at IS NULL OR c.last_message_at > cp.hidden_at)
          AND (
              $2::timestamptz IS NULL
              OR (c.last_message_at, c.id) < ($2::timestamptz, $3::uuid)
          )
        ORDER BY c.last_message_at DESC, c.id DESC
        LIMIT $4
        "#,
        me,
        cursor_at,
        cursor_id,
        limit + 1,
    )
    .fetch_all(pool.get_ref())
    .await
    .map_err(db_err)?;

    let storage = storage.as_ref().as_ref();
    let mut conversations: Vec<ConversationSummary> = rows
        .into_iter()
        .map(|row| ConversationSummary {
            id: row.id,
            counterpart: ProfileSummary {
                id: row.counterpart_id,
                slug: row.counterpart_slug,
                name: row.counterpart_name,
                avatar_url: public_url_of(storage, row.counterpart_avatar_url),
                status: row.counterpart_status,
            },
            last_message: LastMessage {
                preview: markup_preview(&row.last_body, PREVIEW_MAX_CHARS),
                from_me: row.last_from_me,
                created_at: row.last_created_at,
            },
            unread_count: row.unread_count,
            last_message_at: row.last_message_at,
        })
        .collect();

    let next_cursor = take_page(&mut conversations, limit, |conversation| {
        format!(
            "{}:{}",
            conversation.last_message_at.to_rfc3339(),
            conversation.id
        )
    });

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(conversations, next_cursor)))
}

#[utoipa::path(
    get,
    path = "/api/v1/me/conversation/with/{slug}",
    description = "A conversa com um usuário específico. 404 quando ainda não existe: o front abre a tela de escrever",
    params(("slug" = String, Path, description = "Slug do outro usuário")),
    responses(
        (status = 200, description = "Conversa", body = ApiResponse<ConversationSummary>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Usuário ou conversa não encontrada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Busca a conversa com um usuário", skip_all)]
pub async fn with_slug(
    slug: Path<String>,
    session: SessionData,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let me = session.user_id;

    let Some(target) = fetch_target(pool.get_ref(), &slug.into_inner(), Some(me)).await? else {
        return Ok(profile_not_found());
    };

    if target.user_id == me {
        return Ok(conversation_not_found());
    }

    let (lo, hi) = order_pair(me, target.user_id);

    let Some(conversation_id) = sqlx::query_scalar!(
        r#"SELECT id FROM conversation WHERE user_lo = $1 AND user_hi = $2"#,
        lo,
        hi,
    )
    .fetch_optional(pool.get_ref())
    .await
    .map_err(db_err)?
    else {
        return Ok(conversation_not_found());
    };

    match fetch_summary(pool.get_ref(), storage.as_ref().as_ref(), me, conversation_id).await? {
        Some(summary) => Ok(HttpResponse::Ok().json(ApiResponse::new(summary))),
        None => Ok(conversation_not_found()),
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/me/conversation/{id}",
    description = "Metadados de uma conversa: quem é o outro, prévia da última mensagem e não lidas",
    params(("id" = Uuid, Path, description = "Id da conversa")),
    responses(
        (status = 200, description = "Conversa", body = ApiResponse<ConversationSummary>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Conversa não encontrada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Retorna os metadados de uma conversa", skip_all)]
pub async fn detail(
    id: Path<Uuid>,
    session: SessionData,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    match fetch_summary(
        pool.get_ref(),
        storage.as_ref().as_ref(),
        session.user_id,
        id.into_inner(),
    )
    .await?
    {
        Some(summary) => Ok(HttpResponse::Ok().json(ApiResponse::new(summary))),
        None => Ok(conversation_not_found()),
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/me/conversation/{id}/message",
    description = "Histórico da conversa, das mensagens mais novas para as mais antigas. Puro, sem marcar lido: seguro de pollar",
    params(
        ("id" = Uuid, Path, description = "Id da conversa"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Mensagens", body = ApiCursorPaginatedResponse<Vec<MessageResponse>>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Conversa não encontrada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Lista as mensagens de uma conversa", skip_all)]
pub async fn messages(
    id: Path<Uuid>,
    pagination: Query<CursorPaginationQuery>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let me = session.user_id;
    let conversation_id = id.into_inner();

    if !is_participant(pool.get_ref(), conversation_id, me).await? {
        return Ok(conversation_not_found());
    }

    let limit = pagination.limit();
    let (cursor_at, cursor_id) = match parse_cursor(pagination.cursor.as_deref()) {
        Some((at, id)) => (Some(at), Some(id)),
        None => (None, None),
    };

    let rows = sqlx::query!(
        r#"
        SELECT
            m.id,
            m.body,
            m.created_at,
            (m.sender_id = $1) AS "from_me!"
        FROM message m
        WHERE m.conversation_id = $2
          AND (
              $3::timestamptz IS NULL
              OR (m.created_at, m.id) < ($3::timestamptz, $4::uuid)
          )
        ORDER BY m.created_at DESC, m.id DESC
        LIMIT $5
        "#,
        me,
        conversation_id,
        cursor_at,
        cursor_id,
        limit + 1,
    )
    .fetch_all(pool.get_ref())
    .await
    .map_err(db_err)?;

    let mut messages: Vec<MessageResponse> = rows
        .into_iter()
        .map(|row| MessageResponse {
            id: row.id,
            conversation_id,
            body: row.body,
            from_me: row.from_me,
            created_at: row.created_at,
        })
        .collect();

    let next_cursor = take_page(&mut messages, limit, |message| {
        format!("{}:{}", message.created_at.to_rfc3339(), message.id)
    });

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(messages, next_cursor)))
}

#[utoipa::path(
    post,
    path = "/api/v1/me/conversation/{id}/read",
    description = "Marca a conversa como lida até agora",
    params(("id" = Uuid, Path, description = "Id da conversa")),
    responses(
        (status = 204, description = "Conversa marcada como lida"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Conversa não encontrada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Marca uma conversa como lida", skip_all)]
pub async fn read(
    id: Path<Uuid>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let updated = sqlx::query!(
        r#"
        UPDATE conversation_participant
        SET last_read_at = NOW()
        WHERE conversation_id = $1 AND user_id = $2
        "#,
        id.into_inner(),
        session.user_id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?;

    if updated.rows_affected() == 0 {
        return Ok(conversation_not_found());
    }

    Ok(HttpResponse::NoContent().finish())
}

#[utoipa::path(
    delete,
    path = "/api/v1/me/conversation/{id}",
    description = "Esconde a conversa da sua lista. Ela volta sozinha se chegar mensagem nova",
    params(("id" = Uuid, Path, description = "Id da conversa")),
    responses(
        (status = 204, description = "Conversa escondida"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Conversa não encontrada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Esconde uma conversa", skip_all)]
pub async fn delete(
    id: Path<Uuid>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let updated = sqlx::query!(
        r#"
        UPDATE conversation_participant
        SET hidden_at = NOW()
        WHERE conversation_id = $1 AND user_id = $2
        "#,
        id.into_inner(),
        session.user_id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?;

    if updated.rows_affected() == 0 {
        return Ok(conversation_not_found());
    }

    Ok(HttpResponse::NoContent().finish())
}

/// Par de usuários na ordem canônica (menor, maior), como a tabela `conversation`
/// guarda.
pub fn order_pair(a: Uuid, b: Uuid) -> (Uuid, Uuid) {
    if a < b { (a, b) } else { (b, a) }
}
