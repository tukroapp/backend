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
    routes::{
        profile::ProfileSummary,
        topic::{
            TopicTarget, author_required, fetch_topic_target, member_required, topic_locked,
            topic_not_found,
        },
    },
    services::{
        auth::SessionData,
        notification::{NotificationKind, NotificationTarget, notify},
        storage::{StorageService, public_url_of},
    },
    utils::{
        ApiCursorPaginatedResponse, ApiErrorResponse, ApiResponse, CursorPaginationQuery, db_err,
        invalid_field, unauthorized, validate_markup_body,
    },
};

/// O que o leitor vê no lugar do texto de um comentário excluído. O corpo
/// original continua no banco para a moderação.
const TOMBSTONE_BODY: &str = "[EXCLUÍDO]";

/// Teto de profundidade da árvore montada em uma resposta. Serve de guarda para
/// a recursão, não de regra de negócio: o banco aceita qualquer profundidade.
const MAX_DEPTH: i32 = 50;

#[derive(Serialize, ToSchema)]
pub struct CommentResponse {
    pub id: Uuid,
    pub topic_id: Uuid,
    /// `null` nos comentários de primeiro nível.
    pub parent_id: Option<Uuid>,
    /// Distância até a raiz da árvore devolvida nesta resposta. Na rota de
    /// contexto é relativa ao comentário focado, que vem como 0.
    pub depth: i32,
    /// Vem como marcador de exclusão quando `deleted_at` está preenchido.
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub edited_at: Option<DateTime<Utc>>,
    /// Preenchido no comentário excluído, que continua na thread como marcador
    /// para não quebrar as respostas presas nele.
    pub deleted_at: Option<DateTime<Utc>>,
    /// `null` no comentário excluído.
    pub author: Option<ProfileSummary>,
    pub can_edit: bool,
    pub can_delete: bool,
}

#[derive(Debug, Deserialize, ToSchema, Validate)]
pub struct CommentRequest {
    #[validate(
        length(
            min = 1,
            max = 1000,
            message = "O comentário deve ter entre 1 e 1000 caracteres"
        ),
        custom(function = "validate_markup_body")
    )]
    pub body: String,
    /// Id do comentário respondido. Ausente cria um comentário de primeiro nível.
    pub parent_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, ToSchema, Validate)]
pub struct CommentUpdateRequest {
    #[validate(
        length(
            min = 1,
            max = 1000,
            message = "O comentário deve ter entre 1 e 1000 caracteres"
        ),
        custom(function = "validate_markup_body")
    )]
    pub body: String,
}

#[derive(Serialize, ToSchema)]
pub struct CommentContextResponse {
    /// Corrente do comentário focado até a raiz, do mais antigo para o mais
    /// novo. Vazia quando o focado já é de primeiro nível.
    pub ancestors: Vec<CommentResponse>,
    pub comment: CommentResponse,
    /// Respostas diretas do focado, cada uma com a própria subárvore.
    pub replies: Vec<CommentResponse>,
    pub next_cursor: Option<String>,
}

fn comment_not_found() -> HttpResponse {
    HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
        "comment",
        "not_found",
        Some("Comentário não encontrado."),
        vec![],
    ))
}

fn comment_deleted() -> HttpResponse {
    HttpResponse::Forbidden().json(ApiErrorResponse::from_single_validation_error(
        "comment",
        "comment_deleted",
        Some("Esse comentário foi excluído."),
        vec![],
    ))
}

/// Uma linha crua da árvore, antes de virar resposta. Existe para que as três
/// consultas (listagem, contexto e ancestrais) compartilhem a mesma montagem.
struct CommentRow {
    id: Uuid,
    topic_id: Uuid,
    parent_id: Option<Uuid>,
    depth: i32,
    body: String,
    created_at: DateTime<Utc>,
    edited_at: Option<DateTime<Utc>>,
    deleted_at: Option<DateTime<Utc>>,
    author_id: Uuid,
    author_slug: String,
    author_name: String,
    author_avatar_url: Option<String>,
    author_status: Option<String>,
}

/// Troca o texto do comentário excluído pelo marcador e esconde o autor. A troca
/// é sempre na leitura: o banco guarda o original.
fn to_response(
    row: CommentRow,
    storage: &dyn StorageService,
    viewer: Option<Uuid>,
    can_moderate: bool,
    locked: bool,
) -> CommentResponse {
    let is_author = viewer == Some(row.author_id);
    let deleted = row.deleted_at.is_some();

    CommentResponse {
        id: row.id,
        topic_id: row.topic_id,
        parent_id: row.parent_id,
        depth: row.depth,
        body: if deleted {
            TOMBSTONE_BODY.to_owned()
        } else {
            row.body
        },
        created_at: row.created_at,
        edited_at: row.edited_at,
        deleted_at: row.deleted_at,
        author: (!deleted).then(|| ProfileSummary {
            id: row.author_id,
            slug: row.author_slug,
            name: row.author_name,
            avatar_url: public_url_of(storage, row.author_avatar_url),
            status: row.author_status,
        }),
        can_edit: is_author && !deleted && !locked,
        can_delete: !deleted && (is_author || can_moderate),
    }
}

/// Página da árvore de comentários. A semente são os filhos diretos de `parent`
/// (ou os de primeiro nível quando `parent` é `None`), paginados por cursor; de
/// cada semente desce a subárvore inteira.
///
/// O `deleted_at` não filtra nada aqui: filtrar apagaria as respostas presas em
/// um comentário excluído junto com ele.
/// Traz `limit + 1` raízes com as subárvores delas. A raiz a mais é o sinal de que
/// existe próxima página, e `truncate_to_limit` a remove antes da resposta.
async fn fetch_comment_tree(
    pool: &PgPool,
    topic_id: Uuid,
    parent: Option<Uuid>,
    cursor: Option<Uuid>,
    limit: i64,
) -> Result<Vec<CommentRow>, actix_web::Error> {
    let rows = sqlx::query!(
        r#"
        WITH RECURSIVE roots AS (
            SELECT
                c.id,
                c.topic_id,
                c.parent_id,
                c.body,
                c.created_at,
                c.edited_at,
                c.deleted_at,
                c.author_id,
                0 AS depth,
                ARRAY[c.id] AS path
            FROM community_topic_comment c
            WHERE c.topic_id = $1
              AND c.parent_id IS NOT DISTINCT FROM $2::uuid
              AND ($3::uuid IS NULL OR c.id > $3::uuid)
            ORDER BY c.id
            LIMIT $4
        ),
        comment_tree AS (
            SELECT * FROM roots
            UNION ALL
            SELECT
                c.id,
                c.topic_id,
                c.parent_id,
                c.body,
                c.created_at,
                c.edited_at,
                c.deleted_at,
                c.author_id,
                ct.depth + 1,
                ct.path || c.id
            FROM community_topic_comment c
            INNER JOIN comment_tree ct ON c.parent_id = ct.id
            WHERE ct.depth < $5
        )
        SELECT
            ct.id AS "id!",
            ct.topic_id AS "topic_id!",
            ct.parent_id,
            ct.depth AS "depth!",
            ct.body AS "body!",
            ct.created_at AS "created_at!",
            ct.edited_at,
            ct.deleted_at,
            up.user_id AS author_id,
            up.slug AS author_slug,
            up.name AS author_name,
            up.avatar_url AS author_avatar_url,
            up.status AS author_status
        FROM comment_tree ct
        INNER JOIN user_profile up ON up.user_id = ct.author_id
        ORDER BY ct.path
        "#,
        topic_id,
        parent,
        cursor,
        limit + 1,
        MAX_DEPTH,
    )
    .fetch_all(pool)
    .await
    .map_err(db_err)?;

    Ok(rows
        .into_iter()
        .map(|row| CommentRow {
            id: row.id,
            topic_id: row.topic_id,
            parent_id: row.parent_id,
            depth: row.depth,
            body: row.body,
            created_at: row.created_at,
            edited_at: row.edited_at,
            deleted_at: row.deleted_at,
            author_id: row.author_id,
            author_slug: row.author_slug,
            author_name: row.author_name,
            author_avatar_url: row.author_avatar_url,
            author_status: row.author_status,
        })
        .collect())
}

/// O cursor anda só sobre as sementes, que são as raízes da página. As linhas de
/// profundidade maior vêm penduradas nelas e não contam para a paginação.
/// Corta a raiz extra que a consulta trouxe e devolve o cursor da última raiz que
/// fica. Sem a extra, a página é a última e não há cursor: contar `limit` raízes
/// não serve como sinal, porque a página cheia pode ser exatamente o fim da lista.
fn truncate_to_limit(rows: &mut Vec<CommentRow>, limit: i64) -> Option<String> {
    let extra = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.depth == 0)
        .map(|(index, _)| index)
        .nth(limit as usize);

    // A ordem é a de leitura, então a raiz extra e a subárvore dela estão no fim.
    rows.truncate(extra?);

    rows.iter()
        .rev()
        .find(|row| row.depth == 0)
        .map(|row| row.id.to_string())
}

#[utoipa::path(
    get,
    path = "/api/v1/topic/{id}/comment",
    description = "Lista paginada (cursor) dos comentários do tópico. Cada comentário de primeiro nível vem com a árvore de respostas abaixo dele",
    params(
        ("id" = Uuid, Path, description = "Id do tópico"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Comentários do tópico", body = ApiCursorPaginatedResponse<Vec<CommentResponse>>),
        (status = 404, description = "Tópico não encontrado ou comunidade privada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Lista os comentários de um tópico", skip_all)]
pub async fn get(
    id: Path<Uuid>,
    pagination: Query<CursorPaginationQuery>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let viewer = session.map(|session| session.user_id);

    let Some(target) = fetch_topic_target(pool.get_ref(), id.into_inner(), viewer).await? else {
        return Ok(topic_not_found());
    };

    if !target.can_view() {
        return Ok(topic_not_found());
    }

    let limit = pagination.limit();
    let mut rows = fetch_comment_tree(
        pool.get_ref(),
        target.id,
        None,
        pagination.cursor_id(),
        limit,
    )
    .await?;

    let next_cursor = truncate_to_limit(&mut rows, limit);
    let storage = storage.as_ref().as_ref();
    let comments: Vec<CommentResponse> = rows
        .into_iter()
        .map(|row| {
            to_response(
                row,
                storage,
                viewer,
                target.can_moderate(),
                target.is_locked,
            )
        })
        .collect();

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(comments, next_cursor)))
}

#[utoipa::path(
    get,
    path = "/api/v1/topic/{id}/comment/{comment_id}/context",
    description = "Abre um comentário específico com a corrente de ancestrais acima dele e as respostas abaixo",
    params(
        ("id" = Uuid, Path, description = "Id do tópico"),
        ("comment_id" = Uuid, Path, description = "Id do comentário focado"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Comentário com contexto", body = ApiResponse<CommentContextResponse>),
        (status = 404, description = "Tópico ou comentário não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Abre um comentário com o contexto da thread", skip_all)]
pub async fn context(
    path: Path<(Uuid, Uuid)>,
    pagination: Query<CursorPaginationQuery>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let (topic_id, comment_id) = path.into_inner();
    let viewer = session.map(|session| session.user_id);

    let Some(target) = fetch_topic_target(pool.get_ref(), topic_id, viewer).await? else {
        return Ok(topic_not_found());
    };

    if !target.can_view() {
        return Ok(topic_not_found());
    }

    // A corrente sobe do comentário focado até a raiz. O ancestral excluído
    // continua nela como marcador, senão o caminho de volta perde um degrau.
    let ancestor_rows = sqlx::query!(
        r#"
        WITH RECURSIVE chain AS (
            SELECT
                c.id,
                c.topic_id,
                c.parent_id,
                c.body,
                c.created_at,
                c.edited_at,
                c.deleted_at,
                c.author_id,
                0 AS depth
            FROM community_topic_comment c
            WHERE c.id = $1 AND c.topic_id = $2
            UNION ALL
            SELECT
                p.id,
                p.topic_id,
                p.parent_id,
                p.body,
                p.created_at,
                p.edited_at,
                p.deleted_at,
                p.author_id,
                ch.depth - 1
            FROM community_topic_comment p
            INNER JOIN chain ch ON ch.parent_id = p.id
            WHERE ch.depth + $3::int > 0
        )
        SELECT
            ch.id AS "id!",
            ch.topic_id AS "topic_id!",
            ch.parent_id,
            ch.depth AS "depth!",
            ch.body AS "body!",
            ch.created_at AS "created_at!",
            ch.edited_at,
            ch.deleted_at,
            up.user_id AS author_id,
            up.slug AS author_slug,
            up.name AS author_name,
            up.avatar_url AS author_avatar_url,
            up.status AS author_status
        FROM chain ch
        INNER JOIN user_profile up ON up.user_id = ch.author_id
        ORDER BY ch.depth
        "#,
        comment_id,
        topic_id,
        MAX_DEPTH,
    )
    .fetch_all(pool.get_ref())
    .await
    .map_err(db_err)?;

    if ancestor_rows.is_empty() {
        return Ok(comment_not_found());
    }

    let storage = storage.as_ref().as_ref();
    let can_moderate = target.can_moderate();
    let locked = target.is_locked;

    // A corrente vem do mais antigo para o mais novo, então o focado é o último.
    let mut chain: Vec<CommentResponse> = ancestor_rows
        .into_iter()
        .map(|row| {
            to_response(
                CommentRow {
                    id: row.id,
                    topic_id: row.topic_id,
                    parent_id: row.parent_id,
                    depth: row.depth,
                    body: row.body,
                    created_at: row.created_at,
                    edited_at: row.edited_at,
                    deleted_at: row.deleted_at,
                    author_id: row.author_id,
                    author_slug: row.author_slug,
                    author_name: row.author_name,
                    author_avatar_url: row.author_avatar_url,
                    author_status: row.author_status,
                },
                storage,
                viewer,
                can_moderate,
                locked,
            )
        })
        .collect();

    let comment = chain
        .pop()
        .expect("a corrente sempre tem o comentário focado");
    let ancestors = chain;

    let limit = pagination.limit();
    let mut reply_rows = fetch_comment_tree(
        pool.get_ref(),
        target.id,
        Some(comment_id),
        pagination.cursor_id(),
        limit,
    )
    .await?;

    let next_cursor = truncate_to_limit(&mut reply_rows, limit);
    let replies: Vec<CommentResponse> = reply_rows
        .into_iter()
        .map(|row| to_response(row, storage, viewer, can_moderate, locked))
        .collect();

    Ok(
        HttpResponse::Ok().json(ApiResponse::new(CommentContextResponse {
            ancestors,
            comment,
            replies,
            next_cursor,
        })),
    )
}

#[utoipa::path(
    post,
    path = "/api/v1/topic/{id}/comment",
    description = "Comenta no tópico ou responde outro comentário. Exige ser membro da comunidade",
    params(("id" = Uuid, Path, description = "Id do tópico")),
    request_body = CommentRequest,
    responses(
        (status = 201, description = "Comentário publicado", body = ApiResponse<CommentResponse>),
        (status = 400, description = "Corpo inválido ou comentário respondido inexistente", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Apenas membros comentam, e o tópico não pode estar trancado", body = ApiErrorResponse),
        (status = 404, description = "Tópico não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Comenta em um tópico do fórum", skip_all)]
pub async fn post(
    id: Path<Uuid>,
    payload: Json<CommentRequest>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    if let Err(errors) = payload.validate() {
        return Ok(HttpResponse::BadRequest().json(ApiErrorResponse::from(errors)));
    }

    let Some(target) =
        fetch_topic_target(pool.get_ref(), id.into_inner(), Some(session.user_id)).await?
    else {
        return Ok(topic_not_found());
    };

    if !target.can_view() {
        return Ok(topic_not_found());
    }

    if !target.is_member() {
        return Ok(member_required());
    }

    if target.is_locked {
        return Ok(topic_locked());
    }

    // Quem recebe o aviso muda conforme o comentário seja raiz ou resposta, e o
    // pai precisa ser do mesmo tópico.
    let parent_author = match payload.parent_id {
        Some(parent_id) => {
            let parent = sqlx::query!(
                r#"
                SELECT author_id, deleted_at
                FROM community_topic_comment
                WHERE id = $1 AND topic_id = $2
                "#,
                parent_id,
                target.id,
            )
            .fetch_optional(pool.get_ref())
            .await
            .map_err(db_err)?;

            let Some(parent) = parent else {
                return Ok(invalid_field(
                    "parent_id",
                    "not_found",
                    "O comentário respondido não existe nesse tópico.",
                ));
            };

            if parent.deleted_at.is_some() {
                return Ok(comment_deleted());
            }

            Some(parent.author_id)
        }
        None => None,
    };

    let row = sqlx::query!(
        r#"
        WITH inserted AS (
            INSERT INTO community_topic_comment (topic_id, author_id, parent_id, body)
            VALUES ($1, $2, $3, $4)
            RETURNING id, topic_id, parent_id, body, created_at, edited_at, deleted_at, author_id
        )
        SELECT
            i.id,
            i.topic_id,
            i.parent_id,
            i.body,
            i.created_at,
            i.edited_at,
            i.deleted_at,
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
        payload.parent_id,
        payload.body.trim(),
    )
    .fetch_one(pool.get_ref())
    .await
    .map_err(db_err)?;

    match parent_author {
        Some(parent_author) => notify(
            pool.get_ref(),
            parent_author,
            session.user_id,
            NotificationKind::TopicReply,
            NotificationTarget::Comment {
                topic_id: target.id,
                comment_id: row.id,
            },
        )
        .await
        .map_err(db_err)?,
        None => notify(
            pool.get_ref(),
            target.author_id,
            session.user_id,
            NotificationKind::TopicComment,
            NotificationTarget::Topic(target.id),
        )
        .await
        .map_err(db_err)?,
    }

    let comment = to_response(
        CommentRow {
            id: row.id,
            topic_id: row.topic_id,
            parent_id: row.parent_id,
            depth: 0,
            body: row.body,
            created_at: row.created_at,
            edited_at: row.edited_at,
            deleted_at: row.deleted_at,
            author_id: row.author_id,
            author_slug: row.author_slug,
            author_name: row.author_name,
            author_avatar_url: row.author_avatar_url,
            author_status: row.author_status,
        },
        storage.as_ref().as_ref(),
        Some(session.user_id),
        target.can_moderate(),
        target.is_locked,
    );

    Ok(HttpResponse::Created().json(ApiResponse::new(comment)))
}

/// Carrega o comentário para as rotas de escrita, garantindo que ele é mesmo do
/// tópico da URL.
async fn fetch_comment_owner(
    pool: &PgPool,
    target: &TopicTarget,
    comment_id: Uuid,
) -> Result<Option<(Uuid, Option<DateTime<Utc>>)>, actix_web::Error> {
    let row = sqlx::query!(
        r#"
        SELECT author_id, deleted_at
        FROM community_topic_comment
        WHERE id = $1 AND topic_id = $2
        "#,
        comment_id,
        target.id,
    )
    .fetch_optional(pool)
    .await
    .map_err(db_err)?;

    Ok(row.map(|row| (row.author_id, row.deleted_at)))
}

#[utoipa::path(
    put,
    path = "/api/v1/topic/{id}/comment/{comment_id}",
    description = "Edita o próprio comentário, marcando a edição",
    params(
        ("id" = Uuid, Path, description = "Id do tópico"),
        ("comment_id" = Uuid, Path, description = "Id do comentário"),
    ),
    request_body = CommentUpdateRequest,
    responses(
        (status = 200, description = "Comentário atualizado", body = ApiResponse<CommentResponse>),
        (status = 400, description = "Corpo inválido", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Apenas o autor edita, e não em tópico trancado", body = ApiErrorResponse),
        (status = 404, description = "Tópico ou comentário não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Edita um comentário do fórum", skip_all)]
pub async fn put(
    path: Path<(Uuid, Uuid)>,
    payload: Json<CommentUpdateRequest>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    if let Err(errors) = payload.validate() {
        return Ok(HttpResponse::BadRequest().json(ApiErrorResponse::from(errors)));
    }

    let (topic_id, comment_id) = path.into_inner();

    let Some(target) = fetch_topic_target(pool.get_ref(), topic_id, Some(session.user_id)).await?
    else {
        return Ok(topic_not_found());
    };

    if !target.can_view() {
        return Ok(topic_not_found());
    }

    let Some((author_id, deleted_at)) =
        fetch_comment_owner(pool.get_ref(), &target, comment_id).await?
    else {
        return Ok(comment_not_found());
    };

    if author_id != session.user_id {
        return Ok(author_required());
    }

    if deleted_at.is_some() {
        return Ok(comment_deleted());
    }

    if target.is_locked {
        return Ok(topic_locked());
    }

    let row = sqlx::query!(
        r#"
        WITH updated AS (
            UPDATE community_topic_comment
            SET body = $1, edited_at = NOW()
            WHERE id = $2
            RETURNING id, topic_id, parent_id, body, created_at, edited_at, deleted_at, author_id
        )
        SELECT
            u.id,
            u.topic_id,
            u.parent_id,
            u.body,
            u.created_at,
            u.edited_at,
            u.deleted_at,
            up.user_id AS author_id,
            up.slug AS author_slug,
            up.name AS author_name,
            up.avatar_url AS author_avatar_url,
            up.status AS author_status
        FROM updated u
        INNER JOIN user_profile up ON up.user_id = u.author_id
        "#,
        payload.body.trim(),
        comment_id,
    )
    .fetch_one(pool.get_ref())
    .await
    .map_err(db_err)?;

    let comment = to_response(
        CommentRow {
            id: row.id,
            topic_id: row.topic_id,
            parent_id: row.parent_id,
            depth: 0,
            body: row.body,
            created_at: row.created_at,
            edited_at: row.edited_at,
            deleted_at: row.deleted_at,
            author_id: row.author_id,
            author_slug: row.author_slug,
            author_name: row.author_name,
            author_avatar_url: row.author_avatar_url,
            author_status: row.author_status,
        },
        storage.as_ref().as_ref(),
        Some(session.user_id),
        target.can_moderate(),
        target.is_locked,
    );

    Ok(HttpResponse::Ok().json(ApiResponse::new(comment)))
}

#[utoipa::path(
    delete,
    path = "/api/v1/topic/{id}/comment/{comment_id}",
    description = "Exclui o comentário. Ele continua na thread como marcador e o texto original fica no banco para a moderação",
    params(
        ("id" = Uuid, Path, description = "Id do tópico"),
        ("comment_id" = Uuid, Path, description = "Id do comentário"),
    ),
    responses(
        (status = 204, description = "Comentário excluído"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Apenas o autor, a moderação da comunidade ou um administrador", body = ApiErrorResponse),
        (status = 404, description = "Tópico ou comentário não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Exclui um comentário do fórum", skip_all)]
pub async fn delete(
    path: Path<(Uuid, Uuid)>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    let (topic_id, comment_id) = path.into_inner();

    let Some(target) = fetch_topic_target(pool.get_ref(), topic_id, Some(session.user_id)).await?
    else {
        return Ok(topic_not_found());
    };

    if !target.can_view() {
        return Ok(topic_not_found());
    }

    let Some((author_id, deleted_at)) =
        fetch_comment_owner(pool.get_ref(), &target, comment_id).await?
    else {
        return Ok(comment_not_found());
    };

    if author_id != session.user_id && !target.can_moderate() {
        return Ok(author_required());
    }

    if deleted_at.is_some() {
        return Ok(HttpResponse::NoContent().finish());
    }

    sqlx::query!(
        r#"
        UPDATE community_topic_comment
        SET deleted_at = NOW()
        WHERE id = $1
        "#,
        comment_id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?;

    Ok(HttpResponse::NoContent().finish())
}
