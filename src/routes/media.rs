use actix_multipart::form::{MultipartForm, tempfile::TempFile};
use actix_web::{
    HttpResponse, Responder,
    web::{Data, Path},
};
use chrono::{DateTime, Utc};
use mime_guess::mime;
use serde::{Deserialize, Serialize};
use sqlx::{PgExecutor, PgPool};
use utoipa::ToSchema;
use uuid::Uuid;

use std::{io::Read, sync::Arc};

use crate::{
    constants::{MAX_PHOTOS_PER_USER, MAX_UPLOAD_SIZE_BYTES, MAX_VIDEOS_PER_USER},
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
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, MultipartForm, ToSchema)]
pub struct MediaUpload {
    /// Imagem JPEG, PNG, WebP ou GIF, ou vídeo MP4, MOV ou WebM, de no máximo 25 MB.
    #[multipart(limit = "25 MiB")]
    #[schema(value_type = String, format = Binary)]
    pub file: TempFile,
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

fn media_not_found() -> HttpResponse {
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
            created_at
        FROM user_media
        WHERE user_id = $1
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
        INSERT INTO user_media (user_id, kind, storage_key, content_type, size_bytes)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id, created_at
        "#,
        session.user_id,
        kind as MediaKind,
        key,
        file.content_type,
        size_bytes,
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
