use actix_web::{
    HttpResponse, Responder,
    web::{Data, Json, Path, Query},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use std::sync::Arc;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

use crate::{
    routes::profile::ProfileSummary,
    services::{
        auth::SessionData,
        storage::{StorageService, public_url_of},
    },
    utils::{
        ApiCursorPaginatedResponse, ApiErrorResponse, CursorPaginationQuery, db_err, invalid_field,
        take_page,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, ToSchema, sqlx::Type)]
#[sqlx(type_name = "moderation_action", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ModerationAction {
    ContentRemoved,
    ContentRestored,
    UserSuspended,
    UserUnsuspended,
}

/// Os tipos de conteúdo que a moderação tira do ar. Perfil fica de fora: conta
/// não se remove, se suspende.
#[derive(Debug, Clone, Copy)]
pub enum ContentKind {
    Scrap,
    Testimonial,
    Community,
    Topic,
    Comment,
    Poll,
}

impl ContentKind {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "scrap" => Some(ContentKind::Scrap),
            "testimonial" => Some(ContentKind::Testimonial),
            "community" => Some(ContentKind::Community),
            "topic" => Some(ContentKind::Topic),
            "comment" => Some(ContentKind::Comment),
            "poll" => Some(ContentKind::Poll),
            _ => None,
        }
    }
}

/// O que sobra de um conteúdo depois que ele sai do ar: o texto guardado no log
/// e o autor, que é quem responde por ele.
struct RemovedContent {
    snapshot: String,
    author_id: Option<Uuid>,
    label: String,
}

#[derive(Debug, Deserialize, ToSchema, Validate)]
pub struct ModerationRequest {
    /// Justificativa registrada no histórico.
    #[validate(length(min = 1, max = 500))]
    pub reason: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ModerationLogEntry {
    pub id: Uuid,
    pub action: ModerationAction,
    pub reason: Option<String>,
    /// Texto do conteúdo no momento da remoção. Continua aqui mesmo depois que
    /// a linha original some.
    pub content_snapshot: Option<String>,
    /// Slug do perfil ou da comunidade atingida, como estava na hora da ação.
    pub target_label: Option<String>,
    /// Tipo do conteúdo atingido, `null` numa ação sobre a conta.
    pub content_kind: Option<String>,
    pub content_id: Option<Uuid>,
    /// A denúncia que originou a ação, quando houve uma.
    pub report_id: Option<Uuid>,
    /// `null` quando a conta do moderador foi removida.
    pub moderator: Option<ProfileSummary>,
    pub created_at: DateTime<Utc>,
}

/// Um registro no histórico. As colunas de alvo são preenchidas conforme o que
/// a ação atingiu, e `target_user_id` sempre aponta para a conta responsável.
pub struct LogEntry<'a> {
    pub moderator_id: Uuid,
    pub action: ModerationAction,
    pub report_id: Option<Uuid>,
    pub reason: Option<&'a str>,
    pub content_snapshot: Option<&'a str>,
    pub target_user_id: Option<Uuid>,
    pub target_label: Option<&'a str>,
    pub content: Option<(ContentKind, Uuid)>,
}

pub async fn log_action(
    connection: &mut PgConnection,
    entry: LogEntry<'_>,
) -> Result<(), actix_web::Error> {
    let (mut scrap, mut testimonial, mut community, mut topic, mut comment, mut poll) =
        (None, None, None, None, None, None);

    if let Some((kind, id)) = entry.content {
        match kind {
            ContentKind::Scrap => scrap = Some(id),
            ContentKind::Testimonial => testimonial = Some(id),
            ContentKind::Community => community = Some(id),
            ContentKind::Topic => topic = Some(id),
            ContentKind::Comment => comment = Some(id),
            ContentKind::Poll => poll = Some(id),
        }
    }

    sqlx::query!(
        r#"
        INSERT INTO moderation_log (
            moderator_id, action, report_id, reason, content_snapshot,
            target_user_id, target_label,
            scrap_id, testimonial_id, community_id, topic_id, comment_id, poll_id
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
        "#,
        entry.moderator_id,
        entry.action as ModerationAction,
        entry.report_id,
        entry.reason,
        entry.content_snapshot,
        entry.target_user_id,
        entry.target_label,
        scrap,
        testimonial,
        community,
        topic,
        comment,
        poll,
    )
    .execute(connection)
    .await
    .map_err(db_err)?;

    Ok(())
}

fn content_not_found() -> HttpResponse {
    HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
        "content",
        "not_found",
        Some("Conteúdo não encontrado."),
        vec![],
    ))
}

/// Tira o conteúdo do ar e devolve o que precisa ir para o log. `None` quando o
/// conteúdo não existe ou já estava fora do ar.
async fn soft_delete(
    tx: &mut Transaction<'_, Postgres>,
    kind: ContentKind,
    id: Uuid,
) -> Result<Option<RemovedContent>, actix_web::Error> {
    let removed = match kind {
        ContentKind::Scrap => sqlx::query!(
            r#"
            UPDATE scrap s
            SET deleted_at = NOW()
            WHERE s.id = $1 AND s.deleted_at IS NULL
            RETURNING
                s.body,
                s.author_id,
                (SELECT up.slug FROM user_profile up WHERE up.user_id = s.author_id) AS "slug!"
            "#,
            id,
        )
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_err)?
        .map(|row| RemovedContent {
            snapshot: row.body,
            author_id: Some(row.author_id),
            label: row.slug,
        }),

        ContentKind::Testimonial => sqlx::query!(
            r#"
            UPDATE testimonial t
            SET deleted_at = NOW()
            WHERE t.id = $1 AND t.deleted_at IS NULL
            RETURNING
                t.body,
                t.author_id,
                (SELECT up.slug FROM user_profile up WHERE up.user_id = t.author_id) AS "slug!"
            "#,
            id,
        )
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_err)?
        .map(|row| RemovedContent {
            snapshot: row.body,
            author_id: Some(row.author_id),
            label: row.slug,
        }),

        ContentKind::Community => sqlx::query!(
            r#"
            UPDATE community c
            SET deleted_at = NOW()
            WHERE c.id = $1 AND c.deleted_at IS NULL
            RETURNING c.slug, c.name, c.description
            "#,
            id,
        )
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_err)?
        .map(|row| RemovedContent {
            snapshot: match row.description {
                Some(description) => format!("{}\n\n{}", row.name, description),
                None => row.name,
            },
            // A comunidade não tem um responsável único: o dono pode ter mudado.
            author_id: None,
            label: row.slug,
        }),

        ContentKind::Topic => sqlx::query!(
            r#"
            UPDATE community_topic t
            SET deleted_at = NOW()
            WHERE t.id = $1 AND t.deleted_at IS NULL
            RETURNING
                t.title,
                t.body,
                t.author_id,
                (SELECT up.slug FROM user_profile up WHERE up.user_id = t.author_id) AS "slug!"
            "#,
            id,
        )
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_err)?
        .map(|row| RemovedContent {
            snapshot: format!("{}\n\n{}", row.title, row.body),
            author_id: Some(row.author_id),
            label: row.slug,
        }),

        ContentKind::Comment => sqlx::query!(
            r#"
            UPDATE community_topic_comment c
            SET deleted_at = NOW()
            WHERE c.id = $1 AND c.deleted_at IS NULL
            RETURNING
                c.body,
                c.author_id,
                (SELECT up.slug FROM user_profile up WHERE up.user_id = c.author_id) AS "slug!"
            "#,
            id,
        )
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_err)?
        .map(|row| RemovedContent {
            snapshot: row.body,
            author_id: Some(row.author_id),
            label: row.slug,
        }),

        ContentKind::Poll => sqlx::query!(
            r#"
            UPDATE community_poll p
            SET deleted_at = NOW()
            WHERE p.id = $1 AND p.deleted_at IS NULL
            RETURNING
                p.question,
                p.author_id,
                (SELECT up.slug FROM user_profile up WHERE up.user_id = p.author_id) AS "slug!"
            "#,
            id,
        )
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_err)?
        .map(|row| RemovedContent {
            snapshot: row.question,
            author_id: Some(row.author_id),
            label: row.slug,
        }),
    };

    Ok(removed)
}

/// Devolve o conteúdo ao ar. `false` quando ele não existe ou já está no ar.
async fn undelete(
    tx: &mut Transaction<'_, Postgres>,
    kind: ContentKind,
    id: Uuid,
) -> Result<bool, actix_web::Error> {
    let affected = match kind {
        ContentKind::Scrap => sqlx::query!(
            r#"UPDATE scrap SET deleted_at = NULL WHERE id = $1 AND deleted_at IS NOT NULL"#,
            id,
        )
        .execute(&mut **tx)
        .await,
        ContentKind::Testimonial => sqlx::query!(
            r#"UPDATE testimonial SET deleted_at = NULL WHERE id = $1 AND deleted_at IS NOT NULL"#,
            id,
        )
        .execute(&mut **tx)
        .await,
        ContentKind::Community => sqlx::query!(
            r#"UPDATE community SET deleted_at = NULL WHERE id = $1 AND deleted_at IS NOT NULL"#,
            id,
        )
        .execute(&mut **tx)
        .await,
        ContentKind::Topic => sqlx::query!(
            r#"UPDATE community_topic SET deleted_at = NULL WHERE id = $1 AND deleted_at IS NOT NULL"#,
            id,
        )
        .execute(&mut **tx)
        .await,
        ContentKind::Comment => sqlx::query!(
            r#"UPDATE community_topic_comment SET deleted_at = NULL WHERE id = $1 AND deleted_at IS NOT NULL"#,
            id,
        )
        .execute(&mut **tx)
        .await,
        ContentKind::Poll => sqlx::query!(
            r#"UPDATE community_poll SET deleted_at = NULL WHERE id = $1 AND deleted_at IS NOT NULL"#,
            id,
        )
        .execute(&mut **tx)
        .await,
    };

    Ok(affected.map_err(db_err)?.rows_affected() > 0)
}

/// A última ação da moderação sobre esse conteúdo. É o que separa "a moderação
/// removeu" de "o autor apagou": só o primeiro caso pode ser restaurado.
async fn last_action_on(
    tx: &mut Transaction<'_, Postgres>,
    kind: ContentKind,
    id: Uuid,
) -> Result<Option<ModerationAction>, actix_web::Error> {
    let (mut scrap, mut testimonial, mut community, mut topic, mut comment, mut poll) =
        (None, None, None, None, None, None);

    match kind {
        ContentKind::Scrap => scrap = Some(id),
        ContentKind::Testimonial => testimonial = Some(id),
        ContentKind::Community => community = Some(id),
        ContentKind::Topic => topic = Some(id),
        ContentKind::Comment => comment = Some(id),
        ContentKind::Poll => poll = Some(id),
    }

    sqlx::query_scalar!(
        r#"
        SELECT action AS "action: ModerationAction"
        FROM moderation_log
        WHERE action IN ('content_removed', 'content_restored')
          AND (scrap_id = $1 OR testimonial_id = $2 OR community_id = $3
               OR topic_id = $4 OR comment_id = $5 OR poll_id = $6)
        ORDER BY id DESC
        LIMIT 1
        "#,
        scrap,
        testimonial,
        community,
        topic,
        comment,
        poll,
    )
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_err)
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/report/{id}/remove",
    description = "Tira do ar o conteúdo denunciado, registra a ação no histórico e encerra a denúncia como procedente",
    params(("id" = Uuid, Path, description = "Id da denúncia")),
    request_body = ModerationRequest,
    responses(
        (status = 204, description = "Conteúdo removido e denúncia resolvida"),
        (status = 400, description = "Justificativa inválida ou denúncia de perfil, que se resolve com suspensão", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Não é administrador", body = ApiErrorResponse),
        (status = 404, description = "Denúncia não encontrada", body = ApiErrorResponse),
        (status = 409, description = "O conteúdo já estava fora do ar", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Remove o conteúdo denunciado", skip_all)]
pub async fn remove(
    id: Path<Uuid>,
    payload: Json<ModerationRequest>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    if let Err(errors) = payload.validate() {
        return Ok(HttpResponse::BadRequest().json(ApiErrorResponse::from(errors)));
    }

    let id = id.into_inner();
    let report = sqlx::query!(
        r#"
        SELECT reported_user_id, scrap_id, testimonial_id, community_id, topic_id, comment_id, poll_id
        FROM report
        WHERE id = $1
        "#,
        id,
    )
    .fetch_optional(pool.get_ref())
    .await
    .map_err(db_err)?;

    let Some(report) = report else {
        return Ok(
            HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
                "report",
                "not_found",
                Some("Denúncia não encontrada."),
                vec![],
            )),
        );
    };

    let target = if let Some(target) = report.scrap_id {
        (ContentKind::Scrap, target)
    } else if let Some(target) = report.testimonial_id {
        (ContentKind::Testimonial, target)
    } else if let Some(target) = report.community_id {
        (ContentKind::Community, target)
    } else if let Some(target) = report.topic_id {
        (ContentKind::Topic, target)
    } else if let Some(target) = report.comment_id {
        (ContentKind::Comment, target)
    } else if let Some(target) = report.poll_id {
        (ContentKind::Poll, target)
    } else {
        return Ok(invalid_field(
            "report",
            "profile_report",
            "Denúncia de perfil não remove conteúdo: use a suspensão da conta.",
        ));
    };

    let mut tx = pool.begin().await.map_err(db_err)?;

    let Some(removed) = soft_delete(&mut tx, target.0, target.1).await? else {
        return Ok(
            HttpResponse::Conflict().json(ApiErrorResponse::from_single_validation_error(
                "content",
                "already_removed",
                Some("Esse conteúdo já está fora do ar."),
                vec![],
            )),
        );
    };

    log_action(
        &mut tx,
        LogEntry {
            moderator_id: session.user_id,
            action: ModerationAction::ContentRemoved,
            report_id: Some(id),
            reason: payload.reason.as_deref().map(str::trim),
            content_snapshot: Some(&removed.snapshot),
            target_user_id: removed.author_id,
            target_label: Some(&removed.label),
            content: Some(target),
        },
    )
    .await?;

    sqlx::query!(
        r#"
        UPDATE report
        SET status = 'resolved', reviewed_by = $2, reviewed_at = NOW()
        WHERE id = $1
        "#,
        id,
        session.user_id,
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;

    tx.commit().await.map_err(db_err)?;

    Ok(HttpResponse::NoContent().finish())
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/content/{kind}/{id}/restore",
    description = "Devolve ao ar um conteúdo que a moderação tinha removido. O que o próprio autor apagou não é restaurado",
    params(
        ("kind" = String, Path, description = "scrap, testimonial, community, topic, comment ou poll"),
        ("id" = Uuid, Path, description = "Id do conteúdo"),
    ),
    request_body = ModerationRequest,
    responses(
        (status = 204, description = "Conteúdo de volta ao ar"),
        (status = 400, description = "Tipo desconhecido ou justificativa inválida", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Não é administrador", body = ApiErrorResponse),
        (status = 404, description = "Conteúdo não encontrado, já no ar, ou apagado pelo próprio autor", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Restaura um conteúdo removido", skip_all)]
pub async fn restore(
    path: Path<(String, Uuid)>,
    payload: Json<ModerationRequest>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    if let Err(errors) = payload.validate() {
        return Ok(HttpResponse::BadRequest().json(ApiErrorResponse::from(errors)));
    }

    let (kind, id) = path.into_inner();
    let Some(kind) = ContentKind::parse(&kind) else {
        return Ok(invalid_field(
            "kind",
            "invalid_kind",
            "O tipo precisa ser 'scrap', 'testimonial', 'community', 'topic', 'comment' ou 'poll'.",
        ));
    };

    let mut tx = pool.begin().await.map_err(db_err)?;

    // Restaurar o que o autor apagou seria republicar conteúdo por cima da
    // vontade dele, então só volta o que a moderação tirou.
    if last_action_on(&mut tx, kind, id).await? != Some(ModerationAction::ContentRemoved) {
        return Ok(content_not_found());
    }

    if !undelete(&mut tx, kind, id).await? {
        return Ok(content_not_found());
    }

    log_action(
        &mut tx,
        LogEntry {
            moderator_id: session.user_id,
            action: ModerationAction::ContentRestored,
            report_id: None,
            reason: payload.reason.as_deref().map(str::trim),
            content_snapshot: None,
            target_user_id: None,
            target_label: None,
            content: Some((kind, id)),
        },
    )
    .await?;

    tx.commit().await.map_err(db_err)?;

    Ok(HttpResponse::NoContent().finish())
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/user/{slug}/history",
    description = "Histórico da moderação sobre uma conta: remoções de conteúdo, suspensões e restaurações, da mais nova para a mais antiga",
    params(
        ("slug" = String, Path, description = "Slug do perfil"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Histórico", body = ApiCursorPaginatedResponse<Vec<ModerationLogEntry>>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Não é administrador", body = ApiErrorResponse),
        (status = 404, description = "Usuário não encontrado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Histórico da moderação sobre uma conta", skip_all)]
pub async fn history(
    slug: Path<String>,
    pagination: Query<CursorPaginationQuery>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let user_id = sqlx::query_scalar!(
        r#"SELECT user_id FROM user_profile WHERE slug = $1"#,
        slug.into_inner(),
    )
    .fetch_optional(pool.get_ref())
    .await
    .map_err(db_err)?;

    let Some(user_id) = user_id else {
        return Ok(
            HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
                "user",
                "not_found",
                Some("Usuário não encontrado."),
                vec![],
            )),
        );
    };

    let limit = pagination.limit();
    let rows = sqlx::query!(
        r#"
        SELECT
            l.id,
            l.action AS "action: ModerationAction",
            l.reason,
            l.content_snapshot,
            l.target_label,
            l.report_id,
            CASE
                WHEN l.scrap_id IS NOT NULL THEN 'scrap'
                WHEN l.testimonial_id IS NOT NULL THEN 'testimonial'
                WHEN l.community_id IS NOT NULL THEN 'community'
                WHEN l.topic_id IS NOT NULL THEN 'topic'
                WHEN l.comment_id IS NOT NULL THEN 'comment'
                WHEN l.poll_id IS NOT NULL THEN 'poll'
            END AS content_kind,
            COALESCE(l.scrap_id, l.testimonial_id, l.community_id, l.topic_id,
                     l.comment_id, l.poll_id) AS content_id,
            l.created_at,
            moderator.user_id AS "moderator_id?",
            moderator.slug AS "moderator_slug?",
            moderator.name AS "moderator_name?",
            moderator.avatar_url AS "moderator_avatar_url?",
            moderator.status AS "moderator_status?"
        FROM moderation_log l
        LEFT JOIN user_profile moderator ON moderator.user_id = l.moderator_id
        WHERE l.target_user_id = $1
          AND ($2::uuid IS NULL OR l.id < $2::uuid)
        ORDER BY l.id DESC
        LIMIT $3
        "#,
        user_id,
        pagination.cursor_id(),
        limit + 1,
    )
    .fetch_all(pool.get_ref())
    .await
    .map_err(db_err)?;

    let storage = storage.as_ref().as_ref();
    let mut results = rows
        .into_iter()
        .map(|row| ModerationLogEntry {
            id: row.id,
            action: row.action,
            reason: row.reason,
            content_snapshot: row.content_snapshot,
            target_label: row.target_label,
            content_kind: row.content_kind,
            content_id: row.content_id,
            report_id: row.report_id,
            moderator: row.moderator_id.map(|id| ProfileSummary {
                id,
                slug: row.moderator_slug.unwrap_or_default(),
                name: row.moderator_name.unwrap_or_default(),
                avatar_url: public_url_of(storage, row.moderator_avatar_url),
                status: row.moderator_status,
            }),
            created_at: row.created_at,
        })
        .collect::<Vec<_>>();

    let next_cursor = take_page(&mut results, limit, |item| item.id.to_string());

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(results, next_cursor)))
}
