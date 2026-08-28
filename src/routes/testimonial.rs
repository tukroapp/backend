use actix_web::{
    HttpResponse, Responder,
    web::{Data, Json, Path, Query},
};
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;
use validator::Validate;

use std::sync::Arc;

use crate::{
    routes::profile::{
        ProfileSummary,
        slug::testimonials::{TestimonialRequest, TestimonialResponse},
    },
    services::{
        activity::{NewActivity, record_activity},
        auth::SessionData,
        notification::{NotificationKind, NotificationTarget, notify},
        storage::{StorageService, public_url_of},
    },
    utils::{
        ApiCursorPaginatedResponse, ApiErrorResponse, ApiResponse, CursorPaginationQuery, db_err,
        invalid_field, take_page,
    },
};

/// Fica separado de `CursorPaginationQuery` porque `#[serde(flatten)]` faria o
/// serde ler todos os valores da query string como texto, quebrando `limit`.
#[derive(Debug, Deserialize)]
pub struct InboxQuery {
    /// `received` (padrão) lista os depoimentos escritos para mim, `sent` os
    /// que eu escrevi para os outros.
    pub direction: Option<String>,
    /// Filtra por `pending`, `accepted` ou `rejected`. Sem filtro, vêm todos.
    pub status: Option<String>,
}

fn testimonial_not_found() -> HttpResponse {
    HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
        "testimonial",
        "not_found",
        Some("Depoimento não encontrado."),
        vec![],
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/testimonial",
    description = "Lista paginada (cursor) dos depoimentos recebidos (caixa de moderação) ou escritos pelo usuário autenticado",
    params(
        ("direction" = Option<String>, Query, description = "received (padrão) ou sent"),
        ("status" = Option<String>, Query, description = "Filtra por pending, accepted ou rejected"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Depoimentos", body = ApiCursorPaginatedResponse<Vec<TestimonialResponse>>),
        (status = 400, description = "Filtro inválido", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Lista os depoimentos do usuário", skip_all)]
pub async fn get(
    query: Query<InboxQuery>,
    pagination: Query<CursorPaginationQuery>,
    session: SessionData,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let sent = match query.direction.as_deref() {
        None | Some("received") => false,
        Some("sent") => true,
        Some(_) => {
            return Ok(invalid_field(
                "direction",
                "invalid_direction",
                "A direção precisa ser 'received' ou 'sent'.",
            ));
        }
    };

    if let Some(status) = query.status.as_deref()
        && !matches!(status, "pending" | "accepted" | "rejected")
    {
        return Ok(invalid_field(
            "status",
            "invalid_status",
            "O status precisa ser 'pending', 'accepted' ou 'rejected'.",
        ));
    }

    let limit = pagination.limit();
    // O perfil devolvido é sempre o outro lado: quem escreveu na caixa de
    // moderação, quem recebeu na lista dos meus depoimentos.
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
        INNER JOIN user_profile up
            ON up.user_id = CASE WHEN $2 THEN t.profile_id ELSE t.author_id END
        INNER JOIN auth_user au ON au.id = up.user_id AND au.status IN ('active', 'deleted')
        WHERE (CASE WHEN $2 THEN t.author_id ELSE t.profile_id END) = $1
          AND t.deleted_at IS NULL
          AND ($3::TEXT IS NULL OR t.status::TEXT = $3)
          AND ($4::uuid IS NULL OR t.id < $4::uuid)
        ORDER BY t.id DESC
        LIMIT $5
        "#,
        session.user_id,
        sent,
        query.status,
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
    put,
    path = "/api/v1/testimonial/{id}",
    description = "Edita um depoimento. Só o autor pode editar, e a edição volta o depoimento para pendente",
    params(("id" = Uuid, Path, description = "Id do depoimento")),
    request_body = TestimonialRequest,
    responses(
        (status = 200, description = "Depoimento editado", body = ApiResponse<TestimonialResponse>),
        (status = 400, description = "Corpo inválido", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Depoimento não encontrado ou de outro autor", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Edita um depoimento", skip_all)]
pub async fn put(
    id: Path<Uuid>,
    payload: Json<TestimonialRequest>,
    session: SessionData,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    if let Err(errors) = payload.validate() {
        return Ok(HttpResponse::BadRequest().json(ApiErrorResponse::from(errors)));
    }

    // Editar volta para pendente: o dono aprovou o texto antigo, não o novo.
    let row = sqlx::query!(
        r#"
        WITH updated AS (
            UPDATE testimonial
            SET body = $1, status = 'pending', responded_at = NULL
            WHERE id = $2 AND author_id = $3 AND deleted_at IS NULL
            RETURNING id, body, status, created_at, updated_at, responded_at, profile_id
        )
        SELECT
            u.id,
            u.body,
            u.status::TEXT AS "status!",
            u.created_at,
            u.updated_at,
            u.responded_at,
            up.user_id AS profile_id,
            up.slug AS profile_slug,
            up.name AS profile_name,
            up.avatar_url AS profile_avatar_url,
            up.status AS profile_status
        FROM updated u
        INNER JOIN user_profile up ON up.user_id = u.profile_id
        "#,
        payload.body.trim(),
        id.into_inner(),
        session.user_id,
    )
    .fetch_optional(pool.get_ref())
    .await
    .map_err(db_err)?;

    let Some(row) = row else {
        return Ok(testimonial_not_found());
    };

    Ok(
        HttpResponse::Ok().json(ApiResponse::new(TestimonialResponse {
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

#[utoipa::path(
    delete,
    path = "/api/v1/testimonial/{id}",
    description = "Apaga um depoimento. Podem apagar o autor e o dono do perfil",
    params(("id" = Uuid, Path, description = "Id do depoimento")),
    responses(
        (status = 204, description = "Depoimento apagado"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Depoimento não encontrado ou sem permissão", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Apaga um depoimento", skip_all)]
pub async fn delete(
    id: Path<Uuid>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    // Soft delete: some das leituras e continua disponível para a moderação.
    let removed = sqlx::query!(
        r#"
        UPDATE testimonial
        SET deleted_at = NOW()
        WHERE id = $1 AND (author_id = $2 OR profile_id = $2) AND deleted_at IS NULL
        "#,
        id.into_inner(),
        session.user_id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?
    .rows_affected();

    if removed == 0 {
        return Ok(testimonial_not_found());
    }

    Ok(HttpResponse::NoContent().finish())
}

/// Aceitar e recusar mudam só o status, e só o dono do perfil pode responder.
/// Um depoimento já respondido não volta para a fila.
async fn respond(
    pool: &PgPool,
    id: Uuid,
    profile_id: Uuid,
    accepted: bool,
) -> Result<bool, actix_web::Error> {
    let mut tx = pool.begin().await.map_err(db_err)?;

    let updated = sqlx::query!(
        r#"
        UPDATE testimonial
        SET status = CASE WHEN $3 THEN 'accepted'::approval_status ELSE 'rejected'::approval_status END,
            responded_at = NOW()
        WHERE id = $1 AND profile_id = $2 AND status = 'pending' AND deleted_at IS NULL
        RETURNING author_id
        "#,
        id,
        profile_id,
        accepted,
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_err)?;

    let Some(updated) = updated else {
        return Ok(false);
    };

    // Só o aceite vira notificação: uma recusa não precisa ser anunciada.
    if accepted {
        notify(
            &mut *tx,
            updated.author_id,
            profile_id,
            NotificationKind::TestimonialAccepted,
            NotificationTarget::Testimonial(id),
        )
        .await
        .map_err(db_err)?;

        record_activity(
            &mut *tx,
            NewActivity::testimonial_received(profile_id, updated.author_id, id),
        )
        .await
        .map_err(db_err)?;
    }

    tx.commit().await.map_err(db_err)?;

    Ok(true)
}

#[utoipa::path(
    post,
    path = "/api/v1/testimonial/{id}/accept",
    description = "Aceita um depoimento pendente, publicando ele no perfil",
    params(("id" = Uuid, Path, description = "Id do depoimento")),
    responses(
        (status = 204, description = "Depoimento aceito"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Depoimento não encontrado, já respondido ou de outro perfil", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Aceita um depoimento", skip_all)]
pub async fn accept(
    id: Path<Uuid>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    if !respond(pool.get_ref(), id.into_inner(), session.user_id, true).await? {
        return Ok(testimonial_not_found());
    }

    Ok(HttpResponse::NoContent().finish())
}

#[utoipa::path(
    post,
    path = "/api/v1/testimonial/{id}/reject",
    description = "Recusa um depoimento pendente, que deixa de aparecer no perfil",
    params(("id" = Uuid, Path, description = "Id do depoimento")),
    responses(
        (status = 204, description = "Depoimento recusado"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Depoimento não encontrado, já respondido ou de outro perfil", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Recusa um depoimento", skip_all)]
pub async fn reject(
    id: Path<Uuid>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    if !respond(pool.get_ref(), id.into_inner(), session.user_id, false).await? {
        return Ok(testimonial_not_found());
    }

    Ok(HttpResponse::NoContent().finish())
}
