use std::{io::Read, sync::Arc};

use actix_multipart::form::{MultipartForm, tempfile::TempFile};
use actix_web::{
    HttpResponse, Responder,
    web::{Data, Json},
};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

use crate::{
    routes::profile::ProfileSummary,
    services::{
        activity::{NewActivity, record_activity},
        auth::SessionData,
        storage::{AVATAR_PREFIX, StorageService, public_url_of, sniff_image},
        visit::recent_visitors,
    },
    startup::RedisPool,
    utils::{ApiErrorResponse, ApiResponse, db_err, e500, invalid_field},
};
use chrono::{DateTime, Utc};

/// Espelha o enum `visibility` do banco.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[sqlx(type_name = "visibility", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Everyone,
    Friends,
    Nobody,
}

#[derive(Serialize, ToSchema)]
pub struct MeResponse {
    pub id: Uuid,
    pub email: String,
    pub is_email_verified: bool,
    pub is_superuser: bool,
    pub is_google: bool,
    pub slug: String,
    pub name: String,
    pub avatar_url: Option<String>,
    pub status: Option<String>,
    pub bio: Option<String>,
    pub city: Option<String>,
    pub birthdate: Option<NaiveDate>,
    /// Quem vê o perfil completo, incluindo a lista de amigos.
    pub profile_visibility: Visibility,
    /// Quem lê os scraps do mural.
    pub scrap_read: Visibility,
    /// Quem pode escrever scraps no mural.
    pub scrap_write: Visibility,
    /// Quem pode enviar depoimentos.
    pub testimonial_write: Visibility,
    /// Quem pode iniciar uma conversa privada com você.
    pub message_receive: Visibility,
    /// Toggle do histórico de visitas. `true` mostra quem visitou o seu perfil e
    /// registra as suas visitas nos perfis alheios; `false` desliga os dois lados.
    pub show_profile_visits: bool,
    /// Notificações não lidas, para o badge do sino. Ignora quem está bloqueado,
    /// igual à listagem.
    pub unread_notifications_count: i64,
    /// Mensagens não lidas em todas as conversas não escondidas, para o badge.
    pub unread_messages_count: i64,
}

async fn fetch_me(
    pool: &PgPool,
    storage: &dyn StorageService,
    user_id: &Uuid,
) -> Result<MeResponse, actix_web::Error> {
    let mut me = sqlx::query_as!(
        MeResponse,
        r#"
        SELECT
            au.id,
            au.email AS "email!: String",
            au.is_email_verified,
            au.is_superuser,
            (au.google_id IS NOT NULL) AS "is_google!",
            up.slug,
            up.name,
            up.avatar_url,
            up.status,
            up.bio,
            up.city,
            up.birthdate,
            up.profile_visibility AS "profile_visibility: Visibility",
            up.scrap_read AS "scrap_read: Visibility",
            up.scrap_write AS "scrap_write: Visibility",
            up.testimonial_write AS "testimonial_write: Visibility",
            up.message_receive AS "message_receive: Visibility",
            up.show_profile_visits,
            (
                SELECT COUNT(*) FROM notification n
                LEFT JOIN scrap s ON s.id = n.scrap_id
                LEFT JOIN testimonial t ON t.id = n.testimonial_id
                WHERE n.user_id = au.id
                  AND n.read_at IS NULL
                  AND s.deleted_at IS NULL
                  AND t.deleted_at IS NULL
                  AND NOT EXISTS (
                      SELECT 1 FROM block b
                      WHERE (b.blocker_id = n.user_id AND b.blocked_id = n.actor_id)
                         OR (b.blocker_id = n.actor_id AND b.blocked_id = n.user_id)
                  )
            ) AS "unread_notifications_count!",
            (
                SELECT COUNT(*) FROM message m
                INNER JOIN conversation_participant cp
                    ON cp.conversation_id = m.conversation_id AND cp.user_id = au.id
                WHERE m.sender_id <> au.id
                  AND m.created_at > cp.last_read_at
                  AND (cp.hidden_at IS NULL OR m.created_at > cp.hidden_at)
            ) AS "unread_messages_count!"
        FROM auth_user au
        INNER JOIN user_profile up ON up.user_id = au.id
        WHERE au.id = $1
        "#,
        user_id
    )
    .fetch_one(pool)
    .await
    .map_err(db_err)?;

    me.avatar_url = public_url_of(storage, me.avatar_url);

    Ok(me)
}

#[utoipa::path(
    get,
    path = "/api/v1/me",
    description = "Retorna os dados de autenticação e perfil do usuário autenticado",
    responses(
        (status = 200, description = "Dados do usuário", body = ApiResponse<MeResponse>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Retorna os dados do usuário autenticado", skip_all)]
pub async fn get(
    session: SessionData,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let me = fetch_me(pool.get_ref(), storage.as_ref().as_ref(), &session.user_id).await?;

    Ok(HttpResponse::Ok().json(ApiResponse::new(me)))
}

#[utoipa::path(
    put,
    path = "/api/v1/me",
    description = "Atualiza os dados de perfil do usuário autenticado. O slug é imutável e não pode ser alterado aqui",
    request_body = UpdateProfileRequest,
    responses(
        (status = 200, description = "Perfil atualizado", body = ApiResponse<MeResponse>),
        (status = 400, description = "Dados inválidos", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Atualiza o perfil do usuário autenticado", skip_all)]
pub async fn put(
    session: SessionData,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
    payload: Json<UpdateProfileRequest>,
) -> Result<impl Responder, actix_web::Error> {
    if let Err(e) = payload.validate() {
        return Ok(HttpResponse::BadRequest().json(ApiErrorResponse::from(e)));
    }

    let payload = payload.into_inner();

    let old_status: Option<String> = sqlx::query_scalar!(
        r#"SELECT status FROM user_profile WHERE user_id = $1"#,
        session.user_id,
    )
    .fetch_one(pool.get_ref())
    .await
    .map_err(db_err)?;

    sqlx::query!(
        r#"
        UPDATE user_profile
        SET name = $1,
            status = $2,
            bio = $3,
            city = $4,
            birthdate = $5,
            profile_visibility = COALESCE($6, profile_visibility),
            scrap_read = COALESCE($7, scrap_read),
            scrap_write = COALESCE($8, scrap_write),
            testimonial_write = COALESCE($9, testimonial_write),
            message_receive = COALESCE($10, message_receive),
            show_profile_visits = COALESCE($11, show_profile_visits)
        WHERE user_id = $12
        "#,
        payload.name,
        payload.status,
        payload.bio,
        payload.city,
        payload.birthdate,
        payload.profile_visibility as Option<Visibility>,
        payload.scrap_read as Option<Visibility>,
        payload.scrap_write as Option<Visibility>,
        payload.testimonial_write as Option<Visibility>,
        payload.message_receive as Option<Visibility>,
        payload.show_profile_visits,
        session.user_id,
    )
    .execute(pool.get_ref())
    .await
    .map_err(db_err)?;

    // "Atualizou o recado" só entra no feed quando o texto muda de verdade.
    let new_status = payload
        .status
        .as_deref()
        .map(str::trim)
        .filter(|status| !status.is_empty());

    if let Some(new_status) = new_status
        && old_status.as_deref().map(str::trim) != Some(new_status)
    {
        record_activity(
            pool.get_ref(),
            NewActivity::status_changed(session.user_id, new_status.to_owned()),
        )
        .await
        .map_err(db_err)?;
    }

    let me = fetch_me(pool.get_ref(), storage.as_ref().as_ref(), &session.user_id).await?;

    Ok(HttpResponse::Ok().json(ApiResponse::new(me)))
}

#[derive(Deserialize, Validate, ToSchema)]
pub struct UpdateProfileRequest {
    #[validate(length(min = 1, max = 80, message = "O nome deve ter entre 1 e 80 caracteres"))]
    #[schema(example = "Fulano de Tal")]
    pub name: String,

    #[schema(example = "Disponível")]
    pub status: Option<String>,

    #[schema(example = "Desenvolvedor e entusiasta de café")]
    pub bio: Option<String>,

    #[schema(example = "São Paulo")]
    pub city: Option<String>,

    #[schema(example = "1990-05-20")]
    pub birthdate: Option<NaiveDate>,

    /// Campos de privacidade: diferente dos demais, quando omitidos mantêm o
    /// valor atual, para que salvar o formulário de perfil não reabra o perfil
    /// sem querer.
    pub profile_visibility: Option<Visibility>,
    pub scrap_read: Option<Visibility>,
    pub scrap_write: Option<Visibility>,
    pub testimonial_write: Option<Visibility>,
    pub message_receive: Option<Visibility>,
    /// Liga/desliga o histórico de visitas dos dois lados. Omitido mantém o atual.
    pub show_profile_visits: Option<bool>,
}

#[derive(Debug, MultipartForm, ToSchema)]
pub struct AvatarUpload {
    /// Imagem JPEG, PNG, WebP ou GIF, de no máximo 2 MB.
    #[multipart(limit = "2 MiB")]
    #[schema(value_type = String, format = Binary)]
    pub file: TempFile,
}

/// Grava a nova URL, devolvendo a anterior para que o objeto antigo seja removido.
async fn replace_avatar(
    pool: &PgPool,
    user_id: &Uuid,
    key: Option<String>,
) -> Result<Option<String>, actix_web::Error> {
    let row = sqlx::query!(
        r#"
        UPDATE user_profile
        SET avatar_url = $1
        WHERE user_id = $2
        RETURNING (SELECT avatar_url FROM user_profile WHERE user_id = $2) AS previous
        "#,
        key,
        user_id,
    )
    .fetch_one(pool)
    .await
    .map_err(db_err)?;

    Ok(row.previous)
}

#[utoipa::path(
    post,
    path = "/api/v1/me/avatar",
    description = "Envia a foto de perfil do usuário autenticado e remove a anterior",
    request_body(content = AvatarUpload, content_type = "multipart/form-data"),
    responses(
        (status = 200, description = "Avatar atualizado", body = ApiResponse<MeResponse>),
        (status = 400, description = "Arquivo vazio ou formato não suportado", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
        (status = 413, description = "Arquivo maior que 2 MB", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Atualiza o avatar do usuário autenticado", skip_all)]
pub async fn post_avatar(
    session: SessionData,
    MultipartForm(form): MultipartForm<AvatarUpload>,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
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
        .upload(AVATAR_PREFIX, &session.user_id, &data, image)
        .await
        .map_err(e500)?;

    let previous = match replace_avatar(pool.get_ref(), &session.user_id, Some(key.clone())).await {
        Ok(previous) => previous,
        Err(e) => {
            // Não deixa objeto órfão no bucket quando o banco recusa.
            let _ = storage.delete(&key).await;
            return Err(e);
        }
    };

    if let Some(previous) = previous
        && let Err(error) = storage.delete(&previous).await
    {
        tracing::error!(%error, "falha ao remover o avatar anterior");
    }

    let me = fetch_me(pool.get_ref(), storage.as_ref().as_ref(), &session.user_id).await?;

    Ok(HttpResponse::Ok().json(ApiResponse::new(me)))
}

#[utoipa::path(
    delete,
    path = "/api/v1/me/avatar",
    description = "Remove a foto de perfil do usuário autenticado",
    responses(
        (status = 200, description = "Avatar removido", body = ApiResponse<MeResponse>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Remove o avatar do usuário autenticado", skip_all)]
pub async fn delete_avatar(
    session: SessionData,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let previous = replace_avatar(pool.get_ref(), &session.user_id, None).await?;

    if let Some(previous) = previous
        && let Err(error) = storage.delete(&previous).await
    {
        tracing::error!(%error, "falha ao remover o avatar");
    }

    let me = fetch_me(pool.get_ref(), storage.as_ref().as_ref(), &session.user_id).await?;

    Ok(HttpResponse::Ok().json(ApiResponse::new(me)))
}

#[derive(Serialize, ToSchema)]
pub struct ProfileVisitor {
    pub profile: ProfileSummary,
    pub visited_at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
pub struct ProfileVisitorsResponse {
    /// `false` quando o usuário desligou o histórico de visitas. Nesse caso
    /// `count_30d` é 0 e `recent` vem vazio.
    pub enabled: bool,
    /// Visitantes distintos nos últimos 30 dias.
    pub count_30d: i64,
    /// Últimos visitantes (7 dias), do mais recente para o mais antigo. Visitante
    /// de conta inativa ou bloqueada não aparece, então a lista pode ser menor
    /// que a contagem sugere.
    pub recent: Vec<ProfileVisitor>,
}

#[utoipa::path(
    get,
    path = "/api/v1/me/visitor",
    description = "Visitantes do próprio perfil: contagem distinta de 30 dias e a lista dos últimos 7 dias. Só o dono vê",
    responses(
        (status = 200, description = "Visitantes do perfil", body = ApiResponse<ProfileVisitorsResponse>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Lista os visitantes do próprio perfil", skip_all)]
pub async fn visitor(
    session: SessionData,
    pool: Data<PgPool>,
    redis: Data<RedisPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    // Tit-for-tat: só vê quem entrou no perfil quem também deixa as próprias
    // visitas visíveis.
    let enabled = sqlx::query_scalar!(
        r#"SELECT show_profile_visits FROM user_profile WHERE user_id = $1"#,
        session.user_id,
    )
    .fetch_one(pool.get_ref())
    .await
    .map_err(db_err)?;

    if !enabled {
        return Ok(HttpResponse::Ok().json(ApiResponse::new(ProfileVisitorsResponse {
            enabled: false,
            count_30d: 0,
            recent: Vec::new(),
        })));
    }

    let (count_30d, entries) = recent_visitors(&redis, session.user_id).await;

    let ids: Vec<Uuid> = entries.iter().map(|entry| entry.visitor_id).collect();

    let rows = sqlx::query!(
        r#"
        SELECT up.user_id AS id, up.slug, up.name, up.avatar_url, up.status
        FROM user_profile up
        INNER JOIN auth_user au ON au.id = up.user_id AND au.status = 'active'
        WHERE up.user_id = ANY($1)
          AND NOT EXISTS (
              SELECT 1 FROM block b
              WHERE (b.blocker_id = up.user_id AND b.blocked_id = $2)
                 OR (b.blocker_id = $2 AND b.blocked_id = up.user_id)
          )
        "#,
        &ids,
        session.user_id,
    )
    .fetch_all(pool.get_ref())
    .await
    .map_err(db_err)?;

    let storage = storage.as_ref().as_ref();
    let mut summaries: std::collections::HashMap<Uuid, ProfileSummary> = rows
        .into_iter()
        .map(|row| {
            (
                row.id,
                ProfileSummary {
                    id: row.id,
                    slug: row.slug,
                    name: row.name,
                    avatar_url: public_url_of(storage, row.avatar_url),
                    status: row.status,
                },
            )
        })
        .collect();

    // A ordem vem do Redis (mais recente primeiro); o `HashMap` só hidrata.
    let recent = entries
        .into_iter()
        .filter_map(|entry| {
            summaries
                .remove(&entry.visitor_id)
                .map(|profile| ProfileVisitor {
                    profile,
                    visited_at: entry.visited_at,
                })
        })
        .collect();

    Ok(HttpResponse::Ok().json(ApiResponse::new(ProfileVisitorsResponse {
        enabled: true,
        count_30d,
        recent,
    })))
}

#[derive(Serialize, ToSchema)]
pub struct UnreadResponse {
    pub notifications: i64,
    pub messages: i64,
    /// Atividades no feed mais novas que o `feed_seen_at` do usuário.
    pub feed: i64,
}

#[utoipa::path(
    get,
    path = "/api/v1/me/unread",
    description = "Contadores para os badges. Endpoint leve, feito para polling a cada 30s",
    responses(
        (status = 200, description = "Contadores de não lidas", body = ApiResponse<UnreadResponse>),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Contadores de não lidas", skip_all)]
pub async fn unread(
    session: SessionData,
    pool: Data<PgPool>,
) -> Result<impl Responder, actix_web::Error> {
    let row = sqlx::query!(
        r#"
        SELECT
            (
                SELECT COUNT(*) FROM notification n
                LEFT JOIN scrap s ON s.id = n.scrap_id
                LEFT JOIN testimonial t ON t.id = n.testimonial_id
                WHERE n.user_id = $1
                  AND n.read_at IS NULL
                  AND s.deleted_at IS NULL
                  AND t.deleted_at IS NULL
                  AND NOT EXISTS (
                      SELECT 1 FROM block b
                      WHERE (b.blocker_id = n.user_id AND b.blocked_id = n.actor_id)
                         OR (b.blocker_id = n.actor_id AND b.blocked_id = n.user_id)
                  )
            ) AS "notifications!",
            (
                SELECT COUNT(*) FROM message m
                INNER JOIN conversation_participant cp
                    ON cp.conversation_id = m.conversation_id AND cp.user_id = $1
                WHERE m.sender_id <> $1
                  AND m.created_at > cp.last_read_at
                  AND (cp.hidden_at IS NULL OR m.created_at > cp.hidden_at)
            ) AS "messages!",
            (
                SELECT COUNT(*) FROM activity a
                INNER JOIN auth_user au ON au.id = a.actor_id AND au.status = 'active'
                INNER JOIN user_profile ap ON ap.user_id = a.actor_id
                WHERE a.actor_id <> $1
                  AND a.created_at > (SELECT feed_seen_at FROM user_profile WHERE user_id = $1)
                  AND (
                      (EXISTS (SELECT 1 FROM friend f WHERE f.user_id = $1 AND f.friend_id = a.actor_id)
                          AND ap.profile_visibility <> 'nobody')
                      OR (EXISTS (SELECT 1 FROM fan WHERE fan.fan_id = $1 AND fan.idol_id = a.actor_id)
                          AND ap.profile_visibility = 'everyone')
                  )
            ) AS "feed!"
        "#,
        session.user_id,
    )
    .fetch_one(pool.get_ref())
    .await
    .map_err(db_err)?;

    Ok(HttpResponse::Ok().json(ApiResponse::new(UnreadResponse {
        notifications: row.notifications,
        messages: row.messages,
        feed: row.feed,
    })))
}
