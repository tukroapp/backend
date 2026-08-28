use actix_web::{
    HttpResponse, Responder,
    web::{Data, Path, Query},
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use std::sync::Arc;

use crate::{
    routes::profile::ProfileSummary,
    services::{
        auth::{AccountStatus, SessionData},
        storage::{StorageService, public_url_of},
    },
    utils::{
        ApiCursorPaginatedResponse, ApiErrorResponse, ApiResponse, CursorPaginationQuery, db_err,
        invalid_field, take_page,
    },
};

/// Quantos itens recentes do usuário denunciado o detalhe carrega. É material
/// de conferência, não listagem: se o moderador precisar de mais, abre o perfil.
const RECENT_LIMIT: i64 = 10;

/// Quantos caracteres do conteúdo denunciado a fila mostra. O suficiente para
/// decidir sem abrir o detalhe.
const EXCERPT_LIMIT: i32 = 280;

/// O usuário responsável pelo conteúdo denunciado: o próprio perfil na denúncia
/// de perfil, o autor nos outros alvos. Denúncia de comunidade não tem um.
#[derive(Serialize, ToSchema)]
pub struct AccountableUser {
    pub profile: ProfileSummary,
    pub account_status: AccountStatus,
    /// Denúncias contra o perfil dessa conta, sem contar as de conteúdo.
    pub profile_reports_count: i64,
    /// Quantas pessoas já bloquearam essa conta. Sinal forte de abuso.
    pub blocked_by_count: i64,
}

/// O alvo resumido para a fila, uniforme entre os tipos: o moderador lê o
/// conteúdo sem abrir o detalhe.
#[derive(Serialize, ToSchema)]
pub struct ReportTargetSummary {
    /// `profile`, `scrap`, `testimonial`, `community`, `topic`, `comment` ou `poll`.
    pub kind: String,
    /// Id do alvo. Na denúncia de perfil é o id do usuário.
    pub id: Uuid,
    /// Slug para montar o link: o do perfil, o do mural, o da comunidade.
    pub slug: Option<String>,
    /// Nome da comunidade, título do tópico ou pergunta da enquete.
    pub title: Option<String>,
    /// Primeiros caracteres do corpo do conteúdo denunciado.
    pub excerpt: Option<String>,
    /// `true` quando o conteúdo já foi apagado. Continua visível aqui.
    pub is_deleted: bool,
}

#[derive(Serialize, ToSchema)]
pub struct ReportListItem {
    pub id: Uuid,
    pub reason: String,
    /// Texto que o denunciante escreveu, vem inteiro para a triagem.
    pub details: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub reporter: ProfileSummary,
    pub target: ReportTargetSummary,
    /// `null` na denúncia de comunidade, que não tem um responsável único.
    pub accountable: Option<AccountableUser>,
}

/// A conta denunciada vista pela moderação, sem filtro de privacidade: é o
/// ponto do painel.
#[derive(Serialize, ToSchema)]
pub struct ReportedUser {
    pub id: Uuid,
    pub email: String,
    pub account_status: AccountStatus,
    /// Preenchido só em conta suspensa com prazo. `null` em suspensão
    /// permanente.
    pub suspended_until: Option<DateTime<Utc>>,
    pub is_email_verified: bool,
    pub registered_at: DateTime<Utc>,
    pub slug: String,
    pub name: String,
    pub avatar_url: Option<String>,
    pub status: Option<String>,
    pub bio: Option<String>,
    pub city: Option<String>,
    pub birthdate: Option<NaiveDate>,
    pub friends_count: i64,
    /// Recados que essa conta escreveu, no mural de qualquer pessoa.
    pub scraps_written_count: i64,
    /// Recados que essa conta recebeu no próprio mural.
    pub scraps_received_count: i64,
    pub testimonials_written_count: i64,
    /// Quantas pessoas já bloquearam essa conta. Sinal forte de abuso.
    pub blocked_by_count: i64,
    /// Denúncias contra o perfil dessa conta, incluindo a que está sendo vista.
    pub reports_count: i64,
}

#[derive(Serialize, ToSchema)]
pub struct RecentScrap {
    pub id: Uuid,
    pub body: String,
    pub created_at: DateTime<Utc>,
    /// Dono do mural onde o recado foi escrito.
    pub wall_slug: String,
    pub is_reply: bool,
    /// `true` quando o autor ou o dono do mural apagou. O conteúdo some das
    /// telas normais e continua aqui: é o ponto do soft delete.
    pub is_deleted: bool,
}

#[derive(Serialize, ToSchema)]
pub struct RecentTestimonial {
    pub id: Uuid,
    pub body: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    /// Perfil para quem o depoimento foi escrito.
    pub profile_slug: String,
    /// `true` quando foi apagado. Continua visível só para a moderação.
    pub is_deleted: bool,
}

#[derive(Serialize, ToSchema)]
pub struct OtherReport {
    pub id: Uuid,
    pub reason: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub reporter_slug: String,
}

/// O conteúdo denunciado, com os campos que cada tipo de alvo tem. `kind` é a
/// tag no JSON, então o front lê um objeto só e decide pelo tipo.
#[derive(Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReportTargetDetail {
    /// Denúncia de perfil: o conteúdo é o próprio bloco `reported`.
    Profile,
    Scrap {
        id: Uuid,
        body: String,
        created_at: DateTime<Utc>,
        /// Dono do mural onde o recado foi escrito.
        wall_slug: String,
        is_reply: bool,
        is_deleted: bool,
    },
    Testimonial {
        id: Uuid,
        body: String,
        status: String,
        created_at: DateTime<Utc>,
        /// Perfil para quem o depoimento foi escrito.
        profile_slug: String,
        is_deleted: bool,
    },
    Community {
        id: Uuid,
        slug: String,
        name: String,
        description: Option<String>,
        is_private: bool,
        created_at: DateTime<Utc>,
        members_count: i64,
        /// Dono da comunidade, que é quem responde por ela.
        owner_slug: Option<String>,
        is_deleted: bool,
    },
    Topic {
        id: Uuid,
        title: String,
        body: String,
        created_at: DateTime<Utc>,
        community_slug: String,
        is_locked: bool,
        is_deleted: bool,
    },
    Comment {
        id: Uuid,
        body: String,
        created_at: DateTime<Utc>,
        topic_id: Uuid,
        topic_title: String,
        community_slug: String,
        is_deleted: bool,
    },
    Poll {
        id: Uuid,
        question: String,
        created_at: DateTime<Utc>,
        community_slug: String,
        is_closed: bool,
        closes_at: DateTime<Utc>,
        is_deleted: bool,
        options: Vec<String>,
    },
}

#[derive(Serialize, ToSchema)]
pub struct ReportDetail {
    pub id: Uuid,
    pub reason: String,
    pub details: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub reviewed_by: Option<ProfileSummary>,
    pub reporter: ProfileSummary,
    /// O conteúdo denunciado.
    pub target: ReportTargetDetail,
    /// A conta responsável pelo alvo. `null` em denúncia de comunidade.
    pub reported: Option<ReportedUser>,
    /// Últimos recados escritos pela conta denunciada, do mais novo ao mais antigo.
    pub recent_scraps: Vec<RecentScrap>,
    /// Últimos depoimentos escritos pela conta denunciada.
    pub recent_testimonials: Vec<RecentTestimonial>,
    /// Outras denúncias contra a mesma conta.
    pub other_reports: Vec<OtherReport>,
}

/// Fica separado de `CursorPaginationQuery` porque `#[serde(flatten)]` faria o
/// serde ler todos os valores da query string como texto, quebrando `limit`.
#[derive(Debug, Deserialize)]
pub struct QueueQuery {
    /// Filtra por `open`, `reviewing`, `resolved` ou `dismissed`. Sem filtro,
    /// vêm todas.
    pub status: Option<String>,
    /// Filtra pelo tipo de alvo: `profile`, `scrap`, `testimonial`,
    /// `community`, `topic`, `comment` ou `poll`.
    pub kind: Option<String>,
}

fn report_not_found() -> HttpResponse {
    HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
        "report",
        "not_found",
        Some("Denúncia não encontrada."),
        vec![],
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/report",
    description = "Fila de denúncias para a moderação, da mais nova para a mais antiga, com o conteúdo denunciado e o histórico da conta responsável",
    params(
        ("status" = Option<String>, Query, description = "open, reviewing, resolved ou dismissed"),
        ("kind" = Option<String>, Query, description = "profile, scrap, testimonial, community, topic, comment ou poll"),
        CursorPaginationQuery,
    ),
    responses(
        (status = 200, description = "Denúncias", body = ApiCursorPaginatedResponse<Vec<ReportListItem>>),
        (status = 400, description = "Status ou tipo inválido", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Não é administrador", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Lista a fila de denúncias", skip_all)]
pub async fn get(
    query: Query<QueueQuery>,
    pagination: Query<CursorPaginationQuery>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    if let Some(status) = query.status.as_deref()
        && !matches!(status, "open" | "reviewing" | "resolved" | "dismissed")
    {
        return Ok(invalid_field(
            "status",
            "invalid_status",
            "O status precisa ser 'open', 'reviewing', 'resolved' ou 'dismissed'.",
        ));
    }

    if let Some(kind) = query.kind.as_deref()
        && !matches!(
            kind,
            "profile" | "scrap" | "testimonial" | "community" | "topic" | "comment" | "poll"
        )
    {
        return Ok(invalid_field(
            "kind",
            "invalid_kind",
            "O tipo precisa ser 'profile', 'scrap', 'testimonial', 'community', 'topic', 'comment' ou 'poll'.",
        ));
    }

    let limit = pagination.limit();

    // A CTE resolve o alvo (tipo, link, conteúdo) e o responsável por ele; a
    // consulta de fora só monta o perfil e os contadores desse responsável.
    let rows = sqlx::query!(
        r#"
        WITH base AS (
            SELECT
                r.id,
                r.reason::TEXT AS reason,
                r.details,
                r.status::TEXT AS status,
                r.created_at,
                r.reviewed_at,
                r.reporter_id,
                CASE
                    WHEN r.reported_user_id IS NOT NULL THEN 'profile'
                    WHEN r.scrap_id IS NOT NULL THEN 'scrap'
                    WHEN r.testimonial_id IS NOT NULL THEN 'testimonial'
                    WHEN r.community_id IS NOT NULL THEN 'community'
                    WHEN r.topic_id IS NOT NULL THEN 'topic'
                    WHEN r.comment_id IS NOT NULL THEN 'comment'
                    ELSE 'poll'
                END AS target_kind,
                COALESCE(r.reported_user_id, r.scrap_id, r.testimonial_id, r.community_id,
                         r.topic_id, r.comment_id, r.poll_id) AS target_id,
                COALESCE(r.reported_user_id, s.author_id, t.author_id, ct.author_id,
                         cc.author_id, cp.author_id) AS accountable_id,
                COALESCE(reported.slug, wall.slug, target_profile.slug, c.slug,
                         topic_community.slug, comment_community.slug, poll_community.slug) AS target_slug,
                COALESCE(c.name, ct.title, comment_topic.title, cp.question) AS target_title,
                LEFT(COALESCE(s.body, t.body, ct.body, cc.body), $4) AS target_excerpt,
                COALESCE(s.deleted_at, t.deleted_at, c.deleted_at, ct.deleted_at,
                         cc.deleted_at, cp.deleted_at) IS NOT NULL AS target_is_deleted
            FROM report r
            LEFT JOIN user_profile reported ON reported.user_id = r.reported_user_id
            LEFT JOIN scrap s ON s.id = r.scrap_id
            LEFT JOIN user_profile wall ON wall.user_id = s.profile_id
            LEFT JOIN testimonial t ON t.id = r.testimonial_id
            LEFT JOIN user_profile target_profile ON target_profile.user_id = t.profile_id
            LEFT JOIN community c ON c.id = r.community_id
            LEFT JOIN community_topic ct ON ct.id = r.topic_id
            LEFT JOIN community topic_community ON topic_community.id = ct.community_id
            LEFT JOIN community_topic_comment cc ON cc.id = r.comment_id
            LEFT JOIN community_topic comment_topic ON comment_topic.id = cc.topic_id
            LEFT JOIN community comment_community ON comment_community.id = comment_topic.community_id
            LEFT JOIN community_poll cp ON cp.id = r.poll_id
            LEFT JOIN community poll_community ON poll_community.id = cp.community_id
            WHERE ($1::TEXT IS NULL OR r.status::TEXT = $1)
              AND ($2::uuid IS NULL OR r.id < $2::uuid)
        )
        SELECT
            b.id AS "id!",
            b.reason AS "reason!",
            b.details,
            b.status AS "status!",
            b.created_at AS "created_at!",
            b.reviewed_at,
            b.target_kind AS "target_kind!",
            b.target_id AS "target_id!",
            b.target_slug,
            b.target_title,
            b.target_excerpt,
            b.target_is_deleted AS "target_is_deleted!",
            reporter.user_id AS "reporter_id!",
            reporter.slug AS "reporter_slug!",
            reporter.name AS "reporter_name!",
            reporter.avatar_url AS reporter_avatar_url,
            reporter.status AS reporter_status,
            accountable.user_id AS "accountable_id?",
            accountable.slug AS "accountable_slug?",
            accountable.name AS "accountable_name?",
            accountable.avatar_url AS "accountable_avatar_url?",
            accountable.status AS "accountable_status?",
            accountable_user.status AS "account_status?: AccountStatus",
            (SELECT COUNT(*) FROM report r2 WHERE r2.reported_user_id = b.accountable_id)
                AS "profile_reports_count?",
            (SELECT COUNT(*) FROM block bl WHERE bl.blocked_id = b.accountable_id)
                AS "blocked_by_count?"
        FROM base b
        INNER JOIN user_profile reporter ON reporter.user_id = b.reporter_id
        LEFT JOIN user_profile accountable ON accountable.user_id = b.accountable_id
        LEFT JOIN auth_user accountable_user ON accountable_user.id = b.accountable_id
        WHERE $5::TEXT IS NULL OR b.target_kind = $5
        ORDER BY b.id DESC
        LIMIT $3
        "#,
        query.status,
        pagination.cursor_id(),
        limit + 1,
        EXCERPT_LIMIT,
        query.kind,
    )
    .fetch_all(pool.get_ref())
    .await
    .map_err(db_err)?;

    let storage = storage.as_ref().as_ref();
    let mut results = rows
        .into_iter()
        .map(|row| ReportListItem {
            id: row.id,
            reason: row.reason,
            details: row.details,
            status: row.status,
            created_at: row.created_at,
            reviewed_at: row.reviewed_at,
            reporter: ProfileSummary {
                id: row.reporter_id,
                slug: row.reporter_slug,
                name: row.reporter_name,
                avatar_url: public_url_of(storage, row.reporter_avatar_url),
                status: row.reporter_status,
            },
            target: ReportTargetSummary {
                kind: row.target_kind,
                id: row.target_id,
                slug: row.target_slug,
                title: row.target_title,
                excerpt: row.target_excerpt,
                is_deleted: row.target_is_deleted,
            },
            accountable: row.accountable_id.map(|id| AccountableUser {
                profile: ProfileSummary {
                    id,
                    slug: row.accountable_slug.unwrap_or_default(),
                    name: row.accountable_name.unwrap_or_default(),
                    avatar_url: public_url_of(storage, row.accountable_avatar_url),
                    status: row.accountable_status,
                },
                account_status: row.account_status.unwrap_or(AccountStatus::Active),
                profile_reports_count: row.profile_reports_count.unwrap_or_default(),
                blocked_by_count: row.blocked_by_count.unwrap_or_default(),
            }),
        })
        .collect::<Vec<_>>();

    let next_cursor = take_page(&mut results, limit, |item| item.id.to_string());

    Ok(HttpResponse::Ok().json(ApiCursorPaginatedResponse::new(results, next_cursor)))
}

/// Carrega o conteúdo do alvo da denúncia. Cada tipo tem a sua consulta porque
/// os campos que interessam ao moderador são diferentes.
async fn load_target(
    pool: &PgPool,
    report: &ReportRow,
) -> Result<Option<ReportTargetDetail>, actix_web::Error> {
    if report.reported_user_id.is_some() {
        return Ok(Some(ReportTargetDetail::Profile));
    }

    if let Some(id) = report.scrap_id {
        let row = sqlx::query!(
            r#"
            SELECT
                s.body,
                s.created_at,
                s.parent_id IS NOT NULL AS "is_reply!",
                s.deleted_at IS NOT NULL AS "is_deleted!",
                wall.slug AS wall_slug
            FROM scrap s
            INNER JOIN user_profile wall ON wall.user_id = s.profile_id
            WHERE s.id = $1
            "#,
            id,
        )
        .fetch_optional(pool)
        .await
        .map_err(db_err)?;

        return Ok(row.map(|row| ReportTargetDetail::Scrap {
            id,
            body: row.body,
            created_at: row.created_at,
            wall_slug: row.wall_slug,
            is_reply: row.is_reply,
            is_deleted: row.is_deleted,
        }));
    }

    if let Some(id) = report.testimonial_id {
        let row = sqlx::query!(
            r#"
            SELECT
                t.body,
                t.status::TEXT AS "status!",
                t.created_at,
                t.deleted_at IS NOT NULL AS "is_deleted!",
                target.slug AS profile_slug
            FROM testimonial t
            INNER JOIN user_profile target ON target.user_id = t.profile_id
            WHERE t.id = $1
            "#,
            id,
        )
        .fetch_optional(pool)
        .await
        .map_err(db_err)?;

        return Ok(row.map(|row| ReportTargetDetail::Testimonial {
            id,
            body: row.body,
            status: row.status,
            created_at: row.created_at,
            profile_slug: row.profile_slug,
            is_deleted: row.is_deleted,
        }));
    }

    if let Some(id) = report.community_id {
        let row = sqlx::query!(
            r#"
            SELECT
                c.slug,
                c.name,
                c.description,
                c.is_private,
                c.created_at,
                c.deleted_at IS NOT NULL AS "is_deleted!",
                (SELECT COUNT(*) FROM community_member cm WHERE cm.community_id = c.id)
                    AS "members_count!",
                (
                    SELECT up.slug
                    FROM community_member cm
                    INNER JOIN user_profile up ON up.user_id = cm.user_id
                    WHERE cm.community_id = c.id AND cm.role = 'owner'
                ) AS "owner_slug?"
            FROM community c
            WHERE c.id = $1
            "#,
            id,
        )
        .fetch_optional(pool)
        .await
        .map_err(db_err)?;

        return Ok(row.map(|row| ReportTargetDetail::Community {
            id,
            slug: row.slug,
            name: row.name,
            description: row.description,
            is_private: row.is_private,
            created_at: row.created_at,
            members_count: row.members_count,
            owner_slug: row.owner_slug,
            is_deleted: row.is_deleted,
        }));
    }

    if let Some(id) = report.topic_id {
        let row = sqlx::query!(
            r#"
            SELECT
                ct.title,
                ct.body,
                ct.created_at,
                ct.is_locked,
                ct.deleted_at IS NOT NULL AS "is_deleted!",
                c.slug AS community_slug
            FROM community_topic ct
            INNER JOIN community c ON c.id = ct.community_id
            WHERE ct.id = $1
            "#,
            id,
        )
        .fetch_optional(pool)
        .await
        .map_err(db_err)?;

        return Ok(row.map(|row| ReportTargetDetail::Topic {
            id,
            title: row.title,
            body: row.body,
            created_at: row.created_at,
            community_slug: row.community_slug,
            is_locked: row.is_locked,
            is_deleted: row.is_deleted,
        }));
    }

    if let Some(id) = report.comment_id {
        let row = sqlx::query!(
            r#"
            SELECT
                cc.body,
                cc.created_at,
                cc.deleted_at IS NOT NULL AS "is_deleted!",
                ct.id AS topic_id,
                ct.title AS topic_title,
                c.slug AS community_slug
            FROM community_topic_comment cc
            INNER JOIN community_topic ct ON ct.id = cc.topic_id
            INNER JOIN community c ON c.id = ct.community_id
            WHERE cc.id = $1
            "#,
            id,
        )
        .fetch_optional(pool)
        .await
        .map_err(db_err)?;

        return Ok(row.map(|row| ReportTargetDetail::Comment {
            id,
            body: row.body,
            created_at: row.created_at,
            topic_id: row.topic_id,
            topic_title: row.topic_title,
            community_slug: row.community_slug,
            is_deleted: row.is_deleted,
        }));
    }

    if let Some(id) = report.poll_id {
        let row = sqlx::query!(
            r#"
            SELECT
                cp.question,
                cp.created_at,
                cp.is_closed,
                cp.closes_at,
                cp.deleted_at IS NOT NULL AS "is_deleted!",
                c.slug AS community_slug,
                ARRAY(
                    SELECT o.text
                    FROM community_poll_option o
                    WHERE o.poll_id = cp.id
                    ORDER BY o.position
                ) AS "options!"
            FROM community_poll cp
            INNER JOIN community c ON c.id = cp.community_id
            WHERE cp.id = $1
            "#,
            id,
        )
        .fetch_optional(pool)
        .await
        .map_err(db_err)?;

        return Ok(row.map(|row| ReportTargetDetail::Poll {
            id,
            question: row.question,
            created_at: row.created_at,
            community_slug: row.community_slug,
            is_closed: row.is_closed,
            closes_at: row.closes_at,
            is_deleted: row.is_deleted,
            options: row.options,
        }));
    }

    Ok(None)
}

/// A linha da denúncia com os ids de alvo, do jeito que `load_target` precisa.
struct ReportRow {
    reported_user_id: Option<Uuid>,
    scrap_id: Option<Uuid>,
    testimonial_id: Option<Uuid>,
    community_id: Option<Uuid>,
    topic_id: Option<Uuid>,
    comment_id: Option<Uuid>,
    poll_id: Option<Uuid>,
}

/// Carrega o bloco da conta responsável pelo alvo, com os contadores que o
/// moderador usa para decidir.
async fn load_reported_user(
    pool: &PgPool,
    user_id: Uuid,
    storage: &dyn StorageService,
) -> Result<ReportedUser, actix_web::Error> {
    let reported = sqlx::query!(
        r#"
        SELECT
            au.id,
            au.email AS "email!: String",
            au.status AS "account_status: AccountStatus",
            au.suspended_until,
            au.is_email_verified,
            au.created_at AS registered_at,
            up.slug,
            up.name,
            up.avatar_url,
            up.status,
            up.bio,
            up.city,
            up.birthdate,
            (SELECT COUNT(*) FROM friend f WHERE f.user_id = au.id) AS "friends_count!",
            (SELECT COUNT(*) FROM scrap s WHERE s.author_id = au.id) AS "scraps_written_count!",
            (SELECT COUNT(*) FROM scrap s WHERE s.profile_id = au.id) AS "scraps_received_count!",
            (SELECT COUNT(*) FROM testimonial t WHERE t.author_id = au.id) AS "testimonials_written_count!",
            (SELECT COUNT(*) FROM block b WHERE b.blocked_id = au.id) AS "blocked_by_count!",
            (SELECT COUNT(*) FROM report r WHERE r.reported_user_id = au.id) AS "reports_count!"
        FROM auth_user au
        INNER JOIN user_profile up ON up.user_id = au.id
        WHERE au.id = $1
        "#,
        user_id,
    )
    .fetch_one(pool)
    .await
    .map_err(db_err)?;

    Ok(ReportedUser {
        id: reported.id,
        email: reported.email,
        account_status: reported.account_status,
        suspended_until: reported.suspended_until,
        is_email_verified: reported.is_email_verified,
        registered_at: reported.registered_at,
        slug: reported.slug,
        name: reported.name,
        avatar_url: public_url_of(storage, reported.avatar_url),
        status: reported.status,
        bio: reported.bio,
        city: reported.city,
        birthdate: reported.birthdate,
        friends_count: reported.friends_count,
        scraps_written_count: reported.scraps_written_count,
        scraps_received_count: reported.scraps_received_count,
        testimonials_written_count: reported.testimonials_written_count,
        blocked_by_count: reported.blocked_by_count,
        reports_count: reported.reports_count,
    })
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/report/{id}",
    description = "Detalhe de uma denúncia com o conteúdo denunciado e o histórico recente da conta responsável, para o moderador conferir sem sair da tela",
    params(("id" = Uuid, Path, description = "Id da denúncia")),
    responses(
        (status = 200, description = "Denúncia detalhada", body = ApiResponse<ReportDetail>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Não é administrador", body = ApiErrorResponse),
        (status = 404, description = "Denúncia não encontrada ou alvo já removido", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Detalha uma denúncia", skip_all)]
pub async fn detail(
    id: Path<Uuid>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let id = id.into_inner();
    let storage = storage.as_ref().as_ref();

    let report = sqlx::query!(
        r#"
        SELECT
            r.id,
            r.reason::TEXT AS "reason!",
            r.details,
            r.status::TEXT AS "status!",
            r.created_at,
            r.reviewed_at,
            r.reported_user_id,
            r.scrap_id,
            r.testimonial_id,
            r.community_id,
            r.topic_id,
            r.comment_id,
            r.poll_id,
            COALESCE(r.reported_user_id, s.author_id, t.author_id, ct.author_id,
                     cc.author_id, cp.author_id) AS accountable_id,
            reporter.user_id AS reporter_id,
            reporter.slug AS reporter_slug,
            reporter.name AS reporter_name,
            reporter.avatar_url AS reporter_avatar_url,
            reporter.status AS reporter_status,
            reviewer.user_id AS "reviewer_id?",
            reviewer.slug AS "reviewer_slug?",
            reviewer.name AS "reviewer_name?",
            reviewer.avatar_url AS "reviewer_avatar_url?",
            reviewer.status AS "reviewer_status?"
        FROM report r
        INNER JOIN user_profile reporter ON reporter.user_id = r.reporter_id
        LEFT JOIN user_profile reviewer ON reviewer.user_id = r.reviewed_by
        LEFT JOIN scrap s ON s.id = r.scrap_id
        LEFT JOIN testimonial t ON t.id = r.testimonial_id
        LEFT JOIN community_topic ct ON ct.id = r.topic_id
        LEFT JOIN community_topic_comment cc ON cc.id = r.comment_id
        LEFT JOIN community_poll cp ON cp.id = r.poll_id
        WHERE r.id = $1
        "#,
        id,
    )
    .fetch_optional(pool.get_ref())
    .await
    .map_err(db_err)?;

    let Some(report) = report else {
        return Ok(report_not_found());
    };

    let target_ids = ReportRow {
        reported_user_id: report.reported_user_id,
        scrap_id: report.scrap_id,
        testimonial_id: report.testimonial_id,
        community_id: report.community_id,
        topic_id: report.topic_id,
        comment_id: report.comment_id,
        poll_id: report.poll_id,
    };

    // O alvo só some quando a linha é apagada de verdade (comunidade excluída,
    // conta removida), e aí a denúncia perde o sentido.
    let Some(target) = load_target(pool.get_ref(), &target_ids).await? else {
        return Ok(report_not_found());
    };

    let (reported, recent_scraps, recent_testimonials, other_reports) = match report.accountable_id
    {
        None => (None, Vec::new(), Vec::new(), Vec::new()),
        Some(accountable_id) => {
            let reported = load_reported_user(pool.get_ref(), accountable_id, storage).await?;

            let recent_scraps = sqlx::query!(
                r#"
                    SELECT
                        s.id,
                        s.body,
                        s.created_at,
                        s.parent_id IS NOT NULL AS "is_reply!",
                        s.deleted_at IS NOT NULL AS "is_deleted!",
                        wall.slug AS wall_slug
                    FROM scrap s
                    INNER JOIN user_profile wall ON wall.user_id = s.profile_id
                    WHERE s.author_id = $1
                    ORDER BY s.id DESC
                    LIMIT $2
                    "#,
                accountable_id,
                RECENT_LIMIT,
            )
            .fetch_all(pool.get_ref())
            .await
            .map_err(db_err)?
            .into_iter()
            .map(|row| RecentScrap {
                id: row.id,
                body: row.body,
                created_at: row.created_at,
                wall_slug: row.wall_slug,
                is_reply: row.is_reply,
                is_deleted: row.is_deleted,
            })
            .collect();

            let recent_testimonials = sqlx::query!(
                r#"
                    SELECT
                        t.id,
                        t.body,
                        t.status::TEXT AS "status!",
                        t.created_at,
                        t.deleted_at IS NOT NULL AS "is_deleted!",
                        target.slug AS profile_slug
                    FROM testimonial t
                    INNER JOIN user_profile target ON target.user_id = t.profile_id
                    WHERE t.author_id = $1
                    ORDER BY t.id DESC
                    LIMIT $2
                    "#,
                accountable_id,
                RECENT_LIMIT,
            )
            .fetch_all(pool.get_ref())
            .await
            .map_err(db_err)?
            .into_iter()
            .map(|row| RecentTestimonial {
                id: row.id,
                body: row.body,
                status: row.status,
                created_at: row.created_at,
                profile_slug: row.profile_slug,
                is_deleted: row.is_deleted,
            })
            .collect();

            let other_reports = sqlx::query!(
                r#"
                    SELECT
                        r.id,
                        r.reason::TEXT AS "reason!",
                        r.status::TEXT AS "status!",
                        r.created_at,
                        reporter.slug AS reporter_slug
                    FROM report r
                    INNER JOIN user_profile reporter ON reporter.user_id = r.reporter_id
                    LEFT JOIN scrap s ON s.id = r.scrap_id
                    LEFT JOIN testimonial t ON t.id = r.testimonial_id
                    LEFT JOIN community_topic ct ON ct.id = r.topic_id
                    LEFT JOIN community_topic_comment cc ON cc.id = r.comment_id
                    LEFT JOIN community_poll cp ON cp.id = r.poll_id
                    WHERE r.id <> $2
                      AND COALESCE(r.reported_user_id, s.author_id, t.author_id, ct.author_id,
                                   cc.author_id, cp.author_id) = $1
                    ORDER BY r.id DESC
                    LIMIT $3
                    "#,
                accountable_id,
                id,
                RECENT_LIMIT,
            )
            .fetch_all(pool.get_ref())
            .await
            .map_err(db_err)?
            .into_iter()
            .map(|row| OtherReport {
                id: row.id,
                reason: row.reason,
                status: row.status,
                created_at: row.created_at,
                reporter_slug: row.reporter_slug,
            })
            .collect();

            (
                Some(reported),
                recent_scraps,
                recent_testimonials,
                other_reports,
            )
        }
    };

    Ok(HttpResponse::Ok().json(ApiResponse::new(ReportDetail {
        id: report.id,
        reason: report.reason,
        details: report.details,
        status: report.status,
        created_at: report.created_at,
        reviewed_at: report.reviewed_at,
        reviewed_by: report.reviewer_id.map(|id| ProfileSummary {
            id,
            slug: report.reviewer_slug.unwrap_or_default(),
            name: report.reviewer_name.unwrap_or_default(),
            avatar_url: public_url_of(storage, report.reviewer_avatar_url),
            status: report.reviewer_status,
        }),
        reporter: ProfileSummary {
            id: report.reporter_id,
            slug: report.reporter_slug,
            name: report.reporter_name,
            avatar_url: public_url_of(storage, report.reporter_avatar_url),
            status: report.reporter_status,
        },
        target,
        reported,
        recent_scraps,
        recent_testimonials,
        other_reports,
    })))
}

/// Os estados que o moderador pode gravar. `open` fica de fora: é o inicial e
/// não se volta para ele.
#[derive(Debug, Clone, Copy, sqlx::Type)]
#[sqlx(type_name = "report_status", rename_all = "snake_case")]
enum ReviewStatus {
    Reviewing,
    Resolved,
    Dismissed,
}

/// Grava a decisão do moderador. `reviewing` é o estado intermediário e não
/// carimba `reviewed_at`, que marca a conclusão.
async fn set_status(
    pool: &PgPool,
    id: Uuid,
    reviewer_id: Uuid,
    status: ReviewStatus,
) -> Result<bool, actix_web::Error> {
    // `reviewed_at` marca a conclusão, então só é carimbado quando a denúncia
    // sai da fila de análise.
    let concluded = !matches!(status, ReviewStatus::Reviewing);

    let updated = sqlx::query!(
        r#"
        UPDATE report
        SET status = $3,
            reviewed_by = $2,
            reviewed_at = CASE WHEN $4 THEN NOW() ELSE NULL END
        WHERE id = $1
        "#,
        id,
        reviewer_id,
        status as ReviewStatus,
        concluded,
    )
    .execute(pool)
    .await
    .map_err(db_err)?
    .rows_affected();

    Ok(updated > 0)
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/report/{id}/reviewing",
    description = "Assume a denúncia para análise, registrando quem está olhando",
    params(("id" = Uuid, Path, description = "Id da denúncia")),
    responses(
        (status = 204, description = "Denúncia em análise"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Não é administrador", body = ApiErrorResponse),
        (status = 404, description = "Denúncia não encontrada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Assume uma denúncia para análise", skip_all)]
pub async fn reviewing(
    id: Path<Uuid>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    if !set_status(
        pool.get_ref(),
        id.into_inner(),
        session.user_id,
        ReviewStatus::Reviewing,
    )
    .await?
    {
        return Ok(report_not_found());
    }

    Ok(HttpResponse::NoContent().finish())
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/report/{id}/resolve",
    description = "Encerra a denúncia como procedente, depois de tomada a providência",
    params(("id" = Uuid, Path, description = "Id da denúncia")),
    responses(
        (status = 204, description = "Denúncia resolvida"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Não é administrador", body = ApiErrorResponse),
        (status = 404, description = "Denúncia não encontrada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Resolve uma denúncia", skip_all)]
pub async fn resolve(
    id: Path<Uuid>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    if !set_status(
        pool.get_ref(),
        id.into_inner(),
        session.user_id,
        ReviewStatus::Resolved,
    )
    .await?
    {
        return Ok(report_not_found());
    }

    Ok(HttpResponse::NoContent().finish())
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/report/{id}/dismiss",
    description = "Encerra a denúncia como improcedente",
    params(("id" = Uuid, Path, description = "Id da denúncia")),
    responses(
        (status = 204, description = "Denúncia descartada"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Não é administrador", body = ApiErrorResponse),
        (status = 404, description = "Denúncia não encontrada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Descarta uma denúncia", skip_all)]
pub async fn dismiss(
    id: Path<Uuid>,
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    if !set_status(
        pool.get_ref(),
        id.into_inner(),
        session.user_id,
        ReviewStatus::Dismissed,
    )
    .await?
    {
        return Ok(report_not_found());
    }

    Ok(HttpResponse::NoContent().finish())
}
