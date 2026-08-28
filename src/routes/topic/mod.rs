pub mod comments;

use actix_web::{
    HttpResponse, Responder,
    web::{Data, Json, Path},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

use std::sync::Arc;

use crate::{
    routes::{community::CommunityRole, profile::ProfileSummary},
    services::{
        auth::SessionData,
        storage::{StorageService, public_url_of},
    },
    utils::{
        ApiErrorResponse, ApiResponse, db_err, unauthorized, validate_markup_body,
        validate_trimmed_string,
    },
};

#[derive(Serialize, ToSchema)]
pub struct TopicResponse {
    pub id: Uuid,
    pub community_slug: String,
    pub title: String,
    pub body: String,
    pub is_pinned: bool,
    pub is_locked: bool,
    pub created_at: DateTime<Utc>,
    /// Preenchido só quando o autor editou o texto. `updated_at` não serve para
    /// isso: ele também muda a cada comentário novo, por causa do bump de
    /// `last_activity_at`.
    pub edited_at: Option<DateTime<Utc>>,
    pub last_activity_at: DateTime<Utc>,
    pub author: ProfileSummary,
    pub comment_count: i64,
    /// Só o autor edita o texto. Moderador apaga, mas não reescreve.
    pub can_edit: bool,
    pub can_delete: bool,
    pub can_moderate: bool,
}

#[derive(Debug, Deserialize, ToSchema, Validate)]
pub struct TopicRequest {
    #[validate(
        length(
            min = 1,
            max = 200,
            message = "O título deve ter entre 1 e 200 caracteres"
        ),
        custom(function = "validate_trimmed_string")
    )]
    pub title: String,
    #[validate(
        length(
            min = 1,
            max = 1000,
            message = "A mensagem deve ter entre 1 e 1000 caracteres"
        ),
        custom(function = "validate_markup_body")
    )]
    pub body: String,
}

/// Tópico resolvido pelo id junto da relação de quem está olhando com a
/// comunidade dele.
pub struct TopicTarget {
    pub id: Uuid,
    pub community_id: Uuid,
    pub community_slug: String,
    pub is_private: bool,
    pub author_id: Uuid,
    pub is_locked: bool,
    pub viewer: Option<Uuid>,
    pub role: Option<CommunityRole>,
    pub is_superuser: bool,
}

impl TopicTarget {
    pub fn is_member(&self) -> bool {
        self.role.is_some()
    }

    pub fn is_author(&self) -> bool {
        self.viewer == Some(self.author_id)
    }

    /// O superuser modera qualquer fórum, sem precisar entrar na comunidade.
    pub fn can_moderate(&self) -> bool {
        self.is_superuser || matches!(self.role, Some(CommunityRole::Owner | CommunityRole::Admin))
    }

    pub fn can_view(&self) -> bool {
        !self.is_private || self.is_member() || self.is_superuser
    }

    pub fn can_delete(&self) -> bool {
        self.is_author() || self.can_moderate()
    }
}

pub fn topic_not_found() -> HttpResponse {
    HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
        "topic",
        "not_found",
        Some("Tópico não encontrado."),
        vec![],
    ))
}

pub fn member_required() -> HttpResponse {
    HttpResponse::Forbidden().json(ApiErrorResponse::from_single_validation_error(
        "topic",
        "member_required",
        Some("Entre na comunidade para participar do fórum."),
        vec![],
    ))
}

pub fn author_required() -> HttpResponse {
    HttpResponse::Forbidden().json(ApiErrorResponse::from_single_validation_error(
        "topic",
        "author_required",
        Some("Apenas quem escreveu pode editar."),
        vec![],
    ))
}

pub fn topic_locked() -> HttpResponse {
    HttpResponse::Forbidden().json(ApiErrorResponse::from_single_validation_error(
        "topic",
        "topic_locked",
        Some("Esse tópico está trancado."),
        vec![],
    ))
}

/// Carrega o tópico com o papel de quem está olhando. Devolve `None` para tópico
/// inexistente ou já excluído: quem chama responde 404 nos dois casos.
pub async fn fetch_topic_target(
    pool: &PgPool,
    topic_id: Uuid,
    viewer: Option<Uuid>,
) -> Result<Option<TopicTarget>, actix_web::Error> {
    let row = sqlx::query!(
        r#"
        SELECT
            t.id,
            t.community_id,
            t.author_id,
            t.is_locked,
            c.slug AS community_slug,
            c.is_private,
            cm.role AS "role?: CommunityRole",
            COALESCE((
                SELECT au.is_superuser FROM auth_user au WHERE au.id = $2::uuid
            ), FALSE) AS "is_superuser!"
        FROM community_topic t
        INNER JOIN community c ON c.id = t.community_id
        LEFT JOIN community_member cm ON cm.community_id = c.id AND cm.user_id = $2::uuid
        WHERE t.id = $1 AND t.deleted_at IS NULL AND c.deleted_at IS NULL
        "#,
        topic_id,
        viewer,
    )
    .fetch_optional(pool)
    .await
    .map_err(db_err)?;

    Ok(row.map(|row| TopicTarget {
        id: row.id,
        community_id: row.community_id,
        community_slug: row.community_slug,
        is_private: row.is_private,
        author_id: row.author_id,
        is_locked: row.is_locked,
        viewer,
        role: row.role,
        is_superuser: row.is_superuser,
    }))
}

/// Monta a resposta de um tópico só. A listagem usa a própria query, para não
/// disparar uma consulta por linha.
pub async fn fetch_topic(
    pool: &PgPool,
    storage: &dyn StorageService,
    target: &TopicTarget,
) -> Result<TopicResponse, actix_web::Error> {
    let row = sqlx::query!(
        r#"
        SELECT
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
        WHERE t.id = $1
        "#,
        target.id,
    )
    .fetch_one(pool)
    .await
    .map_err(db_err)?;

    Ok(TopicResponse {
        id: target.id,
        community_slug: target.community_slug.clone(),
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
            avatar_url: public_url_of(storage, row.author_avatar_url),
            status: row.author_status,
        },
        comment_count: row.comment_count,
        can_edit: target.is_author(),
        can_delete: target.can_delete(),
        can_moderate: target.can_moderate(),
    })
}

#[utoipa::path(
    get,
    path = "/api/v1/topic/{id}",
    description = "Retorna um tópico do fórum, sem os comentários",
    params(("id" = Uuid, Path, description = "Id do tópico")),
    responses(
        (status = 200, description = "Tópico", body = ApiResponse<TopicResponse>),
        (status = 404, description = "Tópico não encontrado ou comunidade privada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Retorna um tópico do fórum", skip_all)]
pub async fn get(
    id: Path<Uuid>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let viewer = session.map(|session| session.user_id);

    let Some(target) = fetch_topic_target(pool.get_ref(), id.into_inner(), viewer).await? else {
        return Ok(topic_not_found());
    };

    // Comunidade privada devolve 404 para quem está de fora, e não 403, para não
    // confirmar que o tópico existe.
    if !target.can_view() {
        return Ok(topic_not_found());
    }

    let topic = fetch_topic(pool.get_ref(), storage.as_ref().as_ref(), &target).await?;

    Ok(HttpResponse::Ok().json(ApiResponse::new(topic)))
}

#[utoipa::path(
    put,
    path = "/api/v1/topic/{id}",
    description = "Edita o título e o corpo do tópico, marcando a edição",
    params(("id" = Uuid, Path, description = "Id do tópico")),
    request_body = TopicRequest,
    responses(
        (status = 200, description = "Tópico atualizado", body = ApiResponse<TopicResponse>),
        (status = 400, description = "Dados inválidos", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Apenas o autor edita, e o tópico não pode estar trancado", body = ApiErrorResponse),
        (status = 404, description = "Tópico não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Edita um tópico do fórum", skip_all)]
pub async fn put(
    id: Path<Uuid>,
    payload: Json<TopicRequest>,
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

    if !target.is_author() {
        return Ok(author_required());
    }

    if target.is_locked {
        return Ok(topic_locked());
    }

    sqlx::query!(
        r#"
        UPDATE community_topic
        SET title = $1, body = $2, edited_at = NOW()
        WHERE id = $3
        "#,
        payload.title.trim(),
        payload.body.trim(),
        target.id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?;

    let topic = fetch_topic(pool.get_ref(), storage.as_ref().as_ref(), &target).await?;

    Ok(HttpResponse::Ok().json(ApiResponse::new(topic)))
}

#[utoipa::path(
    delete,
    path = "/api/v1/topic/{id}",
    description = "Exclui o tópico. O conteúdo continua no banco como prova para a moderação",
    params(("id" = Uuid, Path, description = "Id do tópico")),
    responses(
        (status = 204, description = "Tópico excluído"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Apenas o autor, a moderação da comunidade ou um administrador", body = ApiErrorResponse),
        (status = 404, description = "Tópico não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Exclui um tópico do fórum", skip_all)]
pub async fn delete(
    id: Path<Uuid>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    let Some(target) =
        fetch_topic_target(pool.get_ref(), id.into_inner(), Some(session.user_id)).await?
    else {
        return Ok(topic_not_found());
    };

    if !target.can_view() {
        return Ok(topic_not_found());
    }

    if !target.can_delete() {
        return Ok(member_required());
    }

    sqlx::query!(
        r#"
        UPDATE community_topic
        SET deleted_at = NOW()
        WHERE id = $1
        "#,
        target.id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?;

    Ok(HttpResponse::NoContent().finish())
}

/// Pin e lock só mudam uma coluna e exigem a mesma permissão, então os quatro
/// handlers passam por aqui.
async fn set_moderation_flag(
    pool: &PgPool,
    storage: &dyn StorageService,
    topic_id: Uuid,
    session: Option<SessionData>,
    pinned: Option<bool>,
    locked: Option<bool>,
) -> Result<HttpResponse, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    let Some(target) = fetch_topic_target(pool, topic_id, Some(session.user_id)).await? else {
        return Ok(topic_not_found());
    };

    if !target.can_view() {
        return Ok(topic_not_found());
    }

    if !target.can_moderate() {
        return Ok(member_required());
    }

    sqlx::query!(
        r#"
        UPDATE community_topic
        SET is_pinned = COALESCE($1, is_pinned),
            is_locked = COALESCE($2, is_locked)
        WHERE id = $3
        "#,
        pinned,
        locked,
        target.id,
    )
    .execute(pool)
    .await
    .map_err(db_err)?;

    let topic = fetch_topic(pool, storage, &target).await?;

    Ok(HttpResponse::Ok().json(ApiResponse::new(topic)))
}

#[utoipa::path(
    post,
    path = "/api/v1/topic/{id}/pin",
    description = "Fixa o tópico no topo da listagem da comunidade",
    params(("id" = Uuid, Path, description = "Id do tópico")),
    responses(
        (status = 200, description = "Tópico fixado", body = ApiResponse<TopicResponse>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Apenas a moderação", body = ApiErrorResponse),
        (status = 404, description = "Tópico não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Fixa um tópico", skip_all)]
pub async fn pin(
    id: Path<Uuid>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    set_moderation_flag(
        pool.get_ref(),
        storage.as_ref().as_ref(),
        id.into_inner(),
        session,
        Some(true),
        None,
    )
    .await
}

#[utoipa::path(
    delete,
    path = "/api/v1/topic/{id}/pin",
    description = "Desafixa o tópico",
    params(("id" = Uuid, Path, description = "Id do tópico")),
    responses(
        (status = 200, description = "Tópico desafixado", body = ApiResponse<TopicResponse>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Apenas a moderação", body = ApiErrorResponse),
        (status = 404, description = "Tópico não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Desafixa um tópico", skip_all)]
pub async fn unpin(
    id: Path<Uuid>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    set_moderation_flag(
        pool.get_ref(),
        storage.as_ref().as_ref(),
        id.into_inner(),
        session,
        Some(false),
        None,
    )
    .await
}

#[utoipa::path(
    post,
    path = "/api/v1/topic/{id}/lock",
    description = "Tranca o tópico: ninguém comenta nem edita comentário, mas a moderação continua agindo",
    params(("id" = Uuid, Path, description = "Id do tópico")),
    responses(
        (status = 200, description = "Tópico trancado", body = ApiResponse<TopicResponse>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Apenas a moderação", body = ApiErrorResponse),
        (status = 404, description = "Tópico não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Tranca um tópico", skip_all)]
pub async fn lock(
    id: Path<Uuid>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    set_moderation_flag(
        pool.get_ref(),
        storage.as_ref().as_ref(),
        id.into_inner(),
        session,
        None,
        Some(true),
    )
    .await
}

#[utoipa::path(
    delete,
    path = "/api/v1/topic/{id}/lock",
    description = "Destranca o tópico",
    params(("id" = Uuid, Path, description = "Id do tópico")),
    responses(
        (status = 200, description = "Tópico destrancado", body = ApiResponse<TopicResponse>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Apenas a moderação", body = ApiErrorResponse),
        (status = 404, description = "Tópico não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Destranca um tópico", skip_all)]
pub async fn unlock(
    id: Path<Uuid>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    set_moderation_flag(
        pool.get_ref(),
        storage.as_ref().as_ref(),
        id.into_inner(),
        session,
        None,
        Some(false),
    )
    .await
}
