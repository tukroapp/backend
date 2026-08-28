use actix_web::{
    HttpResponse, Responder,
    web::{Data, Json, Path},
};
use serde::Deserialize;
use sqlx::PgPool;
use utoipa::ToSchema;
use validator::Validate;

use crate::{
    routes::{
        conversation::{MessageResponse, order_pair},
        profile::slug::{fetch_target, profile_not_found},
    },
    services::auth::SessionData,
    utils::{ApiErrorResponse, ApiResponse, db_err, unauthorized, validate_markup_body},
};

#[derive(Debug, Deserialize, ToSchema, Validate)]
pub struct MessageRequest {
    #[validate(
        length(
            min = 1,
            max = 10000,
            message = "A mensagem deve ter entre 1 e 10000 caracteres"
        ),
        custom(function = "validate_markup_body")
    )]
    pub body: String,
}

fn cannot_message_self() -> HttpResponse {
    HttpResponse::BadRequest().json(ApiErrorResponse::from_single_validation_error(
        "message",
        "self",
        Some("Você não pode enviar mensagem para si mesmo."),
        vec![],
    ))
}

fn message_not_allowed() -> HttpResponse {
    HttpResponse::Forbidden().json(ApiErrorResponse::from_single_validation_error(
        "message",
        "not_allowed",
        Some("Esse usuário não aceita mensagens suas."),
        vec![],
    ))
}

#[utoipa::path(
    post,
    path = "/api/v1/profile/{slug}/message",
    description = "Envia uma mensagem privada. Cria a conversa se ainda não existir",
    params(("slug" = String, Path, description = "Slug público do destinatário")),
    request_body = MessageRequest,
    responses(
        (status = 201, description = "Mensagem enviada", body = ApiResponse<MessageResponse>),
        (status = 400, description = "Corpo inválido ou mensagem para si mesmo", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "O destinatário não aceita mensagens suas", body = ApiErrorResponse),
        (status = 404, description = "Destinatário não encontrado ou conta inativa", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Envia uma mensagem privada", skip_all)]
pub async fn post(
    slug: Path<String>,
    payload: Json<MessageRequest>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    // O escopo /profile usa autenticação opcional porque a leitura é pública.
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
        return Ok(cannot_message_self());
    }

    if !target.can_send_message {
        return Ok(message_not_allowed());
    }

    let (lo, hi) = order_pair(session.user_id, target.user_id);
    let body = payload.body.trim();

    let mut tx = pool.begin().await.map_err(db_err)?;

    let conversation_id = sqlx::query_scalar!(
        r#"
        WITH ins AS (
            INSERT INTO conversation (user_lo, user_hi)
            VALUES ($1, $2)
            ON CONFLICT (user_lo, user_hi) DO NOTHING
            RETURNING id
        )
        SELECT id AS "id!" FROM ins
        UNION
        SELECT id FROM conversation WHERE user_lo = $1 AND user_hi = $2
        "#,
        lo,
        hi,
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(db_err)?;

    sqlx::query!(
        r#"
        INSERT INTO conversation_participant (conversation_id, user_id)
        VALUES ($1, $2), ($1, $3)
        ON CONFLICT DO NOTHING
        "#,
        conversation_id,
        lo,
        hi,
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;

    let message = sqlx::query!(
        r#"
        INSERT INTO message (conversation_id, sender_id, body)
        VALUES ($1, $2, $3)
        RETURNING id, created_at
        "#,
        conversation_id,
        session.user_id,
        body,
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(db_err)?;

    tx.commit().await.map_err(db_err)?;

    Ok(
        HttpResponse::Created().json(ApiResponse::new(MessageResponse {
            id: message.id,
            conversation_id,
            body: body.to_owned(),
            from_me: true,
            created_at: message.created_at,
        })),
    )
}
