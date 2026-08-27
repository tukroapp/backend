use actix_web::{
    HttpResponse, Responder,
    web::{Data, Json, Path, Query},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use std::sync::Arc;

use crate::{
    routes::{
        community::{
            CommunityRole, community_limit_reached, is_at_community_limit,
            slug::{
                community_not_found, fetch_community_target, moderator_required, owner_required,
            },
        },
        friend::{resolve_slug, user_not_found},
        profile::ProfileSummary,
    },
    services::{
        activity::{NewActivity, record_activity},
        auth::SessionData,
        notification::{NotificationKind, notify_community_moderators},
        storage::{StorageService, public_url_of},
    },
    utils::{
        ApiCursorPaginatedResponse, ApiErrorResponse, ApiResponse, CursorPaginationQuery, db_err,
        take_page, unauthorized,
    },
};

#[derive(Serialize, ToSchema)]
pub struct CommunityMemberResponse {
    pub role: CommunityRole,
    pub joined_at: DateTime<Utc>,
    pub profile: ProfileSummary,
}

#[derive(Serialize, ToSchema)]
pub struct JoinResponse {
    /// `joined` quando a entrada foi imediata, `pending` quando virou pedido de
    /// aprovação por ser uma comunidade privada.
    #[schema(example = "joined")]
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct MembersQuery {
    /// Filtro opcional pelo nome (trecho) ou pelo início do slug do membro.
    pub q: Option<String>,
    /// Filtro opcional pelo papel: `member`, `admin` ou `owner`.
    pub role: Option<CommunityRole>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct RoleRequest {
    /// `member` ou `admin`. A troca para `owner` é feita pela transferência.
    pub role: CommunityRole,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct TransferRequest {
    /// Slug do perfil que vai receber a comunidade. Precisa já ser membro.
    pub slug: String,
}

fn not_a_member() -> HttpResponse {
    HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
        "member",
        "not_a_member",
        Some("Esse usuário não é membro da comunidade."),
        vec![],
    ))
}

pub async fn fetch_members(
    pool: &PgPool,
    storage: &dyn StorageService,
    community_id: Uuid,
    term: Option<&str>,
    role: Option<CommunityRole>,
    cursor: Option<Uuid>,
    limit: i64,
) -> Result<Vec<CommunityMemberResponse>, actix_web::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT
            cm.role AS "role: CommunityRole",
            cm.joined_at,
            up.user_id AS id,
            up.slug,
            up.name,
            up.avatar_url,
            up.status
        FROM community_member cm
        INNER JOIN auth_user au ON au.id = cm.user_id AND au.status = 'active'
        INNER JOIN user_profile up ON up.user_id = cm.user_id
        WHERE cm.community_id = $1
          AND ($2::uuid IS NULL OR cm.user_id < $2::uuid)
          AND (
              $3::text IS NULL
              OR up.name ILIKE '%' || $3 || '%'
              OR up.slug LIKE $3 || '%'
          )
          AND ($4::community_role IS NULL OR cm.role = $4)
        ORDER BY cm.user_id DESC
        LIMIT $5
        "#,
        community_id,
        cursor,
        term,
        role as Option<CommunityRole>,
        limit,
    )
    .fetch_all(pool)
    .await
    .map_err(db_err)?;

    Ok(rows
        .into_iter()
        .map(|row| CommunityMemberResponse {
            role: row.role,
            joined_at: row.joined_at,
            profile: ProfileSummary {
                id: row.id,
                slug: row.slug,
                name: row.name,
                avatar_url: public_url_of(storage, row.avatar_url),
                status: row.status,
            },
        })
        .collect())
}

#[utoipa::path(
    get,
    path = "/api/v1/community/{slug}/member",
    description = "Lista paginada (cursor) dos membros da comunidade, respeitando a privacidade",
    params(
        ("slug" = String, Path, description = "Slug público da comunidade"),
        ("q" = Option<String>, Query, description = "Filtra os membros por trecho do nome ou início do slug"),
        ("role" = Option<String>, Query, description = "Filtra por papel: `member`, `admin` ou `owner`"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Membros da comunidade", body = ApiCursorPaginatedResponse<Vec<CommunityMemberResponse>>),
        (status = 403, description = "Lista de membros privada", body = ApiErrorResponse),
        (status = 404, description = "Comunidade não encontrada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Lista os membros de uma comunidade", skip_all)]
pub async fn get(
    slug: Path<String>,
    query: Query<MembersQuery>,
    pagination: Query<CursorPaginationQuery>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let viewer = session.map(|session| session.user_id);

    let Some(target) = fetch_community_target(pool.get_ref(), &slug.into_inner(), viewer).await?
    else {
        return Ok(community_not_found());
    };

    if !target.can_view_members() {
        return Ok(
            HttpResponse::Forbidden().json(ApiErrorResponse::from_single_validation_error(
                "member",
                "private_member_list",
                Some("A lista de membros dessa comunidade é privada."),
                vec![],
            )),
        );
    }

    let limit = pagination.limit();
    // `_` e `%` viram literais para o filtro não virar curinga de LIKE.
    let term = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .map(|term| {
            term.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        });

    let mut members = fetch_members(
        pool.get_ref(),
        storage.as_ref().as_ref(),
        target.id,
        term.as_deref(),
        query.role,
        pagination.cursor_id(),
        limit + 1,
    )
    .await?;

    let next_cursor = take_page(&mut members, limit, |member| member.profile.id.to_string());

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(members, next_cursor)))
}

#[utoipa::path(
    post,
    path = "/api/v1/community/{slug}/member",
    description = "Entra na comunidade. Em comunidade privada cria um pedido de aprovação e avisa a moderação",
    params(("slug" = String, Path, description = "Slug público da comunidade")),
    responses(
        (status = 201, description = "Entrou ou pediu para entrar", body = ApiResponse<JoinResponse>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Comunidade não encontrada", body = ApiErrorResponse),
        (status = 409, description = "Já é membro, já tem pedido pendente ou atingiu o limite de comunidades", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Entra em uma comunidade", skip_all)]
pub async fn join(
    slug: Path<String>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    let Some(target) =
        fetch_community_target(pool.get_ref(), &slug.into_inner(), Some(session.user_id)).await?
    else {
        return Ok(community_not_found());
    };

    if target.is_member() {
        return Ok(
            HttpResponse::Conflict().json(ApiErrorResponse::from_single_validation_error(
                "member",
                "already_member",
                Some("Você já é membro dessa comunidade."),
                vec![],
            )),
        );
    }

    if is_at_community_limit(pool.get_ref(), session.user_id).await? {
        return Ok(community_limit_reached());
    }

    if !target.is_private {
        let mut tx = pool.begin().await.map_err(db_err)?;

        let joined = sqlx::query!(
            r#"
            INSERT INTO community_member (community_id, user_id)
            VALUES ($1, $2)
            ON CONFLICT DO NOTHING
            "#,
            target.id,
            session.user_id,
        )
        .execute(&mut *tx)
        .await
        .map_err(db_err)?
        .rows_affected();

        if joined == 1 {
            record_activity(
                &mut *tx,
                NewActivity::community_joined(session.user_id, target.id),
            )
            .await
            .map_err(db_err)?;
        }

        tx.commit().await.map_err(db_err)?;

        return Ok(HttpResponse::Created().json(ApiResponse::new(JoinResponse {
            status: "joined".to_string(),
        })));
    }

    let mut tx = pool.begin().await.map_err(db_err)?;

    // O índice parcial cobre só o pedido pendente, então recusado pode ser refeito.
    let created = sqlx::query_scalar!(
        r#"
        INSERT INTO community_join_request (community_id, user_id)
        VALUES ($1, $2)
        ON CONFLICT DO NOTHING
        RETURNING id
        "#,
        target.id,
        session.user_id,
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_err)?;

    if created.is_none() {
        return Ok(
            HttpResponse::Conflict().json(ApiErrorResponse::from_single_validation_error(
                "member",
                "request_pending",
                Some("Você já pediu para entrar nessa comunidade."),
                vec![],
            )),
        );
    }

    notify_community_moderators(
        &mut *tx,
        target.id,
        session.user_id,
        NotificationKind::CommunityJoinRequest,
    )
    .await
    .map_err(db_err)?;

    tx.commit().await.map_err(db_err)?;

    Ok(HttpResponse::Created().json(ApiResponse::new(JoinResponse {
        status: "pending".to_string(),
    })))
}

#[utoipa::path(
    delete,
    path = "/api/v1/community/{slug}/member",
    description = "Sai da comunidade. O dono precisa transferir a comunidade antes de sair",
    params(("slug" = String, Path, description = "Slug público da comunidade")),
    responses(
        (status = 204, description = "Saiu da comunidade"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 404, description = "Comunidade não encontrada ou você não é membro", body = ApiErrorResponse),
        (status = 409, description = "O dono não pode sair sem transferir", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Sai de uma comunidade", skip_all)]
pub async fn leave(
    slug: Path<String>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    let Some(target) =
        fetch_community_target(pool.get_ref(), &slug.into_inner(), Some(session.user_id)).await?
    else {
        return Ok(community_not_found());
    };

    if !target.is_member() {
        return Ok(not_a_member());
    }

    if target.is_owner() {
        return Ok(
            HttpResponse::Conflict().json(ApiErrorResponse::from_single_validation_error(
                "member",
                "owner_cannot_leave",
                Some("Transfira a comunidade para outro membro antes de sair."),
                vec![],
            )),
        );
    }

    sqlx::query!(
        r#"
        DELETE FROM community_member
        WHERE community_id = $1 AND user_id = $2
        "#,
        target.id,
        session.user_id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?;

    Ok(HttpResponse::NoContent().finish())
}

#[utoipa::path(
    delete,
    path = "/api/v1/community/{slug}/member/{user_slug}",
    description = "Expulsa um membro. Moderador só expulsa membro comum; o dono expulsa qualquer um",
    params(
        ("slug" = String, Path, description = "Slug público da comunidade"),
        ("user_slug" = String, Path, description = "Slug público do membro"),
    ),
    responses(
        (status = 204, description = "Membro expulso"),
        (status = 400, description = "Use sair da comunidade para si mesmo", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Sem permissão para expulsar", body = ApiErrorResponse),
        (status = 404, description = "Comunidade, perfil ou membro não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Expulsa um membro da comunidade", skip_all)]
pub async fn kick(
    path: Path<(String, String)>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    let (slug, user_slug) = path.into_inner();

    let Some(target) = fetch_community_target(pool.get_ref(), &slug, Some(session.user_id)).await?
    else {
        return Ok(community_not_found());
    };

    if !target.can_moderate() {
        return Ok(moderator_required());
    }

    let Some(member_id) = resolve_slug(pool.get_ref(), &user_slug).await? else {
        return Ok(user_not_found());
    };

    if member_id == session.user_id {
        return Ok(HttpResponse::BadRequest().json(
            ApiErrorResponse::from_single_validation_error(
                "member",
                "self_kick",
                Some("Use sair da comunidade para sair você mesmo."),
                vec![],
            ),
        ));
    }

    let member_role = sqlx::query_scalar!(
        r#"
        SELECT role AS "role: CommunityRole"
        FROM community_member
        WHERE community_id = $1 AND user_id = $2
        "#,
        target.id,
        member_id,
    )
    .fetch_optional(pool.get_ref())
    .await
    .map_err(db_err)?;

    let Some(member_role) = member_role else {
        return Ok(not_a_member());
    };

    if member_role != CommunityRole::Member && !target.is_owner() {
        return Ok(owner_required());
    }

    sqlx::query!(
        r#"
        DELETE FROM community_member
        WHERE community_id = $1 AND user_id = $2
        "#,
        target.id,
        member_id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?;

    Ok(HttpResponse::NoContent().finish())
}

#[utoipa::path(
    put,
    path = "/api/v1/community/{slug}/member/{user_slug}/role",
    description = "Promove um membro a moderador ou rebaixa um moderador a membro",
    params(
        ("slug" = String, Path, description = "Slug público da comunidade"),
        ("user_slug" = String, Path, description = "Slug público do membro"),
    ),
    request_body = RoleRequest,
    responses(
        (status = 204, description = "Papel atualizado"),
        (status = 400, description = "Papel inválido", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Apenas o dono muda papéis", body = ApiErrorResponse),
        (status = 404, description = "Comunidade, perfil ou membro não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Muda o papel de um membro", skip_all)]
pub async fn put_role(
    path: Path<(String, String)>,
    payload: Json<RoleRequest>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    let (slug, user_slug) = path.into_inner();

    let Some(target) = fetch_community_target(pool.get_ref(), &slug, Some(session.user_id)).await?
    else {
        return Ok(community_not_found());
    };

    if !target.is_owner() {
        return Ok(owner_required());
    }

    if payload.role == CommunityRole::Owner {
        return Ok(HttpResponse::BadRequest().json(
            ApiErrorResponse::from_single_validation_error(
                "role",
                "invalid_role",
                Some("Use a transferência da comunidade para definir um novo dono."),
                vec![],
            ),
        ));
    }

    let Some(member_id) = resolve_slug(pool.get_ref(), &user_slug).await? else {
        return Ok(user_not_found());
    };

    let updated = sqlx::query!(
        r#"
        UPDATE community_member
        SET role = $1
        WHERE community_id = $2 AND user_id = $3 AND role <> 'owner'
        "#,
        payload.role as CommunityRole,
        target.id,
        member_id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?
    .rows_affected();

    if updated == 0 {
        return Ok(not_a_member());
    }

    Ok(HttpResponse::NoContent().finish())
}

#[utoipa::path(
    put,
    path = "/api/v1/community/{slug}/owner",
    description = "Transfere a comunidade para outro membro. O dono atual vira moderador",
    params(("slug" = String, Path, description = "Slug público da comunidade")),
    request_body = TransferRequest,
    responses(
        (status = 204, description = "Comunidade transferida"),
        (status = 400, description = "Transferência para si mesmo", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Apenas o dono transfere", body = ApiErrorResponse),
        (status = 404, description = "Comunidade, perfil ou membro não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Transfere a comunidade para outro membro", skip_all)]
pub async fn transfer_owner(
    slug: Path<String>,
    payload: Json<TransferRequest>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
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

    let Some(new_owner_id) = resolve_slug(pool.get_ref(), &payload.slug).await? else {
        return Ok(user_not_found());
    };

    if new_owner_id == session.user_id {
        return Ok(HttpResponse::BadRequest().json(
            ApiErrorResponse::from_single_validation_error(
                "slug",
                "already_owner",
                Some("Você já é o dono dessa comunidade."),
                vec![],
            ),
        ));
    }

    let mut tx = pool.begin().await.map_err(db_err)?;

    // `idx_community_single_owner` não é deferrable, então o dono atual precisa
    // sair do papel antes do novo entrar.
    sqlx::query!(
        r#"
        UPDATE community_member
        SET role = 'admin'
        WHERE community_id = $1 AND user_id = $2
        "#,
        target.id,
        session.user_id,
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;

    let promoted = sqlx::query!(
        r#"
        UPDATE community_member
        SET role = 'owner'
        WHERE community_id = $1 AND user_id = $2
        "#,
        target.id,
        new_owner_id,
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?
    .rows_affected();

    if promoted == 0 {
        tx.rollback().await.map_err(db_err)?;
        return Ok(not_a_member());
    }

    tx.commit().await.map_err(db_err)?;

    Ok(HttpResponse::NoContent().finish())
}
