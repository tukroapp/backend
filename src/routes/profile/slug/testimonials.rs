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
pub struct TestimonialResponse {
    pub id: Uuid,
    pub body: String,
    /// `pending`, `accepted` ou `rejected`.
    #[schema(example = "pending")]
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Quando o dono do perfil aceitou ou recusou. `null` enquanto pendente.
    pub responded_at: Option<DateTime<Utc>>,
    /// O outro lado do depoimento: o autor na lista pública e na caixa de
    /// moderação, o dono do perfil na lista dos depoimentos que eu escrevi.
    pub profile: ProfileSummary,
}

#[derive(Debug, Deserialize, ToSchema, Validate)]
pub struct TestimonialRequest {
    #[validate(
        length(
            min = 1,
            max = 5000,
            message = "O depoimento deve ter entre 1 e 5000 caracteres"
        ),
        custom(function = "validate_markup_body")
    )]
    pub body: String,
}

pub fn testimonial_write_forbidden() -> HttpResponse {
    HttpResponse::Forbidden().json(ApiErrorResponse::from_single_validation_error(
        "testimonial",
        "testimonial_write_forbidden",
        Some("Você não pode escrever um depoimento para esse perfil."),
        vec![],
    ))
}

fn private_testimonial_list() -> HttpResponse {
    HttpResponse::Forbidden().json(ApiErrorResponse::from_single_validation_error(
        "testimonial",
        "private_testimonial_list",
        Some("Os depoimentos desse perfil são privados."),
        vec![],
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/profile/{slug}/testimonial",
    description = "Lista paginada (cursor) dos depoimentos aceitos do perfil",
    params(
        ("slug" = String, Path, description = "Slug público do perfil"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Depoimentos aceitos", body = ApiCursorPaginatedResponse<Vec<TestimonialResponse>>),
        (status = 403, description = "Depoimentos privados", body = ApiErrorResponse),
        (status = 404, description = "Perfil não encontrado ou conta inativa", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Lista os depoimentos aceitos de um perfil", skip_all)]
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

    // Os depoimentos seguem a privacidade geral do perfil, como a lista de amigos.
    // O que o dono aceitou é público para quem pode ver o perfil; o `status`
    // filtra pendentes e recusados.
    if !target.can_view_profile_details {
        return Ok(private_testimonial_list());
    }

    let limit = pagination.limit();
    let rows = sqlx::query!(
        r#"
        SELECT
            t.id,
            t.body,
            t.status::TEXT AS "status!",
            t.created_at,
            t.updated_at,
            t.responded_at,
            up.user_id AS profile_id,
            up.slug AS profile_slug,
            up.name AS profile_name,
            up.avatar_url AS profile_avatar_url,
            up.status AS profile_status
        FROM testimonial t
        INNER JOIN user_profile up ON up.user_id = t.author_id
        -- Depoimento de quem está suspenso ou desativado some do perfil alheio.
        INNER JOIN auth_user au ON au.id = t.author_id AND au.status IN ('active', 'deleted')
        WHERE t.profile_id = $1
          AND t.status = 'accepted'
          AND t.deleted_at IS NULL
          AND ($2::uuid IS NULL OR t.id < $2::uuid)
        ORDER BY t.id DESC
        LIMIT $3
        "#,
        target.user_id,
        pagination.cursor_id(),
        limit + 1,
    )
    .fetch_all(pool.get_ref())
    .await
    .map_err(db_err)?;

    let mut results = rows
        .into_iter()
        .map(|row| TestimonialResponse {
            id: row.id,
            body: row.body,
            status: row.status,
            created_at: row.created_at,
            updated_at: row.updated_at,
            responded_at: row.responded_at,
            profile: ProfileSummary {
                id: row.profile_id,
                slug: row.profile_slug,
                name: row.profile_name,
                avatar_url: public_url_of(storage.as_ref().as_ref(), row.profile_avatar_url),
                status: row.profile_status,
            },
        })
        .collect::<Vec<_>>();

    let next_cursor = take_page(&mut results, limit, |item| item.id.to_string());

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(results, next_cursor)))
}

#[utoipa::path(
    post,
    path = "/api/v1/profile/{slug}/testimonial",
    description = "Escreve um depoimento para o perfil, que fica pendente até o dono aceitar",
    params(("slug" = String, Path, description = "Slug público do perfil")),
    request_body = TestimonialRequest,
    responses(
        (status = 201, description = "Depoimento enviado, aguardando aprovação", body = ApiResponse<TestimonialResponse>),
        (status = 400, description = "Corpo inválido ou depoimento para si mesmo", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Sem permissão para escrever nesse perfil", body = ApiErrorResponse),
        (status = 404, description = "Perfil não encontrado ou conta inativa", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Escreve um depoimento", skip_all)]
pub async fn post(
    slug: Path<String>,
    payload: Json<TestimonialRequest>,
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

    if target.user_id == session.user_id {
        return Ok(HttpResponse::BadRequest().json(
            ApiErrorResponse::from_single_validation_error(
                "testimonial",
                "self_testimonial",
                Some("Você não pode escrever um depoimento para si mesmo."),
                vec![],
            ),
        ));
    }

    if !target.can_write_testimonial {
        return Ok(testimonial_write_forbidden());
    }

    // Não há limite de depoimentos por autor: escrever vários para a mesma
    // pessoa é o uso normal, cada um passa pela moderação separadamente.
    let row = sqlx::query!(
        r#"
        WITH inserted AS (
            INSERT INTO testimonial (profile_id, author_id, body)
            VALUES ($1, $2, $3)
            RETURNING id, body, status, created_at, updated_at, responded_at, profile_id
        )
        SELECT
            i.id,
            i.body,
            i.status::TEXT AS "status!",
            i.created_at,
            i.updated_at,
            i.responded_at,
            up.user_id AS profile_id,
            up.slug AS profile_slug,
            up.name AS profile_name,
            up.avatar_url AS profile_avatar_url,
            up.status AS profile_status
        FROM inserted i
        INNER JOIN user_profile up ON up.user_id = i.profile_id
        "#,
        target.user_id,
        session.user_id,
        payload.body.trim(),
    )
    .fetch_one(pool.get_ref())
    .await
    .map_err(db_err)?;

    notify(
        pool.get_ref(),
        target.user_id,
        session.user_id,
        NotificationKind::TestimonialReceived,
        NotificationTarget::Testimonial(row.id),
    )
    .await
    .map_err(db_err)?;

    Ok(
        HttpResponse::Created().json(ApiResponse::new(TestimonialResponse {
            id: row.id,
            body: row.body,
            status: row.status,
            created_at: row.created_at,
            updated_at: row.updated_at,
            responded_at: row.responded_at,
            profile: ProfileSummary {
                id: row.profile_id,
                slug: row.profile_slug,
                name: row.profile_name,
                avatar_url: public_url_of(storage.as_ref().as_ref(), row.profile_avatar_url),
                status: row.profile_status,
            },
        })),
    )
}
