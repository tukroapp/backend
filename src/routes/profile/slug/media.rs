use actix_web::{
    HttpResponse, Responder,
    web::{Data, Path, Query},
};
use serde::Deserialize;
use sqlx::PgPool;

use std::sync::Arc;

use crate::{
    routes::{
        media::{MediaKind, MediaResponse, fetch_media},
        profile::slug::{fetch_target, profile_not_found},
    },
    services::{auth::SessionData, storage::StorageService},
    utils::{ApiCursorPaginatedResponse, ApiErrorResponse, CursorPaginationQuery, take_page},
};

#[derive(Debug, Deserialize)]
pub struct MediaQuery {
    /// Filtro opcional pelo tipo: `photo` ou `video`.
    pub kind: Option<MediaKind>,
}

#[utoipa::path(
    get,
    path = "/api/v1/profile/{slug}/media",
    description = "Lista paginada (cursor) das fotos e vídeos do perfil, respeitando a privacidade do dono",
    params(
        ("slug" = String, Path, description = "Slug público do perfil"),
        ("kind" = Option<String>, Query, description = "Filtra por tipo: `photo` ou `video`"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Álbum do perfil", body = ApiCursorPaginatedResponse<Vec<MediaResponse>>),
        (status = 403, description = "Álbum privado", body = ApiErrorResponse),
        (status = 404, description = "Perfil não encontrado ou conta inativa", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Lista o álbum de um perfil", skip_all)]
pub async fn get(
    slug: Path<String>,
    query: Query<MediaQuery>,
    pagination: Query<CursorPaginationQuery>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let viewer = session.map(|session| session.user_id);

    let Some(target) = fetch_target(pool.get_ref(), &slug.into_inner(), viewer).await? else {
        return Ok(profile_not_found());
    };

    if !target.can_view_profile_details {
        return Ok(
            HttpResponse::Forbidden().json(ApiErrorResponse::from_single_validation_error(
                "media",
                "private_album",
                Some("O álbum desse perfil é privado."),
                vec![],
            )),
        );
    }

    let limit = pagination.limit();

    let mut media = fetch_media(
        pool.get_ref(),
        storage.as_ref().as_ref(),
        target.user_id,
        query.kind,
        pagination.cursor_id(),
        limit + 1,
    )
    .await?;

    let next_cursor = take_page(&mut media, limit, |item| item.id.to_string());

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(media, next_cursor)))
}
