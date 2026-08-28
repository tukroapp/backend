pub mod comments;

use actix_multipart::form::{MultipartForm, tempfile::TempFile, text::Text};
use actix_web::{
    HttpResponse, Responder,
    web::{Data, Json, Path},
};
use chrono::{DateTime, Utc};
use mime_guess::mime;
use serde::{Deserialize, Serialize};
use sqlx::{PgExecutor, PgPool};
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

use std::{io::Read, sync::Arc};

use crate::{
    constants::{
        MAX_PHOTOS_PER_ALBUM, MAX_PHOTOS_PER_USER, MAX_UPLOAD_SIZE_BYTES, MAX_VIDEOS_PER_USER,
    },
    services::{
        activity::{NewActivity, record_activity},
        auth::SessionData,
        storage::{MEDIA_PREFIX, StorageService, sniff_image, sniff_video},
    },
    utils::{ApiErrorResponse, ApiResponse, db_err, e500, invalid_field},
};

/// Espelha o enum `media_kind` do banco.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[sqlx(type_name = "media_kind", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Photo,
    Video,
}

impl MediaKind {
    fn limit(self) -> i64 {
        match self {
            MediaKind::Photo => MAX_PHOTOS_PER_USER,
            MediaKind::Video => MAX_VIDEOS_PER_USER,
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct MediaResponse {
    pub id: Uuid,
    pub kind: MediaKind,
    pub url: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub caption: Option<String>,
    /// `null` quando a foto está solta, fora de qualquer álbum.
    pub album_id: Option<Uuid>,
    pub position: i16,
    pub comments_count: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, MultipartForm, ToSchema)]
pub struct MediaUpload {
    /// Imagem JPEG, PNG, WebP ou GIF, ou vídeo MP4, MOV ou WebM, de no máximo 25 MB.
    #[multipart(limit = "25 MiB")]
    #[schema(value_type = String, format = Binary)]
    pub file: TempFile,
    /// Álbum de destino. Sem ele, o arquivo fica solto na galeria.
    #[schema(value_type = Option<String>)]
    pub album_id: Option<Text<Uuid>>,
    #[schema(value_type = Option<String>)]
    pub caption: Option<Text<String>>,
}

/// Descarte pelo nome do arquivo, só para falhar antes de ler 25 MB do disco.
/// Quem decide de verdade é o sniff dos bytes: o nome vem do cliente. Sem nome
/// ou com extensão desconhecida, deixa passar e o sniff resolve.
fn name_looks_like_media(file_name: Option<&str>) -> bool {
    let Some(guess) = file_name.and_then(|name| mime_guess::from_path(name).first()) else {
        return true;
    };

    matches!(guess.type_(), mime::IMAGE | mime::VIDEO)
}

fn unsupported_media() -> HttpResponse {
    invalid_field(
        "file",
        "unsupported_media",
        "Envie uma imagem JPEG, PNG, WebP ou GIF, ou um vídeo MP4, MOV ou WebM.",
    )
}

pub fn media_not_found() -> HttpResponse {
    HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
        "media",
        "not_found",
        Some("Arquivo não encontrado."),
        vec![],
    ))
}

fn media_limit_reached(kind: MediaKind) -> HttpResponse {
    let (code, message) = match kind {
        MediaKind::Photo => (
            "photo_limit_reached",
            format!("Você já tem {MAX_PHOTOS_PER_USER} fotos. Apague uma para enviar outra."),
        ),
        MediaKind::Video => (
            "video_limit_reached",
            format!("Você já tem {MAX_VIDEOS_PER_USER} vídeos. Apague um para enviar outro."),
        ),
    };

    HttpResponse::Conflict().json(ApiErrorResponse::from_single_validation_error(
        "file",
        code,
        Some(&message),
        vec![],
    ))
}

async fn count_media(
    executor: impl PgExecutor<'_>,
    user_id: Uuid,
    kind: MediaKind,
) -> Result<i64, actix_web::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT COUNT(*) AS "count!"
        FROM user_media
        WHERE user_id = $1 AND kind = $2
        "#,
        user_id,
        kind as MediaKind,
    )
    .fetch_one(executor)
    .await
    .map_err(db_err)
}

/// Fotos e vídeos de um perfil, do mais novo para o mais antigo. `kind` filtra
/// por tipo quando vier.
pub async fn fetch_media(
    pool: &PgPool,
    storage: &dyn StorageService,
    user_id: Uuid,
    kind: Option<MediaKind>,
    cursor: Option<Uuid>,
    limit: i64,
) -> Result<Vec<MediaResponse>, actix_web::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT
            id,
            kind AS "kind: MediaKind",
            storage_key,
            content_type,
            size_bytes,
            caption,
            album_id,
            position,
            created_at,
            (
                SELECT COUNT(*) FROM media_comment mc
                WHERE mc.media_id = user_media.id AND mc.deleted_at IS NULL
            ) AS "comments_count!"
        FROM user_media
        WHERE user_id = $1
          AND deleted_at IS NULL
          AND ($2::media_kind IS NULL OR kind = $2)
          AND ($3::uuid IS NULL OR id < $3::uuid)
        ORDER BY id DESC
        LIMIT $4
        "#,
        user_id,
        kind as Option<MediaKind>,
        cursor,
        limit,
    )
    .fetch_all(pool)
    .await
    .map_err(db_err)?;

    Ok(rows
        .into_iter()
        .map(|row| MediaResponse {
            id: row.id,
            kind: row.kind,
            url: storage.public_url(&row.storage_key),
            content_type: row.content_type,
            size_bytes: row.size_bytes,
            caption: row.caption,
            album_id: row.album_id,
            position: row.position,
            comments_count: row.comments_count,
            created_at: row.created_at,
        })
        .collect())
}

#[utoipa::path(
    post,
    path = "/api/v1/me/media",
    description = "Envia uma foto ou um vídeo para o álbum do usuário autenticado. O tipo vem dos bytes do arquivo, não do nome",
    request_body(content = MediaUpload, content_type = "multipart/form-data"),
    responses(
        (status = 201, description = "Arquivo enviado", body = ApiResponse<MediaResponse>),
        (status = 400, description = "Arquivo vazio, grande demais ou em formato não suportado", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 409, description = "Limite de fotos ou de vídeos atingido", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Envia uma foto ou vídeo para o álbum", skip_all)]
pub async fn post(
    MultipartForm(form): MultipartForm<MediaUpload>,
    session: SessionData,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    session.require_verified_email(pool.get_ref()).await?;

    if form.file.size == 0 {
        return Ok(invalid_field(
            "file",
            "empty_file",
            "O arquivo enviado está vazio.",
        ));
    }

    if form.file.size > MAX_UPLOAD_SIZE_BYTES {
        return Ok(invalid_field(
            "file",
            "file_too_large",
            "O arquivo excede o limite de 25 MB.",
        ));
    }

    if !name_looks_like_media(form.file.file_name.as_deref()) {
        return Ok(unsupported_media());
    }

    let mut data = Vec::with_capacity(form.file.size);
    form.file
        .file
        .as_file()
        .read_to_end(&mut data)
        .map_err(e500)?;

    let Some((kind, file)) = sniff_image(&data)
        .map(|file| (MediaKind::Photo, file))
        .or_else(|| sniff_video(&data).map(|file| (MediaKind::Video, file)))
    else {
        return Ok(unsupported_media());
    };

    // Checagem antes do upload para não gastar 25 MB de rede à toa. A contagem
    // que vale roda na transação, junto do INSERT.
    if count_media(pool.get_ref(), session.user_id, kind).await? >= kind.limit() {
        return Ok(media_limit_reached(kind));
    }

    let album_id = form.album_id.as_ref().map(|album| album.0);
    let caption = form
        .caption
        .as_ref()
        .map(|caption| caption.0.trim().to_string())
        .filter(|caption| !caption.is_empty());

    if let Some(caption) = &caption
        && caption.chars().count() > 500
    {
        return Ok(invalid_field(
            "caption",
            "too_long",
            "A legenda pode ter no máximo 500 caracteres.",
        ));
    }

    // O álbum precisa ser seu e ter espaço. Vídeo não entra em álbum: álbum é
    // de foto, e o vídeo fica na galeria.
    if let Some(album_id) = album_id {
        match album_room(pool.get_ref(), session.user_id, album_id).await? {
            AlbumRoom::NotFound => return Ok(album_not_found()),
            AlbumRoom::Full => return Ok(album_full()),
            AlbumRoom::Available => {}
        }
    }

    let key = storage
        .upload(MEDIA_PREFIX, &session.user_id, &data, file)
        .await
        .map_err(e500)?;

    let size_bytes = data.len() as i64;

    let mut tx = pool.begin().await.map_err(db_err)?;

    if count_media(&mut *tx, session.user_id, kind).await? >= kind.limit() {
        tx.rollback().await.map_err(db_err)?;
        let _ = storage.delete(&key).await;
        return Ok(media_limit_reached(kind));
    }

    let created = sqlx::query!(
        r#"
        INSERT INTO user_media
            (user_id, kind, storage_key, content_type, size_bytes, album_id, caption)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING id, created_at
        "#,
        session.user_id,
        kind as MediaKind,
        key,
        file.content_type,
        size_bytes,
        album_id,
        caption,
    )
    .fetch_one(&mut *tx)
    .await;

    let created = match created {
        Ok(created) => created,
        Err(e) => {
            // Não deixa objeto órfão no bucket quando o banco recusa.
            let _ = storage.delete(&key).await;
            return Err(db_err(e));
        }
    };

    if let Err(e) = record_activity(
        &mut *tx,
        NewActivity::media_added(session.user_id, created.id),
    )
    .await
    {
        let _ = storage.delete(&key).await;
        return Err(db_err(e));
    }

    tx.commit().await.map_err(db_err)?;

    Ok(
        HttpResponse::Created().json(ApiResponse::new(MediaResponse {
            id: created.id,
            kind,
            url: storage.public_url(&key),
            content_type: file.content_type.to_string(),
            size_bytes,
            caption: caption.clone(),
            album_id,
            position: 0,
            comments_count: 0,
            created_at: created.created_at,
        })),
    )
}

#[utoipa::path(
    delete,
    path = "/api/v1/me/media/{id}",
    description = "Apaga uma foto ou vídeo do álbum do usuário autenticado, removendo também o objeto do bucket",
    params(("id" = Uuid, Path, description = "Id do arquivo")),
    responses(
        (status = 204, description = "Arquivo apagado"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Arquivo não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Apaga uma foto ou vídeo do álbum", skip_all)]
pub async fn delete(
    id: Path<Uuid>,
    session: SessionData,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let removed = sqlx::query_scalar!(
        r#"
        DELETE FROM user_media
        WHERE id = $1 AND user_id = $2
        RETURNING storage_key
        "#,
        id.into_inner(),
        session.user_id,
    )
    .fetch_optional(pool.get_ref())
    .await
    .map_err(db_err)?;

    let Some(key) = removed else {
        return Ok(media_not_found());
    };

    if let Err(error) = storage.delete(&key).await {
        tracing::error!(%error, "falha ao remover o arquivo do álbum");
    }

    Ok(HttpResponse::NoContent().finish())
}

/// O que impede (ou não) mais uma foto no álbum.
enum AlbumRoom {
    NotFound,
    Full,
    Available,
}

async fn album_room(
    pool: &PgPool,
    user_id: Uuid,
    album_id: Uuid,
) -> Result<AlbumRoom, actix_web::Error> {
    let count = sqlx::query_scalar!(
        r#"
        SELECT COUNT(*) AS "count!"
        FROM user_media
        WHERE album_id = $1
          AND EXISTS (SELECT 1 FROM album WHERE id = $1 AND user_id = $2)
        "#,
        album_id,
        user_id,
    )
    .fetch_one(pool)
    .await
    .map_err(db_err)?;

    let exists = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM album WHERE id = $1 AND user_id = $2) AS "exists!""#,
        album_id,
        user_id,
    )
    .fetch_one(pool)
    .await
    .map_err(db_err)?;

    if !exists {
        return Ok(AlbumRoom::NotFound);
    }

    if count >= MAX_PHOTOS_PER_ALBUM {
        return Ok(AlbumRoom::Full);
    }

    Ok(AlbumRoom::Available)
}

pub fn album_not_found() -> HttpResponse {
    HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
        "album",
        "not_found",
        Some("Álbum não encontrado."),
        vec![],
    ))
}

fn album_full() -> HttpResponse {
    let message = format!("Esse álbum já tem {MAX_PHOTOS_PER_ALBUM} fotos.");

    HttpResponse::Conflict().json(ApiErrorResponse::from_single_validation_error(
        "album",
        "album_full",
        Some(&message),
        vec![],
    ))
}

/// As fotos de um álbum, na ordem que o dono definiu. Sem paginação: o álbum
/// tem teto de fotos, então a página inteira cabe numa resposta.
pub async fn fetch_album_media(
    pool: &PgPool,
    storage: &dyn StorageService,
    album_id: Uuid,
) -> Result<Vec<MediaResponse>, actix_web::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT
            id,
            kind AS "kind: MediaKind",
            storage_key,
            content_type,
            size_bytes,
            caption,
            album_id,
            position,
            created_at,
            (
                SELECT COUNT(*) FROM media_comment mc
                WHERE mc.media_id = user_media.id AND mc.deleted_at IS NULL
            ) AS "comments_count!"
        FROM user_media
        WHERE album_id = $1 AND deleted_at IS NULL
        ORDER BY position, id
        "#,
        album_id,
    )
    .fetch_all(pool)
    .await
    .map_err(db_err)?;

    Ok(rows
        .into_iter()
        .map(|row| MediaResponse {
            id: row.id,
            kind: row.kind,
            url: storage.public_url(&row.storage_key),
            content_type: row.content_type,
            size_bytes: row.size_bytes,
            caption: row.caption,
            album_id: row.album_id,
            position: row.position,
            comments_count: row.comments_count,
            created_at: row.created_at,
        })
        .collect())
}

#[derive(Debug, Deserialize, ToSchema, Validate)]
pub struct MediaEditRequest {
    #[validate(length(max = 500))]
    pub caption: Option<String>,
    /// Move a foto para outro álbum. `null` mantém onde está.
    pub album_id: Option<Uuid>,
    /// Posição dentro do álbum, para ordenar a página.
    pub position: Option<i16>,
}

#[utoipa::path(
    put,
    path = "/api/v1/me/media/{id}",
    description = "Edita a legenda, o álbum e a posição de uma foto",
    params(("id" = Uuid, Path, description = "Id do arquivo")),
    request_body = MediaEditRequest,
    responses(
        (status = 204, description = "Arquivo atualizado"),
        (status = 400, description = "Dados inválidos", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Arquivo ou álbum não encontrado", body = ApiErrorResponse),
        (status = 409, description = "O álbum de destino está cheio", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Edita uma foto", skip_all)]
pub async fn put(
    id: Path<Uuid>,
    payload: Json<MediaEditRequest>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    if let Err(errors) = payload.validate() {
        return Ok(HttpResponse::BadRequest().json(ApiErrorResponse::from(errors)));
    }

    if let Some(album_id) = payload.album_id {
        match album_room(pool.get_ref(), session.user_id, album_id).await? {
            AlbumRoom::NotFound => return Ok(album_not_found()),
            AlbumRoom::Full => return Ok(album_full()),
            AlbumRoom::Available => {}
        }
    }

    let caption = payload
        .caption
        .as_deref()
        .map(str::trim)
        .filter(|caption| !caption.is_empty());

    let updated = sqlx::query!(
        r#"
        UPDATE user_media
        SET caption = $3,
            album_id = COALESCE($4, album_id),
            position = COALESCE($5, position)
        WHERE id = $1 AND user_id = $2
        "#,
        id.into_inner(),
        session.user_id,
        caption,
        payload.album_id,
        payload.position,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?
    .rows_affected();

    if updated == 0 {
        return Ok(media_not_found());
    }

    Ok(HttpResponse::NoContent().finish())
}
