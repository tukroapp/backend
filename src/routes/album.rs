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
    constants::MAX_ALBUMS_PER_USER,
    routes::{
        me::Visibility,
        media::{MediaResponse, fetch_album_media, media_not_found},
        profile::slug::{allows, fetch_target, profile_not_found},
    },
    services::{
        auth::SessionData,
        storage::{StorageService, public_url_of},
    },
    utils::{
        ApiCursorPaginatedResponse, ApiErrorResponse, ApiResponse, CursorPaginationQuery, db_err,
        take_page,
    },
};

#[derive(Debug, Deserialize, ToSchema, Validate)]
pub struct AlbumRequest {
    #[validate(length(
        min = 1,
        max = 100,
        message = "O título deve ter entre 1 e 100 caracteres"
    ))]
    pub title: String,
    #[validate(length(max = 2000))]
    pub description: Option<String>,
    /// Quem enxerga o álbum. O `profile_visibility` do perfil ainda manda por
    /// cima: perfil fechado esconde tudo.
    pub visibility: Option<Visibility>,
    /// Foto de capa. Precisa ser sua e estar nesse álbum.
    pub cover_media_id: Option<Uuid>,
}

#[derive(Serialize, ToSchema)]
pub struct AlbumSummary {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub visibility: Visibility,
    pub cover_url: Option<String>,
    pub photos_count: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
pub struct AlbumDetail {
    #[serde(flatten)]
    pub album: AlbumSummary,
    /// Slug do dono, para o link de volta ao perfil.
    pub owner_slug: String,
    pub photos: Vec<MediaResponse>,
}

pub fn album_not_found() -> HttpResponse {
    HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
        "album",
        "not_found",
        Some("Álbum não encontrado."),
        vec![],
    ))
}

fn album_limit_reached() -> HttpResponse {
    let message = format!("Você já tem {MAX_ALBUMS_PER_USER} álbuns.");

    HttpResponse::Conflict().json(ApiErrorResponse::from_single_validation_error(
        "album",
        "album_limit_reached",
        Some(&message),
        vec![],
    ))
}

/// Dono da foto, se quem está olhando pode vê-la. `None` quando a foto não
/// existe, o perfil está fechado ou o álbum dela é mais fechado que o visitante.
/// É a mesma regra que decide a leitura e o comentário.
pub async fn visible_media_owner(
    pool: &PgPool,
    media_id: Uuid,
    viewer: Option<Uuid>,
) -> Result<Option<Uuid>, actix_web::Error> {
    let row = sqlx::query!(
        r#"
        SELECT
            m.user_id,
            up.slug,
            a.visibility AS "album_visibility?: Visibility"
        FROM user_media m
        INNER JOIN user_profile up ON up.user_id = m.user_id
        LEFT JOIN album a ON a.id = m.album_id
        WHERE m.id = $1 AND m.deleted_at IS NULL
        "#,
        media_id,
    )
    .fetch_optional(pool)
    .await
    .map_err(db_err)?;

    let Some(row) = row else {
        return Ok(None);
    };

    // Passa pelo perfil primeiro: bloqueio, conta inativa e perfil fechado já
    // resolvem aqui.
    let Some(target) = fetch_target(pool, &row.slug, viewer).await? else {
        return Ok(None);
    };

    if !target.can_view_profile_details {
        return Ok(None);
    }

    let visible = match row.album_visibility {
        // Foto solta segue só a privacidade do perfil.
        None => true,
        Some(visibility) => allows(visibility, target.is_self, target.is_friend),
    };

    Ok(visible.then_some(row.user_id))
}

#[utoipa::path(
    post,
    path = "/api/v1/me/album",
    description = "Cria um álbum de fotos",
    request_body = AlbumRequest,
    responses(
        (status = 201, description = "Álbum criado", body = ApiResponse<AlbumSummary>),
        (status = 400, description = "Dados inválidos", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 409, description = "Limite de álbuns atingido", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Cria um álbum", skip_all)]
pub async fn post(
    payload: Json<AlbumRequest>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    session.require_verified_email(pool.get_ref()).await?;

    if let Err(errors) = payload.validate() {
        return Ok(HttpResponse::BadRequest().json(ApiErrorResponse::from(errors)));
    }

    let count = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM album WHERE user_id = $1"#,
        session.user_id,
    )
    .fetch_one(pool.get_ref())
    .await
    .map_err(db_err)?;

    if count >= MAX_ALBUMS_PER_USER {
        return Ok(album_limit_reached());
    }

    let description = payload
        .description
        .as_deref()
        .map(str::trim)
        .filter(|description| !description.is_empty());

    let created = sqlx::query!(
        r#"
        INSERT INTO album (user_id, title, description, visibility)
        VALUES ($1, $2, $3, COALESCE($4::visibility, 'everyone'))
        RETURNING id, title, description, visibility AS "visibility: Visibility", created_at
        "#,
        session.user_id,
        payload.title.trim(),
        description,
        payload.visibility as Option<Visibility>,
    )
    .fetch_one(pool.get_ref())
    .await
    .map_err(db_err)?;

    Ok(HttpResponse::Created().json(ApiResponse::new(AlbumSummary {
        id: created.id,
        title: created.title,
        description: created.description,
        visibility: created.visibility,
        cover_url: None,
        photos_count: 0,
        created_at: created.created_at,
    })))
}

#[utoipa::path(
    put,
    path = "/api/v1/me/album/{id}",
    description = "Edita título, descrição, privacidade e capa do álbum",
    params(("id" = Uuid, Path, description = "Id do álbum")),
    request_body = AlbumRequest,
    responses(
        (status = 200, description = "Álbum atualizado", body = ApiResponse<AlbumSummary>),
        (status = 400, description = "Dados inválidos", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Álbum não encontrado, ou capa que não está nele", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Edita um álbum", skip_all)]
pub async fn put(
    id: Path<Uuid>,
    payload: Json<AlbumRequest>,
    session: SessionData,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    if let Err(errors) = payload.validate() {
        return Ok(HttpResponse::BadRequest().json(ApiErrorResponse::from(errors)));
    }

    let album_id = id.into_inner();

    // A capa tem que ser uma foto desse álbum, senão a capa mostraria algo que
    // não está lá dentro.
    if let Some(cover_media_id) = payload.cover_media_id {
        let belongs = sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM user_media
                WHERE id = $1 AND album_id = $2 AND user_id = $3
            ) AS "exists!"
            "#,
            cover_media_id,
            album_id,
            session.user_id,
        )
        .fetch_one(pool.get_ref())
        .await
        .map_err(db_err)?;

        if !belongs {
            return Ok(media_not_found());
        }
    }

    let description = payload
        .description
        .as_deref()
        .map(str::trim)
        .filter(|description| !description.is_empty());

    let updated = sqlx::query!(
        r#"
        UPDATE album
        SET title = $3,
            description = $4,
            visibility = COALESCE($5::visibility, visibility),
            cover_media_id = COALESCE($6, cover_media_id)
        WHERE id = $1 AND user_id = $2
        RETURNING
            id,
            title,
            description,
            visibility AS "visibility: Visibility",
            cover_media_id,
            created_at,
            (
                SELECT COUNT(*) FROM user_media m
                WHERE m.album_id = album.id AND m.deleted_at IS NULL
            ) AS "photos_count!"
        "#,
        album_id,
        session.user_id,
        payload.title.trim(),
        description,
        payload.visibility as Option<Visibility>,
        payload.cover_media_id,
    )
    .fetch_optional(pool.get_ref())
    .await
    .map_err(db_err)?;

    let Some(updated) = updated else {
        return Ok(album_not_found());
    };

    let cover_url = cover_url_of(
        pool.get_ref(),
        storage.as_ref().as_ref(),
        updated.cover_media_id,
    )
    .await?;

    Ok(HttpResponse::Ok().json(ApiResponse::new(AlbumSummary {
        id: updated.id,
        title: updated.title,
        description: updated.description,
        visibility: updated.visibility,
        cover_url,
        photos_count: updated.photos_count,
        created_at: updated.created_at,
    })))
}

async fn cover_url_of(
    pool: &PgPool,
    storage: &dyn StorageService,
    cover_media_id: Option<Uuid>,
) -> Result<Option<String>, actix_web::Error> {
    let Some(cover_media_id) = cover_media_id else {
        return Ok(None);
    };

    let key = sqlx::query_scalar!(
        r#"SELECT storage_key FROM user_media WHERE id = $1"#,
        cover_media_id,
    )
    .fetch_optional(pool)
    .await
    .map_err(db_err)?;

    Ok(public_url_of(storage, key))
}

#[utoipa::path(
    delete,
    path = "/api/v1/me/album/{id}",
    description = "Apaga o álbum e as fotos dentro dele, inclusive os arquivos no bucket",
    params(("id" = Uuid, Path, description = "Id do álbum")),
    responses(
        (status = 204, description = "Álbum apagado"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Álbum não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Apaga um álbum", skip_all)]
pub async fn delete(
    id: Path<Uuid>,
    session: SessionData,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let album_id = id.into_inner();

    // As keys saem antes: depois do DELETE as linhas não existem mais, e o que
    // sobraria no bucket seria lixo invisível.
    let keys = sqlx::query_scalar!(
        r#"
        SELECT m.storage_key
        FROM user_media m
        INNER JOIN album a ON a.id = m.album_id AND a.user_id = $2
        WHERE m.album_id = $1
        "#,
        album_id,
        session.user_id,
    )
    .fetch_all(pool.get_ref())
    .await
    .map_err(db_err)?;

    let deleted = sqlx::query!(
        r#"DELETE FROM album WHERE id = $1 AND user_id = $2"#,
        album_id,
        session.user_id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?
    .rows_affected();

    if deleted == 0 {
        return Ok(album_not_found());
    }

    for key in keys {
        if let Err(error) = storage.delete(&key).await {
            tracing::error!(%error, %key, "failed to delete a file of a removed album");
        }
    }

    Ok(HttpResponse::NoContent().finish())
}

#[utoipa::path(
    get,
    path = "/api/v1/profile/{slug}/album",
    description = "Álbuns do perfil que quem está olhando pode ver",
    params(
        ("slug" = String, Path, description = "Slug público do perfil"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Álbuns", body = ApiCursorPaginatedResponse<Vec<AlbumSummary>>),
        (status = 403, description = "Perfil fechado para quem olha", body = ApiErrorResponse),
        (status = 404, description = "Perfil não encontrado ou conta inativa", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Lista os álbuns de um perfil", skip_all)]
pub async fn list(
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

    if !target.can_view_profile_details {
        return Ok(
            HttpResponse::Forbidden().json(ApiErrorResponse::from_single_validation_error(
                "album",
                "private_album",
                Some("Os álbuns desse perfil são privados."),
                vec![],
            )),
        );
    }

    let limit = pagination.limit();
    let rows = sqlx::query!(
        r#"
        SELECT
            a.id,
            a.title,
            a.description,
            a.visibility AS "visibility: Visibility",
            a.created_at,
            cover.storage_key AS "cover_key?",
            (
                SELECT COUNT(*) FROM user_media m
                WHERE m.album_id = a.id AND m.deleted_at IS NULL
            ) AS "photos_count!"
        FROM album a
        LEFT JOIN user_media cover ON cover.id = a.cover_media_id
        WHERE a.user_id = $1
          AND ($2::uuid IS NULL OR a.id < $2::uuid)
        ORDER BY a.id DESC
        LIMIT $3
        "#,
        target.user_id,
        pagination.cursor_id(),
        limit + 1,
    )
    .fetch_all(pool.get_ref())
    .await
    .map_err(db_err)?;

    let storage = storage.as_ref().as_ref();
    let mut results = rows
        .into_iter()
        // O álbum mais fechado que o visitante some da lista, e não vira um
        // card vazio dizendo que existe algo escondido.
        .filter(|row| allows(row.visibility, target.is_self, target.is_friend))
        .map(|row| AlbumSummary {
            id: row.id,
            title: row.title,
            description: row.description,
            visibility: row.visibility,
            cover_url: public_url_of(storage, row.cover_key),
            photos_count: row.photos_count,
            created_at: row.created_at,
        })
        .collect::<Vec<_>>();

    let next_cursor = take_page(&mut results, limit, |album| album.id.to_string());

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(results, next_cursor)))
}

#[utoipa::path(
    get,
    path = "/api/v1/album/{id}",
    description = "Um álbum com as fotos dentro, na ordem definida pelo dono",
    params(("id" = Uuid, Path, description = "Id do álbum")),
    responses(
        (status = 200, description = "Álbum", body = ApiResponse<AlbumDetail>),
        (status = 404, description = "Álbum não encontrado ou fora do alcance de quem olha", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Detalha um álbum", skip_all)]
pub async fn detail(
    id: Path<Uuid>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let album_id = id.into_inner();
    let viewer = session.map(|session| session.user_id);

    let row = sqlx::query!(
        r#"
        SELECT
            a.id,
            a.title,
            a.description,
            a.visibility AS "visibility: Visibility",
            a.created_at,
            up.slug AS owner_slug,
            cover.storage_key AS "cover_key?",
            (
                SELECT COUNT(*) FROM user_media m
                WHERE m.album_id = a.id AND m.deleted_at IS NULL
            ) AS "photos_count!"
        FROM album a
        INNER JOIN user_profile up ON up.user_id = a.user_id
        LEFT JOIN user_media cover ON cover.id = a.cover_media_id
        WHERE a.id = $1
        "#,
        album_id,
    )
    .fetch_optional(pool.get_ref())
    .await
    .map_err(db_err)?;

    let Some(row) = row else {
        return Ok(album_not_found());
    };

    let Some(target) = fetch_target(pool.get_ref(), &row.owner_slug, viewer).await? else {
        return Ok(album_not_found());
    };

    // Álbum escondido responde 404, não 403: quem não pode ver não precisa
    // saber que ele existe.
    if !target.can_view_profile_details || !allows(row.visibility, target.is_self, target.is_friend)
    {
        return Ok(album_not_found());
    }

    let storage = storage.as_ref().as_ref();
    let photos = fetch_album_media(pool.get_ref(), storage, album_id).await?;

    Ok(HttpResponse::Ok().json(ApiResponse::new(AlbumDetail {
        album: AlbumSummary {
            id: row.id,
            title: row.title,
            description: row.description,
            visibility: row.visibility,
            cover_url: public_url_of(storage, row.cover_key),
            photos_count: row.photos_count,
            created_at: row.created_at,
        },
        owner_slug: row.owner_slug,
        photos,
    })))
}
