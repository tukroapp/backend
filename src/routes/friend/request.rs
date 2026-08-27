use actix_web::{
    HttpResponse, Responder,
    web::{Data, Path, Query},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    constants::MAX_FRIENDS,
    routes::friend::{resolve_slug, user_not_found},
    services::{
        activity::{NewActivity, record_activity},
        auth::SessionData,
        notification::{NotificationKind, NotificationTarget, notify},
        storage::{StorageService, public_url_of},
    },
    utils::{
        ApiCursorPaginatedResponse, ApiErrorResponse, ApiResponse, CursorPaginationQuery, db_err,
        take_page,
    },
};

#[derive(Serialize, ToSchema)]
pub struct FriendRequestResponse {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub slug: String,
    pub name: String,
    pub avatar_url: Option<String>,
}

pub enum AcceptOutcome {
    Accepted,
    NotFound,
    OwnLimitReached,
    OtherLimitReached,
}

/// Marca o pedido como aceito e cria os dois lados da amizade. A contagem roda
/// dentro da transação, então o limite vale mesmo com dois pedidos aceitos ao
/// mesmo tempo.
async fn accept_request(
    pool: &PgPool,
    request_id: Uuid,
    addressee_id: Uuid,
) -> Result<AcceptOutcome, actix_web::Error> {
    let mut tx = pool.begin().await.map_err(db_err)?;

    let responded = sqlx::query!(
        r#"
        UPDATE friend_request
        SET status = 'accepted', responded_at = NOW()
        WHERE id = $1 AND addressee_id = $2 AND status = 'pending'
        RETURNING requester_id
        "#,
        request_id,
        addressee_id,
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_err)?;

    let Some(responded) = responded else {
        return Ok(AcceptOutcome::NotFound);
    };

    let counts = sqlx::query!(
        r#"
        SELECT
            (SELECT COUNT(*) FROM friend WHERE user_id = $1) AS "addressee!",
            (SELECT COUNT(*) FROM friend WHERE user_id = $2) AS "requester!"
        "#,
        addressee_id,
        responded.requester_id,
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(db_err)?;

    // O rollback devolve o pedido para pendente, que é o estado certo: a
    // amizade não aconteceu.
    if counts.addressee >= MAX_FRIENDS {
        tx.rollback().await.map_err(db_err)?;
        return Ok(AcceptOutcome::OwnLimitReached);
    }

    if counts.requester >= MAX_FRIENDS {
        tx.rollback().await.map_err(db_err)?;
        return Ok(AcceptOutcome::OtherLimitReached);
    }

    sqlx::query!(
        r#"
        INSERT INTO friend (user_id, friend_id)
        VALUES ($1, $2), ($2, $1)
        ON CONFLICT DO NOTHING
        "#,
        addressee_id,
        responded.requester_id,
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;

    // Quem pediu é quem precisa saber que foi aceito.
    notify(
        &mut *tx,
        responded.requester_id,
        addressee_id,
        NotificationKind::FriendAccepted,
        NotificationTarget::FriendRequest(request_id),
    )
    .await
    .map_err(db_err)?;

    record_activity(
        &mut *tx,
        NewActivity::friendship_made(addressee_id, responded.requester_id),
    )
    .await
    .map_err(db_err)?;

    tx.commit().await.map_err(db_err)?;

    Ok(AcceptOutcome::Accepted)
}

fn request_not_found() -> HttpResponse {
    HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
        "friend_request",
        "not_found",
        Some("Pedido de amizade não encontrado."),
        vec![],
    ))
}

fn friend_limit_reached() -> HttpResponse {
    let message = format!("Você atingiu o limite de {MAX_FRIENDS} amigos.");

    HttpResponse::Conflict().json(ApiErrorResponse::from_single_validation_error(
        "friend",
        "friend_limit_reached",
        Some(&message),
        vec![],
    ))
}

fn target_friend_limit_reached() -> HttpResponse {
    let message = format!("Esse perfil atingiu o limite de {MAX_FRIENDS} amigos.");

    HttpResponse::Conflict().json(ApiErrorResponse::from_single_validation_error(
        "friend",
        "target_friend_limit_reached",
        Some(&message),
        vec![],
    ))
}

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Pedidos que o usuário autenticado recebeu e ainda não respondeu.
    #[default]
    Received,
    /// Pedidos que o usuário autenticado enviou e ainda não foram respondidos.
    Sent,
}

/// Fica separado de `CursorPaginationQuery` porque `#[serde(flatten)]` faria o
/// serde ler todos os valores da query string como texto, quebrando `limit`.
#[derive(Debug, Deserialize)]
pub struct InboxQuery {
    #[serde(default)]
    pub direction: Direction,
}

#[utoipa::path(
    get,
    path = "/api/v1/friend/request",
    description = "Lista paginada (cursor) dos pedidos de amizade pendentes do usuário autenticado, recebidos por padrão ou enviados com `direction=sent`",
    params(
        ("direction" = Option<String>, Query, description = "`received` (padrão) ou `sent`"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Pedidos pendentes", body = ApiCursorPaginatedResponse<Vec<FriendRequestResponse>>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Lista os pedidos de amizade recebidos", skip_all)]
pub async fn get(
    query: Query<InboxQuery>,
    pagination: Query<CursorPaginationQuery>,
    session: SessionData,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let limit = pagination.limit();
    // Em `sent` o perfil exibido é o do destinatário; em `received`, o de quem pediu.
    let sent = query.direction == Direction::Sent;

    let mut requests = sqlx::query_as!(
        FriendRequestResponse,
        r#"
        SELECT
            fr.id,
            fr.created_at,
            up.slug,
            up.name,
            up.avatar_url
        FROM friend_request fr
        INNER JOIN user_profile up
            ON up.user_id = CASE WHEN $4 THEN fr.addressee_id ELSE fr.requester_id END
        INNER JOIN auth_user au ON au.id = up.user_id AND au.status = 'active'
        WHERE fr.status = 'pending'
          AND (($4 AND fr.requester_id = $1) OR (NOT $4 AND fr.addressee_id = $1))
          AND ($2::uuid IS NULL OR fr.id < $2::uuid)
        ORDER BY fr.id DESC
        LIMIT $3
        "#,
        session.user_id,
        pagination.cursor_id(),
        limit + 1,
        sent,
    )
    .fetch_all(pool.get_ref())
    .await
    .map_err(db_err)?;

    for request in &mut requests {
        request.avatar_url = public_url_of(storage.as_ref().as_ref(), request.avatar_url.take());
    }

    let next_cursor = take_page(&mut requests, limit, |request| request.id.to_string());

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(requests, next_cursor)))
}

#[derive(Serialize, ToSchema)]
pub struct FriendRequestCreated {
    pub id: Uuid,
    /// `pending` quando o pedido ficou aguardando resposta e `accepted` quando
    /// já existia um pedido do outro lado e a amizade foi fechada na hora.
    #[schema(example = "pending")]
    pub status: String,
}

#[utoipa::path(
    post,
    path = "/api/v1/friend/request/{slug}",
    description = "Envia um pedido de amizade para o usuário do slug informado. Se já existir um pedido pendente do outro lado, a amizade é aceita automaticamente",
    params(("slug" = String, Path, description = "Slug público do destinatário")),
    responses(
        (status = 200, description = "Pedido cruzado aceito automaticamente", body = ApiResponse<FriendRequestCreated>),
        (status = 201, description = "Pedido enviado", body = ApiResponse<FriendRequestCreated>),
        (status = 400, description = "Pedido para si mesmo", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Perfil não encontrado ou conta inativa", body = ApiErrorResponse),
        (status = 409, description = "Já são amigos, você já enviou um pedido ou algum dos dois lados atingiu o limite de amigos", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Envia um pedido de amizade", skip_all)]
pub async fn post(
    slug: Path<String>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(addressee_id) = resolve_slug(pool.get_ref(), &slug.into_inner()).await? else {
        return Ok(user_not_found());
    };

    if addressee_id == session.user_id {
        return Ok(HttpResponse::BadRequest().json(
            ApiErrorResponse::from_single_validation_error(
                "friend_request",
                "self_request",
                Some("Você não pode enviar um pedido de amizade para si mesmo."),
                vec![],
            ),
        ));
    }

    let state = sqlx::query!(
        r#"
        SELECT
            EXISTS (
                SELECT 1 FROM friend
                WHERE user_id = $1 AND friend_id = $2
            ) AS "is_friend!",
            EXISTS (
                SELECT 1 FROM friend_request
                WHERE status = 'pending'
                  AND requester_id = $1 AND addressee_id = $2
            ) AS "has_outgoing!",
            (
                SELECT id FROM friend_request
                WHERE status = 'pending'
                  AND requester_id = $2 AND addressee_id = $1
                LIMIT 1
            ) AS incoming_id,
            EXISTS (
                SELECT 1 FROM block
                WHERE (blocker_id = $1 AND blocked_id = $2)
                   OR (blocker_id = $2 AND blocked_id = $1)
            ) AS "is_blocked!",
            (SELECT COUNT(*) FROM friend WHERE user_id = $1) AS "own_friends!",
            (SELECT COUNT(*) FROM friend WHERE user_id = $2) AS "target_friends!"
        "#,
        session.user_id,
        addressee_id,
    )
    .fetch_one(pool.get_ref())
    .await
    .map_err(db_err)?;

    // Bloqueio devolve o mesmo 404 de perfil inexistente, para não revelar o bloqueio.
    if state.is_blocked {
        return Ok(user_not_found());
    }

    if state.is_friend {
        return Ok(
            HttpResponse::Conflict().json(ApiErrorResponse::from_single_validation_error(
                "friend_request",
                "already_friends",
                Some("Vocês já são amigos."),
                vec![],
            )),
        );
    }

    if state.has_outgoing {
        return Ok(
            HttpResponse::Conflict().json(ApiErrorResponse::from_single_validation_error(
                "friend_request",
                "request_pending",
                Some("Você já enviou um pedido de amizade para esse perfil."),
                vec![],
            )),
        );
    }

    if state.own_friends >= MAX_FRIENDS {
        return Ok(friend_limit_reached());
    }

    if state.target_friends >= MAX_FRIENDS {
        return Ok(target_friend_limit_reached());
    }

    // Pedido cruzado: os dois se pediram, então a amizade é fechada na hora.
    if let Some(incoming_id) = state.incoming_id {
        match accept_request(pool.get_ref(), incoming_id, session.user_id).await? {
            AcceptOutcome::Accepted => {
                return Ok(
                    HttpResponse::Ok().json(ApiResponse::new(FriendRequestCreated {
                        id: incoming_id,
                        status: "accepted".to_string(),
                    })),
                );
            }
            AcceptOutcome::OwnLimitReached => return Ok(friend_limit_reached()),
            AcceptOutcome::OtherLimitReached => return Ok(target_friend_limit_reached()),
            // O pedido cruzado sumiu no meio do caminho: segue e cria o próprio.
            AcceptOutcome::NotFound => {}
        }
    }

    let created = sqlx::query!(
        r#"
        INSERT INTO friend_request (requester_id, addressee_id)
        VALUES ($1, $2)
        RETURNING id
        "#,
        session.user_id,
        addressee_id,
    )
    .fetch_one(pool.get_ref())
    .await
    .map_err(db_err)?;

    notify(
        pool.get_ref(),
        addressee_id,
        session.user_id,
        NotificationKind::FriendRequest,
        NotificationTarget::FriendRequest(created.id),
    )
    .await
    .map_err(db_err)?;

    Ok(
        HttpResponse::Created().json(ApiResponse::new(FriendRequestCreated {
            id: created.id,
            status: "pending".to_string(),
        })),
    )
}

#[utoipa::path(
    delete,
    path = "/api/v1/friend/request/{slug}",
    description = "Cancela o pedido de amizade pendente que o usuário autenticado enviou para o slug informado",
    params(("slug" = String, Path, description = "Slug público do destinatário")),
    responses(
        (status = 204, description = "Pedido cancelado"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Perfil não encontrado ou não há pedido pendente seu", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Cancela um pedido de amizade enviado", skip_all)]
pub async fn cancel(
    slug: Path<String>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(addressee_id) = resolve_slug(pool.get_ref(), &slug.into_inner()).await? else {
        return Ok(user_not_found());
    };

    let cancelled = sqlx::query!(
        r#"
        DELETE FROM friend_request
        WHERE requester_id = $1 AND addressee_id = $2 AND status = 'pending'
        "#,
        session.user_id,
        addressee_id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?
    .rows_affected();

    if cancelled == 0 {
        return Ok(request_not_found());
    }

    Ok(HttpResponse::NoContent().finish())
}

#[utoipa::path(
    post,
    path = "/api/v1/friend/request/{id}/accept",
    description = "Aceita um pedido de amizade pendente recebido pelo usuário autenticado",
    params(("id" = Uuid, Path, description = "Id do pedido de amizade")),
    responses(
        (status = 204, description = "Pedido aceito"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Pedido não encontrado ou já respondido", body = ApiErrorResponse),
        (status = 409, description = "Um dos dois lados atingiu o limite de amigos", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Aceita um pedido de amizade", skip_all)]
pub async fn accept(
    id: Path<Uuid>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let response = match accept_request(pool.get_ref(), id.into_inner(), session.user_id).await? {
        AcceptOutcome::Accepted => HttpResponse::NoContent().finish(),
        AcceptOutcome::NotFound => request_not_found(),
        AcceptOutcome::OwnLimitReached => friend_limit_reached(),
        AcceptOutcome::OtherLimitReached => target_friend_limit_reached(),
    };

    Ok(response)
}

#[utoipa::path(
    post,
    path = "/api/v1/friend/request/{id}/reject",
    description = "Recusa um pedido de amizade pendente recebido pelo usuário autenticado",
    params(("id" = Uuid, Path, description = "Id do pedido de amizade")),
    responses(
        (status = 204, description = "Pedido recusado"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Pedido não encontrado ou já respondido", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Recusa um pedido de amizade", skip_all)]
pub async fn reject(
    id: Path<Uuid>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let rejected = sqlx::query!(
        r#"
        UPDATE friend_request
        SET status = 'rejected', responded_at = NOW()
        WHERE id = $1 AND addressee_id = $2 AND status = 'pending'
        "#,
        id.into_inner(),
        session.user_id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?
    .rows_affected();

    if rejected == 0 {
        return Ok(request_not_found());
    }

    Ok(HttpResponse::NoContent().finish())
}
