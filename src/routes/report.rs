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

use crate::{
    routes::friend::{resolve_slug, user_not_found},
    services::auth::SessionData,
    utils::{ApiErrorResponse, ApiResponse, db_err},
};

#[derive(Debug, Clone, Copy, Deserialize, ToSchema, sqlx::Type)]
#[sqlx(type_name = "report_reason", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ReportReason {
    Spam,
    Harassment,
    HateSpeech,
    Nudity,
    Impersonation,
    Other,
}

#[derive(Debug, Deserialize, ToSchema, Validate)]
pub struct ReportRequest {
    pub reason: ReportReason,
    /// Texto livre para o moderador. Opcional, mas o que torna uma denúncia
    /// `other` utilizável.
    #[validate(length(max = 2000))]
    pub details: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ReportResponse {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
}

/// O alvo já resolvido para id. O check do banco garante um alvo por linha, e
/// esse enum é o que decide qual coluna recebe o id.
#[derive(Debug, Clone, Copy)]
enum ReportTarget {
    Profile(Uuid),
    Scrap(Uuid),
    Testimonial(Uuid),
    Community(Uuid),
    Topic(Uuid),
    Comment(Uuid),
    Poll(Uuid),
}

/// As sete colunas de alvo da tabela, na ordem do INSERT. Só uma é `Some`.
struct TargetColumns {
    user: Option<Uuid>,
    scrap: Option<Uuid>,
    testimonial: Option<Uuid>,
    community: Option<Uuid>,
    topic: Option<Uuid>,
    comment: Option<Uuid>,
    poll: Option<Uuid>,
}

impl ReportTarget {
    fn columns(self) -> TargetColumns {
        let mut columns = TargetColumns {
            user: None,
            scrap: None,
            testimonial: None,
            community: None,
            topic: None,
            comment: None,
            poll: None,
        };

        match self {
            ReportTarget::Profile(id) => columns.user = Some(id),
            ReportTarget::Scrap(id) => columns.scrap = Some(id),
            ReportTarget::Testimonial(id) => columns.testimonial = Some(id),
            ReportTarget::Community(id) => columns.community = Some(id),
            ReportTarget::Topic(id) => columns.topic = Some(id),
            ReportTarget::Comment(id) => columns.comment = Some(id),
            ReportTarget::Poll(id) => columns.poll = Some(id),
        }

        columns
    }
}

fn target_not_found(field: &'static str, message: &'static str) -> HttpResponse {
    HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
        field,
        "not_found",
        Some(message),
        vec![],
    ))
}

fn self_report() -> HttpResponse {
    HttpResponse::BadRequest().json(ApiErrorResponse::from_single_validation_error(
        "report",
        "self_report",
        Some("Você não pode denunciar o seu próprio conteúdo."),
        vec![],
    ))
}

fn already_reported() -> HttpResponse {
    HttpResponse::Conflict().json(ApiErrorResponse::from_single_validation_error(
        "report",
        "already_reported",
        Some("Você já denunciou esse perfil."),
        vec![],
    ))
}

/// Valida o corpo e grava a denúncia. O `ON CONFLICT DO NOTHING` só tem efeito
/// em denúncia de perfil, que é a única com índice único por denunciante.
async fn create(
    pool: &PgPool,
    reporter_id: Uuid,
    payload: &ReportRequest,
    target: ReportTarget,
) -> Result<HttpResponse, actix_web::Error> {
    let details = payload
        .details
        .as_deref()
        .map(str::trim)
        .filter(|details| !details.is_empty());

    let columns = target.columns();

    let created = sqlx::query!(
        r#"
        INSERT INTO report (
            reporter_id, reason, details,
            reported_user_id, scrap_id, testimonial_id,
            community_id, topic_id, comment_id, poll_id
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        ON CONFLICT DO NOTHING
        RETURNING id, created_at
        "#,
        reporter_id,
        payload.reason as ReportReason,
        details,
        columns.user,
        columns.scrap,
        columns.testimonial,
        columns.community,
        columns.topic,
        columns.comment,
        columns.poll,
    )
    .fetch_optional(pool)
    .await
    .map_err(db_err)?;

    let Some(created) = created else {
        return Ok(already_reported());
    };

    Ok(
        HttpResponse::Created().json(ApiResponse::new(ReportResponse {
            id: created.id,
            created_at: created.created_at,
        })),
    )
}

/// Rejeita o corpo inválido antes de qualquer consulta ao alvo.
fn validate(payload: &ReportRequest) -> Option<HttpResponse> {
    payload
        .validate()
        .err()
        .map(|errors| HttpResponse::BadRequest().json(ApiErrorResponse::from(errors)))
}

#[utoipa::path(
    post,
    path = "/api/v1/report/profile/{slug}",
    description = "Denuncia um perfil para a moderação. Cada usuário pode denunciar o mesmo perfil uma única vez",
    params(("slug" = String, Path, description = "Slug público do perfil denunciado")),
    request_body = ReportRequest,
    responses(
        (status = 201, description = "Denúncia registrada", body = ApiResponse<ReportResponse>),
        (status = 400, description = "Motivo inválido, detalhes muito longos ou denúncia de si mesmo", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Perfil não encontrado ou conta inativa", body = ApiErrorResponse),
        (status = 409, description = "Você já denunciou esse perfil", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Denuncia um perfil", skip_all)]
pub async fn profile(
    slug: Path<String>,
    payload: Json<ReportRequest>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    if let Some(response) = validate(&payload) {
        return Ok(response);
    }

    // Bloqueio não esconde o perfil aqui: denunciar quem você bloqueou é
    // justamente o caso comum.
    let Some(reported_user_id) = resolve_slug(pool.get_ref(), &slug.into_inner()).await? else {
        return Ok(user_not_found());
    };

    if reported_user_id == session.user_id {
        return Ok(self_report());
    }

    create(
        pool.get_ref(),
        session.user_id,
        &payload,
        ReportTarget::Profile(reported_user_id),
    )
    .await
}

#[utoipa::path(
    post,
    path = "/api/v1/report/scrap/{id}",
    description = "Denuncia um recado do mural",
    params(("id" = Uuid, Path, description = "Id do recado")),
    request_body = ReportRequest,
    responses(
        (status = 201, description = "Denúncia registrada", body = ApiResponse<ReportResponse>),
        (status = 400, description = "Motivo inválido, detalhes muito longos ou conteúdo próprio", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Recado não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Denuncia um recado", skip_all)]
pub async fn scrap(
    id: Path<Uuid>,
    payload: Json<ReportRequest>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    if let Some(response) = validate(&payload) {
        return Ok(response);
    }

    let id = id.into_inner();
    let author_id = sqlx::query_scalar!(
        r#"SELECT author_id FROM scrap WHERE id = $1 AND deleted_at IS NULL"#,
        id,
    )
    .fetch_optional(pool.get_ref())
    .await
    .map_err(db_err)?;

    let Some(author_id) = author_id else {
        return Ok(target_not_found("scrap", "Recado não encontrado."));
    };

    if author_id == session.user_id {
        return Ok(self_report());
    }

    create(
        pool.get_ref(),
        session.user_id,
        &payload,
        ReportTarget::Scrap(id),
    )
    .await
}

#[utoipa::path(
    post,
    path = "/api/v1/report/testimonial/{id}",
    description = "Denuncia um depoimento",
    params(("id" = Uuid, Path, description = "Id do depoimento")),
    request_body = ReportRequest,
    responses(
        (status = 201, description = "Denúncia registrada", body = ApiResponse<ReportResponse>),
        (status = 400, description = "Motivo inválido, detalhes muito longos ou conteúdo próprio", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Depoimento não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Denuncia um depoimento", skip_all)]
pub async fn testimonial(
    id: Path<Uuid>,
    payload: Json<ReportRequest>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    if let Some(response) = validate(&payload) {
        return Ok(response);
    }

    let id = id.into_inner();
    let author_id = sqlx::query_scalar!(
        r#"SELECT author_id FROM testimonial WHERE id = $1 AND deleted_at IS NULL"#,
        id,
    )
    .fetch_optional(pool.get_ref())
    .await
    .map_err(db_err)?;

    let Some(author_id) = author_id else {
        return Ok(target_not_found(
            "testimonial",
            "Depoimento não encontrado.",
        ));
    };

    if author_id == session.user_id {
        return Ok(self_report());
    }

    create(
        pool.get_ref(),
        session.user_id,
        &payload,
        ReportTarget::Testimonial(id),
    )
    .await
}

#[utoipa::path(
    post,
    path = "/api/v1/report/community/{slug}",
    description = "Denuncia uma comunidade",
    params(("slug" = String, Path, description = "Slug da comunidade")),
    request_body = ReportRequest,
    responses(
        (status = 201, description = "Denúncia registrada", body = ApiResponse<ReportResponse>),
        (status = 400, description = "Motivo inválido ou detalhes muito longos", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Comunidade não encontrada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Denuncia uma comunidade", skip_all)]
pub async fn community(
    slug: Path<String>,
    payload: Json<ReportRequest>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    if let Some(response) = validate(&payload) {
        return Ok(response);
    }

    let community_id = sqlx::query_scalar!(
        r#"SELECT id FROM community WHERE slug = $1 AND deleted_at IS NULL"#,
        slug.into_inner(),
    )
    .fetch_optional(pool.get_ref())
    .await
    .map_err(db_err)?;

    let Some(community_id) = community_id else {
        return Ok(target_not_found("community", "Comunidade não encontrada."));
    };

    create(
        pool.get_ref(),
        session.user_id,
        &payload,
        ReportTarget::Community(community_id),
    )
    .await
}

#[utoipa::path(
    post,
    path = "/api/v1/report/topic/{id}",
    description = "Denuncia um tópico de comunidade",
    params(("id" = Uuid, Path, description = "Id do tópico")),
    request_body = ReportRequest,
    responses(
        (status = 201, description = "Denúncia registrada", body = ApiResponse<ReportResponse>),
        (status = 400, description = "Motivo inválido, detalhes muito longos ou conteúdo próprio", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Tópico não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Denuncia um tópico", skip_all)]
pub async fn topic(
    id: Path<Uuid>,
    payload: Json<ReportRequest>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    if let Some(response) = validate(&payload) {
        return Ok(response);
    }

    let id = id.into_inner();
    let author_id = sqlx::query_scalar!(
        r#"SELECT author_id FROM community_topic WHERE id = $1 AND deleted_at IS NULL"#,
        id,
    )
    .fetch_optional(pool.get_ref())
    .await
    .map_err(db_err)?;

    let Some(author_id) = author_id else {
        return Ok(target_not_found("topic", "Tópico não encontrado."));
    };

    if author_id == session.user_id {
        return Ok(self_report());
    }

    create(
        pool.get_ref(),
        session.user_id,
        &payload,
        ReportTarget::Topic(id),
    )
    .await
}

#[utoipa::path(
    post,
    path = "/api/v1/report/comment/{id}",
    description = "Denuncia um comentário de tópico",
    params(("id" = Uuid, Path, description = "Id do comentário")),
    request_body = ReportRequest,
    responses(
        (status = 201, description = "Denúncia registrada", body = ApiResponse<ReportResponse>),
        (status = 400, description = "Motivo inválido, detalhes muito longos ou conteúdo próprio", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Comentário não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Denuncia um comentário", skip_all)]
pub async fn comment(
    id: Path<Uuid>,
    payload: Json<ReportRequest>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    if let Some(response) = validate(&payload) {
        return Ok(response);
    }

    let id = id.into_inner();
    let author_id = sqlx::query_scalar!(
        r#"SELECT author_id FROM community_topic_comment WHERE id = $1 AND deleted_at IS NULL"#,
        id,
    )
    .fetch_optional(pool.get_ref())
    .await
    .map_err(db_err)?;

    let Some(author_id) = author_id else {
        return Ok(target_not_found("comment", "Comentário não encontrado."));
    };

    if author_id == session.user_id {
        return Ok(self_report());
    }

    create(
        pool.get_ref(),
        session.user_id,
        &payload,
        ReportTarget::Comment(id),
    )
    .await
}

#[utoipa::path(
    post,
    path = "/api/v1/report/poll/{id}",
    description = "Denuncia uma enquete de comunidade",
    params(("id" = Uuid, Path, description = "Id da enquete")),
    request_body = ReportRequest,
    responses(
        (status = 201, description = "Denúncia registrada", body = ApiResponse<ReportResponse>),
        (status = 400, description = "Motivo inválido, detalhes muito longos ou conteúdo próprio", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Enquete não encontrada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Denuncia uma enquete", skip_all)]
pub async fn poll(
    id: Path<Uuid>,
    payload: Json<ReportRequest>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    if let Some(response) = validate(&payload) {
        return Ok(response);
    }

    let id = id.into_inner();
    let author_id = sqlx::query_scalar!(
        r#"SELECT author_id FROM community_poll WHERE id = $1 AND deleted_at IS NULL"#,
        id,
    )
    .fetch_optional(pool.get_ref())
    .await
    .map_err(db_err)?;

    let Some(author_id) = author_id else {
        return Ok(target_not_found("poll", "Enquete não encontrada."));
    };

    if author_id == session.user_id {
        return Ok(self_report());
    }

    create(
        pool.get_ref(),
        session.user_id,
        &payload,
        ReportTarget::Poll(id),
    )
    .await
}
