pub mod join_requests;
pub mod members;
pub mod polls;
pub mod topics;

use actix_multipart::form::MultipartForm;
use actix_web::{
    HttpResponse, Responder,
    web::{Data, Json, Path},
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

use std::{io::Read, sync::Arc};

use crate::{
    routes::{
        community::slug::members::{CommunityMemberResponse, fetch_members},
        community::{CommunityRequest, CommunityRole},
        me::AvatarUpload,
    },
    services::{
        auth::SessionData,
        storage::{COMMUNITY_AVATAR_PREFIX, StorageService, public_url_of, sniff_image},
    },
    utils::{ApiErrorResponse, ApiResponse, db_err, e500, invalid_field, unauthorized},
};

const MEMBERS_PREVIEW_LIMIT: i64 = 9;

/// Comunidade resolvida pelo slug junto da relação de quem está olhando com ela.
pub struct CommunityTarget {
    pub id: Uuid,
    pub slug: String,
    pub is_private: bool,
    pub viewer: Option<Uuid>,
    pub role: Option<CommunityRole>,
    pub has_pending_request: bool,
}

impl CommunityTarget {
    pub fn is_member(&self) -> bool {
        self.role.is_some()
    }

    pub fn is_owner(&self) -> bool {
        self.role == Some(CommunityRole::Owner)
    }

    pub fn can_moderate(&self) -> bool {
        matches!(self.role, Some(CommunityRole::Owner | CommunityRole::Admin))
    }

    pub fn can_view_members(&self) -> bool {
        !self.is_private || self.is_member()
    }

    pub fn can_join(&self) -> bool {
        self.viewer.is_some() && !self.is_member() && !self.has_pending_request
    }
}

#[derive(Serialize, ToSchema)]
pub struct CommunityResponse {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub avatar_url: Option<String>,
    pub is_private: bool,
    pub created_at: DateTime<Utc>,
    pub members_count: i64,
    /// Papel de quem está olhando: `member`, `admin`, `owner` ou `null` para
    /// quem não é membro.
    pub my_role: Option<CommunityRole>,
    pub has_pending_request: bool,
    pub can_join: bool,
    pub can_edit: bool,
    pub can_moderate: bool,
    /// `false` quando a comunidade é privada e quem está olhando não é membro.
    /// Nesse caso `members` vem vazio por falta de permissão.
    pub members_visible: bool,
    pub members: Vec<CommunityMemberResponse>,
    /// Pedidos de entrada aguardando resposta, `null` para quem não modera.
    pub pending_requests_count: Option<i64>,
}

pub fn community_not_found() -> HttpResponse {
    HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
        "community",
        "not_found",
        Some("Comunidade não encontrada."),
        vec![],
    ))
}

pub fn owner_required() -> HttpResponse {
    HttpResponse::Forbidden().json(ApiErrorResponse::from_single_validation_error(
        "community",
        "owner_required",
        Some("Apenas o dono da comunidade pode fazer isso."),
        vec![],
    ))
}

pub fn moderator_required() -> HttpResponse {
    HttpResponse::Forbidden().json(ApiErrorResponse::from_single_validation_error(
        "community",
        "moderator_required",
        Some("Apenas o dono e os moderadores da comunidade podem fazer isso."),
        vec![],
    ))
}

pub async fn fetch_community_target(
    pool: &PgPool,
    slug: &str,
    viewer: Option<Uuid>,
) -> Result<Option<CommunityTarget>, actix_web::Error> {
    let row = sqlx::query!(
        r#"
        SELECT
            c.id,
            c.slug,
            c.is_private,
            cm.role AS "role?: CommunityRole",
            EXISTS (
                SELECT 1 FROM community_join_request r
                WHERE r.community_id = c.id
                  AND r.user_id = $2::uuid
                  AND r.status = 'pending'
            ) AS "has_pending_request!"
        FROM community c
        LEFT JOIN community_member cm ON cm.community_id = c.id AND cm.user_id = $2::uuid
        WHERE c.slug = $1 AND c.deleted_at IS NULL
        "#,
        slug,
        viewer,
    )
    .fetch_optional(pool)
    .await
    .map_err(db_err)?;

    Ok(row.map(|row| CommunityTarget {
        id: row.id,
        slug: row.slug,
        is_private: row.is_private,
        viewer,
        role: row.role,
        has_pending_request: row.has_pending_request,
    }))
}

pub async fn fetch_detail(
    pool: &PgPool,
    storage: &dyn StorageService,
    target: &CommunityTarget,
) -> Result<CommunityResponse, actix_web::Error> {
    let row = sqlx::query!(
        r#"
        SELECT
            c.slug,
            c.name,
            c.description,
            c.avatar_url,
            c.is_private,
            c.created_at,
            (
                SELECT COUNT(*) FROM community_member cm
                INNER JOIN auth_user au ON au.id = cm.user_id AND au.status = 'active'
                WHERE cm.community_id = c.id
            ) AS "members_count!",
            (
                SELECT COUNT(*) FROM community_join_request r
                WHERE r.community_id = c.id AND r.status = 'pending'
            ) AS "pending_requests_count!"
        FROM community c
        WHERE c.id = $1
        "#,
        target.id,
    )
    .fetch_one(pool)
    .await
    .map_err(db_err)?;

    let members_visible = !row.is_private || target.is_member();

    let members = if members_visible {
        fetch_members(
            pool,
            storage,
            target.id,
            None,
            None,
            None,
            MEMBERS_PREVIEW_LIMIT,
        )
        .await?
    } else {
        Vec::new()
    };

    Ok(CommunityResponse {
        id: target.id,
        slug: row.slug,
        name: row.name,
        description: row.description,
        avatar_url: public_url_of(storage, row.avatar_url),
        is_private: row.is_private,
        created_at: row.created_at,
        members_count: row.members_count,
        my_role: target.role,
        has_pending_request: target.has_pending_request,
        can_join: target.can_join(),
        can_edit: target.is_owner(),
        can_moderate: target.can_moderate(),
        members_visible,
        members,
        pending_requests_count: target.can_moderate().then_some(row.pending_requests_count),
    })
}

#[utoipa::path(
    get,
    path = "/api/v1/community/{slug}",
    description = "Retorna a comunidade pelo slug, com os primeiros membros quando a privacidade permitir",
    params(("slug" = String, Path, description = "Slug público da comunidade")),
    responses(
        (status = 200, description = "Comunidade", body = ApiResponse<CommunityResponse>),
        (status = 404, description = "Comunidade não encontrada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Retorna uma comunidade por slug", skip_all)]
pub async fn get(
    slug: Path<String>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let viewer = session.map(|session| session.user_id);

    let Some(target) = fetch_community_target(pool.get_ref(), &slug.into_inner(), viewer).await?
    else {
        return Ok(community_not_found());
    };

    let community = fetch_detail(pool.get_ref(), storage.as_ref().as_ref(), &target).await?;

    Ok(HttpResponse::Ok().json(ApiResponse::new(community)))
}

#[utoipa::path(
    put,
    path = "/api/v1/community/{slug}",
    description = "Atualiza a comunidade. O slug é imutável e não muda com o nome",
    params(("slug" = String, Path, description = "Slug público da comunidade")),
    request_body = CommunityRequest,
    responses(
        (status = 200, description = "Comunidade atualizada", body = ApiResponse<CommunityResponse>),
        (status = 400, description = "Dados inválidos", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Apenas o dono edita", body = ApiErrorResponse),
        (status = 404, description = "Comunidade não encontrada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Atualiza uma comunidade", skip_all)]
pub async fn put(
    slug: Path<String>,
    payload: Json<CommunityRequest>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    if let Err(errors) = payload.validate() {
        return Ok(HttpResponse::BadRequest().json(ApiErrorResponse::from(errors)));
    }

    let Some(target) =
        fetch_community_target(pool.get_ref(), &slug.into_inner(), Some(session.user_id)).await?
    else {
        return Ok(community_not_found());
    };

    if !target.is_owner() {
        return Ok(owner_required());
    }

    let description = payload
        .description
        .as_deref()
        .map(str::trim)
        .filter(|description| !description.is_empty());

    sqlx::query!(
        r#"
        UPDATE community
        SET name = $1, description = $2, is_private = $3
        WHERE id = $4
        "#,
        payload.name.trim(),
        description,
        payload.is_private,
        target.id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?;

    let community = fetch_detail(pool.get_ref(), storage.as_ref().as_ref(), &target).await?;

    Ok(HttpResponse::Ok().json(ApiResponse::new(community)))
}

#[utoipa::path(
    delete,
    path = "/api/v1/community/{slug}",
    description = "Exclui a comunidade junto de membros, pedidos, tópicos e comentários",
    params(("slug" = String, Path, description = "Slug público da comunidade")),
    responses(
        (status = 204, description = "Comunidade excluída"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Apenas o dono exclui", body = ApiErrorResponse),
        (status = 404, description = "Comunidade não encontrada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Exclui uma comunidade", skip_all)]
pub async fn delete(
    slug: Path<String>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    let Some(target) =
        fetch_community_target(pool.get_ref(), &slug.into_inner(), Some(session.user_id)).await?
    else {
        return Ok(community_not_found());
    };

    if !target.is_owner() {
        return Ok(owner_required());
    }

    let avatar = sqlx::query_scalar!(
        r#"
        DELETE FROM community
        WHERE id = $1
        RETURNING avatar_url
        "#,
        target.id,
    )
    .fetch_one(pool.get_ref())
    .await
    .map_err(db_err)?;

    if let Some(avatar) = avatar
        && let Err(error) = storage.delete(&avatar).await
    {
        tracing::error!(%error, "falha ao remover o avatar da comunidade excluída");
    }

    Ok(HttpResponse::NoContent().finish())
}

/// Grava a nova key, devolvendo a anterior para que o objeto antigo seja removido.
async fn replace_avatar(
    pool: &PgPool,
    community_id: &Uuid,
    key: Option<String>,
) -> Result<Option<String>, actix_web::Error> {
    let row = sqlx::query!(
        r#"
        UPDATE community
        SET avatar_url = $1
        WHERE id = $2
        RETURNING (SELECT avatar_url FROM community WHERE id = $2) AS previous
        "#,
        key,
        community_id,
    )
    .fetch_one(pool)
    .await
    .map_err(db_err)?;

    Ok(row.previous)
}

#[utoipa::path(
    post,
    path = "/api/v1/community/{slug}/avatar",
    description = "Envia a imagem da comunidade e remove a anterior",
    params(("slug" = String, Path, description = "Slug público da comunidade")),
    request_body(content = AvatarUpload, content_type = "multipart/form-data"),
    responses(
        (status = 200, description = "Imagem atualizada", body = ApiResponse<CommunityResponse>),
        (status = 400, description = "Arquivo vazio ou formato não suportado", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Apenas o dono altera a imagem", body = ApiErrorResponse),
        (status = 404, description = "Comunidade não encontrada", body = ApiErrorResponse),
        (status = 413, description = "Arquivo maior que 2 MB", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Atualiza a imagem da comunidade", skip_all)]
pub async fn post_avatar(
    slug: Path<String>,
    MultipartForm(form): MultipartForm<AvatarUpload>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    let Some(target) =
        fetch_community_target(pool.get_ref(), &slug.into_inner(), Some(session.user_id)).await?
    else {
        return Ok(community_not_found());
    };

    if !target.is_owner() {
        return Ok(owner_required());
    }

    let mut data = Vec::with_capacity(form.file.size);
    form.file
        .file
        .as_file()
        .read_to_end(&mut data)
        .map_err(e500)?;

    // O formato vem dos bytes, nunca do nome do arquivo: o bucket é público.
    let Some(image) = sniff_image(&data) else {
        return Ok(invalid_field(
            "file",
            "unsupported_image",
            "Envie uma imagem JPEG, PNG, WebP ou GIF.",
        ));
    };

    let key = storage
        .upload(COMMUNITY_AVATAR_PREFIX, &target.id, &data, image)
        .await
        .map_err(e500)?;

    let previous = match replace_avatar(pool.get_ref(), &target.id, Some(key.clone())).await {
        Ok(previous) => previous,
        Err(e) => {
            let _ = storage.delete(&key).await;
            return Err(e);
        }
    };

    if let Some(previous) = previous
        && let Err(error) = storage.delete(&previous).await
    {
        tracing::error!(%error, "falha ao remover a imagem anterior da comunidade");
    }

    let community = fetch_detail(pool.get_ref(), storage.as_ref().as_ref(), &target).await?;

    Ok(HttpResponse::Ok().json(ApiResponse::new(community)))
}

#[utoipa::path(
    delete,
    path = "/api/v1/community/{slug}/avatar",
    description = "Remove a imagem da comunidade",
    params(("slug" = String, Path, description = "Slug público da comunidade")),
    responses(
        (status = 200, description = "Imagem removida", body = ApiResponse<CommunityResponse>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Apenas o dono altera a imagem", body = ApiErrorResponse),
        (status = 404, description = "Comunidade não encontrada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Remove a imagem da comunidade", skip_all)]
pub async fn delete_avatar(
    slug: Path<String>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    let Some(target) =
        fetch_community_target(pool.get_ref(), &slug.into_inner(), Some(session.user_id)).await?
    else {
        return Ok(community_not_found());
    };

    if !target.is_owner() {
        return Ok(owner_required());
    }

    let previous = replace_avatar(pool.get_ref(), &target.id, None).await?;

    if let Some(previous) = previous
        && let Err(error) = storage.delete(&previous).await
    {
        tracing::error!(%error, "falha ao remover a imagem da comunidade");
    }

    let community = fetch_detail(pool.get_ref(), storage.as_ref().as_ref(), &target).await?;

    Ok(HttpResponse::Ok().json(ApiResponse::new(community)))
}
