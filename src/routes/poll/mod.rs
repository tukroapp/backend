use actix_web::{
    HttpResponse, Responder,
    web::{Data, Json, Path},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::HashSet;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

use std::sync::Arc;

use crate::{
    routes::{community::CommunityRole, profile::ProfileSummary},
    services::{
        auth::SessionData,
        storage::{StorageService, public_url_of},
    },
    utils::{ApiErrorResponse, ApiResponse, db_err, unauthorized, validate_trimmed_string},
};

#[derive(Debug, Deserialize, ToSchema, Validate)]
pub struct PollRequest {
    #[validate(
        length(
            min = 1,
            max = 300,
            message = "A pergunta deve ter entre 1 e 300 caracteres"
        ),
        custom(function = "validate_trimmed_string")
    )]
    pub question: String,
    /// Mínimo de opções que o voto precisa marcar. Sempre >= 1.
    pub min_choices: i16,
    /// Máximo de opções que o voto pode marcar. Sempre >= `min_choices` e nunca
    /// maior que a quantidade de opções.
    pub max_choices: i16,
    #[validate(length(
        min = 2,
        max = 10,
        message = "A enquete deve ter entre 2 e 10 opções"
    ))]
    pub options: Vec<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct VoteRequest {
    pub option_ids: Vec<Uuid>,
}

#[derive(Serialize, ToSchema)]
pub struct PollOptionResponse {
    pub id: Uuid,
    pub position: i16,
    pub text: String,
    /// `null` enquanto os resultados estão escondidos (antes de votar e antes de
    /// a enquete encerrar).
    pub votes: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct PollResponse {
    pub id: Uuid,
    pub community_slug: String,
    pub question: String,
    pub min_choices: i16,
    pub max_choices: i16,
    /// Encerrada de fato: fechada pela moderação ou já passou de `closes_at`. O
    /// front distingue os dois casos comparando `closes_at` com o horário atual.
    pub is_closed: bool,
    pub closes_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    /// Preenchido só quando o autor reescreveu a pergunta.
    pub edited_at: Option<DateTime<Utc>>,
    pub author: ProfileSummary,
    pub options: Vec<PollOptionResponse>,
    /// Votantes distintos. `null` enquanto os resultados estão escondidos.
    pub total_votes: Option<i64>,
    /// Ids das opções que o leitor marcou. Vazio quando ainda não votou.
    pub my_choices: Vec<Uuid>,
    pub has_voted: bool,
    pub results_visible: bool,
    pub can_vote: bool,
    pub can_edit: bool,
    /// As opções só podem mudar enquanto a enquete não recebeu nenhum voto.
    pub can_edit_options: bool,
    pub can_close: bool,
    pub can_delete: bool,
    pub can_moderate: bool,
}

/// Enquete resolvida pelo id junto da relação de quem está olhando com a
/// comunidade dela.
pub struct PollTarget {
    pub id: Uuid,
    pub community_slug: String,
    pub is_private: bool,
    pub author_id: Uuid,
    /// Coluna crua: fechamento manual antecipado pela moderação.
    pub closed_manually: bool,
    pub closes_at: DateTime<Utc>,
    pub min_choices: i16,
    pub max_choices: i16,
    pub total_votes: i64,
    pub option_count: i64,
    pub viewer: Option<Uuid>,
    pub role: Option<CommunityRole>,
    pub is_superuser: bool,
    pub viewer_voted: bool,
}

impl PollTarget {
    pub fn is_member(&self) -> bool {
        self.role.is_some()
    }

    pub fn is_author(&self) -> bool {
        self.viewer == Some(self.author_id)
    }

    pub fn can_moderate(&self) -> bool {
        self.is_superuser || matches!(self.role, Some(CommunityRole::Owner | CommunityRole::Admin))
    }

    pub fn can_view(&self) -> bool {
        !self.is_private || self.is_member() || self.is_superuser
    }

    pub fn effective_closed(&self) -> bool {
        self.closed_manually || Utc::now() >= self.closes_at
    }

    pub fn results_visible(&self) -> bool {
        self.viewer_voted || self.effective_closed()
    }

    pub fn can_edit(&self) -> bool {
        self.is_author() || self.can_moderate()
    }

    pub fn can_delete(&self) -> bool {
        self.is_author() || self.can_moderate()
    }
}

pub fn poll_not_found() -> HttpResponse {
    HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
        "poll",
        "not_found",
        Some("Enquete não encontrada."),
        vec![],
    ))
}

pub fn poll_moderator_required() -> HttpResponse {
    HttpResponse::Forbidden().json(ApiErrorResponse::from_single_validation_error(
        "poll",
        "moderator_required",
        Some("Apenas moderadores e o dono criam enquetes."),
        vec![],
    ))
}

fn poll_editor_required() -> HttpResponse {
    HttpResponse::Forbidden().json(ApiErrorResponse::from_single_validation_error(
        "poll",
        "editor_required",
        Some("Apenas o autor ou a moderação podem fazer isso."),
        vec![],
    ))
}

fn poll_member_required() -> HttpResponse {
    HttpResponse::Forbidden().json(ApiErrorResponse::from_single_validation_error(
        "poll",
        "member_required",
        Some("Entre na comunidade para votar."),
        vec![],
    ))
}

fn poll_closed() -> HttpResponse {
    HttpResponse::Forbidden().json(ApiErrorResponse::from_single_validation_error(
        "poll",
        "poll_closed",
        Some("A enquete já foi encerrada."),
        vec![],
    ))
}

fn poll_already_voted() -> HttpResponse {
    HttpResponse::Conflict().json(ApiErrorResponse::from_single_validation_error(
        "poll",
        "already_voted",
        Some("Você já votou nesta enquete e o voto é definitivo."),
        vec![],
    ))
}

fn poll_has_votes() -> HttpResponse {
    HttpResponse::BadRequest().json(ApiErrorResponse::from_single_validation_error(
        "options",
        "poll_has_votes",
        Some("A enquete já recebeu votos: as opções não podem mudar."),
        vec![],
    ))
}

/// Apara cada opção e recusa vazias, longas demais ou repetidas. A contagem já
/// foi checada pelo `validator` (2 a 10).
pub fn clean_options(options: &[String]) -> Result<Vec<String>, HttpResponse> {
    let mut cleaned = Vec::with_capacity(options.len());
    let mut seen = HashSet::new();

    for option in options {
        let trimmed = option.trim();
        if trimmed.is_empty() || trimmed.chars().count() > 120 {
            return Err(HttpResponse::BadRequest().json(
                ApiErrorResponse::from_single_validation_error(
                    "options",
                    "invalid_option_text",
                    Some("Cada opção deve ter entre 1 e 120 caracteres."),
                    vec![],
                ),
            ));
        }
        if !seen.insert(trimmed.to_owned()) {
            return Err(HttpResponse::BadRequest().json(
                ApiErrorResponse::from_single_validation_error(
                    "options",
                    "duplicate_option",
                    Some("As opções não podem se repetir."),
                    vec![],
                ),
            ));
        }
        cleaned.push(trimmed.to_owned());
    }

    Ok(cleaned)
}

/// Confere os limites de voto contra a quantidade de opções. O `CHECK` do banco
/// cobre `min >= 1` e `max >= min`, mas devolver 400 aqui dá uma mensagem melhor.
pub fn validate_choice_bounds(
    min_choices: i16,
    max_choices: i16,
    option_count: usize,
) -> Result<(), HttpResponse> {
    if min_choices < 1 || max_choices < min_choices || max_choices as usize > option_count {
        return Err(HttpResponse::BadRequest().json(
            ApiErrorResponse::from_single_validation_error(
                "max_choices",
                "invalid_choice_bounds",
                Some("Os limites de voto são inválidos para a quantidade de opções."),
                vec![],
            ),
        ));
    }
    Ok(())
}

pub async fn fetch_poll_target(
    pool: &PgPool,
    poll_id: Uuid,
    viewer: Option<Uuid>,
) -> Result<Option<PollTarget>, actix_web::Error> {
    let row = sqlx::query!(
        r#"
        SELECT
            p.id,
            p.author_id,
            p.is_closed AS closed_manually,
            p.closes_at,
            p.min_choices,
            p.max_choices,
            c.slug AS community_slug,
            c.is_private,
            cm.role AS "role?: CommunityRole",
            COALESCE((
                SELECT au.is_superuser FROM auth_user au WHERE au.id = $2::uuid
            ), FALSE) AS "is_superuser!",
            (
                SELECT COUNT(*) FROM community_poll_option o WHERE o.poll_id = p.id
            ) AS "option_count!",
            (
                SELECT COUNT(DISTINCT v.user_id) FROM community_poll_vote v WHERE v.poll_id = p.id
            ) AS "total_votes!",
            EXISTS (
                SELECT 1 FROM community_poll_vote v
                WHERE v.poll_id = p.id AND v.user_id = $2::uuid
            ) AS "viewer_voted!"
        FROM community_poll p
        INNER JOIN community c ON c.id = p.community_id
        LEFT JOIN community_member cm ON cm.community_id = c.id AND cm.user_id = $2::uuid
        WHERE p.id = $1 AND p.deleted_at IS NULL AND c.deleted_at IS NULL
        "#,
        poll_id,
        viewer,
    )
    .fetch_optional(pool)
    .await
    .map_err(db_err)?;

    Ok(row.map(|row| PollTarget {
        id: row.id,
        community_slug: row.community_slug,
        is_private: row.is_private,
        author_id: row.author_id,
        closed_manually: row.closed_manually,
        closes_at: row.closes_at,
        min_choices: row.min_choices,
        max_choices: row.max_choices,
        total_votes: row.total_votes,
        option_count: row.option_count,
        viewer,
        role: row.role,
        is_superuser: row.is_superuser,
        viewer_voted: row.viewer_voted,
    }))
}

/// Campos que decidem o que cada leitor pode ver e fazer com uma enquete. Sai da
/// `PollTarget` na rota de uma enquete e da `CommunityTarget` na listagem.
pub struct PollViewer {
    pub is_author: bool,
    pub can_moderate: bool,
    pub is_member: bool,
    pub effective_closed: bool,
    pub total_votes: i64,
    pub viewer_voted: bool,
}

/// Os dados fixos de uma enquete, do jeito que saem do banco antes de aplicar as
/// permissões e a regra de esconder resultados.
pub struct PollHead {
    pub id: Uuid,
    pub community_slug: String,
    pub question: String,
    pub min_choices: i16,
    pub max_choices: i16,
    pub closes_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub edited_at: Option<DateTime<Utc>>,
    pub author: ProfileSummary,
}

/// Uma opção crua vinda do banco, com a contagem e se o leitor a marcou.
pub struct RawOption {
    pub id: Uuid,
    pub position: i16,
    pub text: String,
    pub votes: i64,
    pub mine: bool,
}

/// Monta a resposta aplicando a regra de esconder resultados: enquanto o leitor
/// não votou e a enquete não encerrou, as contagens vão como `null`.
pub fn assemble_poll(head: PollHead, options: Vec<RawOption>, viewer: &PollViewer) -> PollResponse {
    let results_visible = viewer.viewer_voted || viewer.effective_closed;

    let my_choices = options
        .iter()
        .filter(|option| option.mine)
        .map(|option| option.id)
        .collect();

    let options = options
        .into_iter()
        .map(|option| PollOptionResponse {
            id: option.id,
            position: option.position,
            text: option.text,
            votes: results_visible.then_some(option.votes),
        })
        .collect();

    PollResponse {
        id: head.id,
        community_slug: head.community_slug,
        question: head.question,
        min_choices: head.min_choices,
        max_choices: head.max_choices,
        is_closed: viewer.effective_closed,
        closes_at: head.closes_at,
        created_at: head.created_at,
        edited_at: head.edited_at,
        author: head.author,
        options,
        total_votes: results_visible.then_some(viewer.total_votes),
        my_choices,
        has_voted: viewer.viewer_voted,
        results_visible,
        can_vote: viewer.is_member && !viewer.effective_closed && !viewer.viewer_voted,
        can_edit: viewer.is_author || viewer.can_moderate,
        can_edit_options: (viewer.is_author || viewer.can_moderate) && viewer.total_votes == 0,
        can_close: viewer.can_moderate && !viewer.effective_closed,
        can_delete: viewer.is_author || viewer.can_moderate,
        can_moderate: viewer.can_moderate,
    }
}

/// Busca as opções de uma enquete com a contagem de votos e se o leitor marcou
/// cada uma.
async fn fetch_options(
    pool: &PgPool,
    poll_id: Uuid,
    viewer: Option<Uuid>,
) -> Result<Vec<RawOption>, actix_web::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT
            o.id,
            o.position,
            o.text,
            COUNT(v.user_id) AS "votes!",
            COALESCE(BOOL_OR(v.user_id = $2::uuid), FALSE) AS "mine!"
        FROM community_poll_option o
        LEFT JOIN community_poll_vote v ON v.option_id = o.id
        WHERE o.poll_id = $1
        GROUP BY o.id, o.position, o.text
        ORDER BY o.position
        "#,
        poll_id,
        viewer,
    )
    .fetch_all(pool)
    .await
    .map_err(db_err)?;

    Ok(rows
        .into_iter()
        .map(|row| RawOption {
            id: row.id,
            position: row.position,
            text: row.text,
            votes: row.votes,
            mine: row.mine,
        })
        .collect())
}

/// Resposta completa de uma enquete, a partir do alvo já resolvido.
pub async fn build_poll_response(
    pool: &PgPool,
    storage: &dyn StorageService,
    target: &PollTarget,
) -> Result<PollResponse, actix_web::Error> {
    let head = sqlx::query!(
        r#"
        SELECT
            p.question,
            p.created_at,
            p.edited_at,
            up.user_id AS author_id,
            up.slug AS author_slug,
            up.name AS author_name,
            up.avatar_url AS author_avatar_url,
            up.status AS author_status
        FROM community_poll p
        INNER JOIN user_profile up ON up.user_id = p.author_id
        WHERE p.id = $1
        "#,
        target.id,
    )
    .fetch_one(pool)
    .await
    .map_err(db_err)?;

    let options = fetch_options(pool, target.id, target.viewer).await?;

    let viewer = PollViewer {
        is_author: target.is_author(),
        can_moderate: target.can_moderate(),
        is_member: target.is_member(),
        effective_closed: target.effective_closed(),
        total_votes: target.total_votes,
        viewer_voted: target.viewer_voted,
    };

    Ok(assemble_poll(
        PollHead {
            id: target.id,
            community_slug: target.community_slug.clone(),
            question: head.question,
            min_choices: target.min_choices,
            max_choices: target.max_choices,
            closes_at: target.closes_at,
            created_at: head.created_at,
            edited_at: head.edited_at,
            author: ProfileSummary {
                id: head.author_id,
                slug: head.author_slug,
                name: head.author_name,
                avatar_url: public_url_of(storage, head.author_avatar_url),
                status: head.author_status,
            },
        },
        options,
        &viewer,
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/poll/{id}",
    description = "Retorna uma enquete de comunidade. As contagens só aparecem depois que o leitor vota ou depois que a enquete encerra",
    params(("id" = Uuid, Path, description = "Id da enquete")),
    responses(
        (status = 200, description = "Enquete", body = ApiResponse<PollResponse>),
        (status = 404, description = "Enquete não encontrada ou comunidade privada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Retorna uma enquete de comunidade", skip_all)]
pub async fn get(
    id: Path<Uuid>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let viewer = session.map(|session| session.user_id);

    let Some(target) = fetch_poll_target(pool.get_ref(), id.into_inner(), viewer).await? else {
        return Ok(poll_not_found());
    };

    if !target.can_view() {
        return Ok(poll_not_found());
    }

    let poll = build_poll_response(pool.get_ref(), storage.as_ref().as_ref(), &target).await?;

    Ok(HttpResponse::Ok().json(ApiResponse::new(poll)))
}

#[utoipa::path(
    put,
    path = "/api/v1/poll/{id}",
    description = "Edita a enquete. A pergunta muda sempre; as opções e os limites de voto só enquanto não houver nenhum voto",
    params(("id" = Uuid, Path, description = "Id da enquete")),
    request_body = PollRequest,
    responses(
        (status = 200, description = "Enquete atualizada", body = ApiResponse<PollResponse>),
        (status = 400, description = "Dados inválidos ou a enquete já tem votos", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Apenas o autor ou a moderação, e a enquete não pode estar encerrada", body = ApiErrorResponse),
        (status = 404, description = "Enquete não encontrada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Edita uma enquete de comunidade", skip_all)]
pub async fn put(
    id: Path<Uuid>,
    payload: Json<PollRequest>,
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
        fetch_poll_target(pool.get_ref(), id.into_inner(), Some(session.user_id)).await?
    else {
        return Ok(poll_not_found());
    };

    if !target.can_view() {
        return Ok(poll_not_found());
    }

    if !target.can_edit() {
        return Ok(poll_editor_required());
    }

    if target.effective_closed() {
        return Ok(poll_closed());
    }

    let options = match clean_options(&payload.options) {
        Ok(options) => options,
        Err(response) => return Ok(response),
    };

    if let Err(response) =
        validate_choice_bounds(payload.min_choices, payload.max_choices, options.len())
    {
        return Ok(response);
    }

    if target.total_votes > 0 {
        let current: Vec<String> = sqlx::query_scalar!(
            r#"SELECT text FROM community_poll_option WHERE poll_id = $1 ORDER BY position"#,
            target.id,
        )
        .fetch_all(pool.get_ref())
        .await
        .map_err(db_err)?;

        if payload.min_choices != target.min_choices
            || payload.max_choices != target.max_choices
            || options != current
        {
            return Ok(poll_has_votes());
        }

        sqlx::query!(
            r#"UPDATE community_poll SET question = $1, edited_at = NOW() WHERE id = $2"#,
            payload.question.trim(),
            target.id,
        )
        .execute(pool.get_ref())
        .await
        .map_err(db_err)?;
    } else {
        let mut tx = pool.begin().await.map_err(db_err)?;

        // Trava a enquete e reconfere a contagem dentro da transação: se um voto
        // entrou entre o `fetch_poll_target` e aqui, apagar as opções levaria
        // esse voto junto pelo CASCADE.
        sqlx::query_scalar!(
            r#"SELECT id FROM community_poll WHERE id = $1 FOR UPDATE"#,
            target.id,
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(db_err)?;

        let votes: i64 = sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "count!" FROM community_poll_vote WHERE poll_id = $1"#,
            target.id,
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(db_err)?;

        if votes > 0 {
            return Ok(poll_has_votes());
        }

        sqlx::query!(
            r#"
            UPDATE community_poll
            SET question = $1, min_choices = $2, max_choices = $3, edited_at = NOW()
            WHERE id = $4
            "#,
            payload.question.trim(),
            payload.min_choices,
            payload.max_choices,
            target.id,
        )
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        sqlx::query!(
            r#"DELETE FROM community_poll_option WHERE poll_id = $1"#,
            target.id,
        )
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        sqlx::query!(
            r#"
            INSERT INTO community_poll_option (poll_id, position, text)
            SELECT $1, (ordinality - 1)::smallint, opt
            FROM UNNEST($2::text[]) WITH ORDINALITY AS t(opt, ordinality)
            "#,
            target.id,
            &options,
        )
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        tx.commit().await.map_err(db_err)?;
    }

    let target = fetch_poll_target(pool.get_ref(), target.id, Some(session.user_id))
        .await?
        .ok_or_else(|| db_err(sqlx::Error::RowNotFound))?;

    let poll = build_poll_response(pool.get_ref(), storage.as_ref().as_ref(), &target).await?;

    Ok(HttpResponse::Ok().json(ApiResponse::new(poll)))
}

#[utoipa::path(
    delete,
    path = "/api/v1/poll/{id}",
    description = "Exclui a enquete. O conteúdo continua no banco como prova para a moderação",
    params(("id" = Uuid, Path, description = "Id da enquete")),
    responses(
        (status = 204, description = "Enquete excluída"),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Apenas o autor, a moderação da comunidade ou um administrador", body = ApiErrorResponse),
        (status = 404, description = "Enquete não encontrada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Exclui uma enquete de comunidade", skip_all)]
pub async fn delete(
    id: Path<Uuid>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    let Some(target) =
        fetch_poll_target(pool.get_ref(), id.into_inner(), Some(session.user_id)).await?
    else {
        return Ok(poll_not_found());
    };

    if !target.can_view() {
        return Ok(poll_not_found());
    }

    if !target.can_delete() {
        return Ok(poll_editor_required());
    }

    sqlx::query!(
        r#"UPDATE community_poll SET deleted_at = NOW() WHERE id = $1"#,
        target.id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?;

    Ok(HttpResponse::NoContent().finish())
}

#[utoipa::path(
    post,
    path = "/api/v1/poll/{id}/close",
    description = "Encerra a enquete antes das 24h. Depois disso ninguém mais vota e os resultados ficam públicos",
    params(("id" = Uuid, Path, description = "Id da enquete")),
    responses(
        (status = 200, description = "Enquete encerrada", body = ApiResponse<PollResponse>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Apenas a moderação, e a enquete não pode já estar encerrada", body = ApiErrorResponse),
        (status = 404, description = "Enquete não encontrada", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Encerra uma enquete de comunidade", skip_all)]
pub async fn close(
    id: Path<Uuid>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    let Some(target) =
        fetch_poll_target(pool.get_ref(), id.into_inner(), Some(session.user_id)).await?
    else {
        return Ok(poll_not_found());
    };

    if !target.can_view() {
        return Ok(poll_not_found());
    }

    if !target.can_moderate() {
        return Ok(poll_moderator_required());
    }

    if target.effective_closed() {
        return Ok(poll_closed());
    }

    sqlx::query!(
        r#"UPDATE community_poll SET is_closed = TRUE WHERE id = $1"#,
        target.id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?;

    let target = fetch_poll_target(pool.get_ref(), target.id, Some(session.user_id))
        .await?
        .ok_or_else(|| db_err(sqlx::Error::RowNotFound))?;

    let poll = build_poll_response(pool.get_ref(), storage.as_ref().as_ref(), &target).await?;

    Ok(HttpResponse::Ok().json(ApiResponse::new(poll)))
}

#[utoipa::path(
    post,
    path = "/api/v1/poll/{id}/vote",
    description = "Registra o voto do membro. Um voto por pessoa, definitivo, marcando entre min_choices e max_choices opções",
    params(("id" = Uuid, Path, description = "Id da enquete")),
    request_body = VoteRequest,
    responses(
        (status = 200, description = "Voto registrado", body = ApiResponse<PollResponse>),
        (status = 400, description = "Quantidade de opções fora do intervalo ou opção inválida", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 403, description = "Apenas membros votam, e a enquete não pode estar encerrada", body = ApiErrorResponse),
        (status = 404, description = "Enquete não encontrada", body = ApiErrorResponse),
        (status = 409, description = "O membro já votou nesta enquete", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Vota em uma enquete de comunidade", skip_all)]
pub async fn vote(
    id: Path<Uuid>,
    payload: Json<VoteRequest>,
    session: Option<SessionData>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let Some(session) = session else {
        return Ok(unauthorized());
    };

    let Some(target) =
        fetch_poll_target(pool.get_ref(), id.into_inner(), Some(session.user_id)).await?
    else {
        return Ok(poll_not_found());
    };

    if !target.can_view() {
        return Ok(poll_not_found());
    }

    if !target.is_member() {
        return Ok(poll_member_required());
    }

    if target.effective_closed() {
        return Ok(poll_closed());
    }

    if target.viewer_voted {
        return Ok(poll_already_voted());
    }

    let unique: Vec<Uuid> = {
        let mut seen = HashSet::new();
        payload
            .option_ids
            .iter()
            .copied()
            .filter(|option_id| seen.insert(*option_id))
            .collect()
    };

    if unique.len() != payload.option_ids.len() {
        return Ok(
            HttpResponse::BadRequest().json(ApiErrorResponse::from_single_validation_error(
                "option_ids",
                "duplicate_option",
                Some("O voto não pode repetir a mesma opção."),
                vec![],
            )),
        );
    }

    // `min_choices` e `max_choices` são sempre >= 1 (CHECK do banco), então o
    // cast para `usize` é seguro. Comparar como `i16` estouraria com uma lista
    // enorme de ids.
    let chosen = unique.len();
    if chosen < target.min_choices as usize || chosen > target.max_choices as usize {
        let message = format!(
            "Escolha entre {} e {} opções.",
            target.min_choices, target.max_choices
        );
        return Ok(
            HttpResponse::BadRequest().json(ApiErrorResponse::from_single_validation_error(
                "option_ids",
                "invalid_choice_count",
                Some(&message),
                vec![],
            )),
        );
    }

    let matched = sqlx::query_scalar!(
        r#"
        SELECT COUNT(*) AS "count!"
        FROM community_poll_option
        WHERE poll_id = $1 AND id = ANY($2)
        "#,
        target.id,
        &unique,
    )
    .fetch_one(pool.get_ref())
    .await
    .map_err(db_err)?;

    if matched != unique.len() as i64 {
        return Ok(
            HttpResponse::BadRequest().json(ApiErrorResponse::from_single_validation_error(
                "option_ids",
                "invalid_option",
                Some("Uma das opções não pertence a esta enquete."),
                vec![],
            )),
        );
    }

    let mut tx = pool.begin().await.map_err(db_err)?;

    // Trava a enquete e reconfere dentro da transação: o voto é definitivo, então
    // dois POST simultâneos do mesmo usuário não podem os dois passar o INSERT.
    sqlx::query_scalar!(
        r#"SELECT id FROM community_poll WHERE id = $1 FOR UPDATE"#,
        target.id,
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(db_err)?;

    let already_voted = sqlx::query_scalar!(
        r#"
        SELECT EXISTS (
            SELECT 1 FROM community_poll_vote WHERE poll_id = $1 AND user_id = $2
        ) AS "e!"
        "#,
        target.id,
        session.user_id,
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(db_err)?;

    if already_voted {
        return Ok(poll_already_voted());
    }

    sqlx::query!(
        r#"
        INSERT INTO community_poll_vote (poll_id, user_id, option_id)
        SELECT $1, $2, UNNEST($3::uuid[])
        "#,
        target.id,
        session.user_id,
        &unique,
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;

    tx.commit().await.map_err(db_err)?;

    let target = fetch_poll_target(pool.get_ref(), target.id, Some(session.user_id))
        .await?
        .ok_or_else(|| db_err(sqlx::Error::RowNotFound))?;

    let poll = build_poll_response(pool.get_ref(), storage.as_ref().as_ref(), &target).await?;

    Ok(HttpResponse::Ok().json(ApiResponse::new(poll)))
}
