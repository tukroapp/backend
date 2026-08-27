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

#[utoipa::path(
    post,
    path = "/api/v1/report/{slug}",
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
pub async fn post(
    slug: Path<String>,
    payload: Json<ReportRequest>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    if let Err(errors) = payload.validate() {
        return Ok(HttpResponse::BadRequest().json(ApiErrorResponse::from(errors)));
    }

    // Bloqueio não esconde o perfil aqui: denunciar quem você bloqueou é
    // justamente o caso comum.
    let Some(reported_user_id) = resolve_slug(pool.get_ref(), &slug.into_inner()).await? else {
        return Ok(user_not_found());
    };

    if reported_user_id == session.user_id {
        return Ok(HttpResponse::BadRequest().json(
            ApiErrorResponse::from_single_validation_error(
                "report",
                "self_report",
                Some("Você não pode denunciar a si mesmo."),
                vec![],
            ),
        ));
    }

    let details = payload
        .details
        .as_deref()
        .map(str::trim)
        .filter(|details| !details.is_empty());

    // O índice único garante uma denúncia por par, então o conflito vira 409.
    let created = sqlx::query!(
        r#"
        INSERT INTO report (reporter_id, reported_user_id, reason, details)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT DO NOTHING
        RETURNING id, created_at
        "#,
        session.user_id,
        reported_user_id,
        payload.reason as ReportReason,
        details,
    )
    .fetch_optional(pool.get_ref())
    .await
    .map_err(db_err)?;

    let Some(created) = created else {
        return Ok(
            HttpResponse::Conflict().json(ApiErrorResponse::from_single_validation_error(
                "report",
                "already_reported",
                Some("Você já denunciou esse perfil."),
                vec![],
            )),
        );
    };

    Ok(
        HttpResponse::Created().json(ApiResponse::new(ReportResponse {
            id: created.id,
            created_at: created.created_at,
        })),
    )
}
