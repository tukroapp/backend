use std::{io::Read, sync::Arc};

use actix_multipart::form::{MultipartForm, tempfile::TempFile};
use actix_web::{
    HttpResponse, Responder,
    web::{Data, Json, Query},
};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

use crate::{
    constants::DEFAULT_TIME_ZONE,
    routes::profile::ProfileSummary,
    services::{
        activity::{NewActivity, record_activity},
        auth::SessionData,
        storage::{AVATAR_PREFIX, StorageService, public_url_of, sniff_image},
        visit::recent_visitors,
    },
    startup::RedisPool,
    utils::{ApiErrorResponse, ApiResponse, db_err, e500, invalid_field, validate_list_items},
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

/// Espelha o enum `relationship_status` do banco.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[sqlx(type_name = "relationship_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum RelationshipStatus {
    Single,
    Dating,
    Engaged,
    Married,
    StableUnion,
    OpenRelationship,
    Complicated,
    Separated,
    Widowed,
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
    /// Perfil detalhado. Para quem olha de fora esses campos vêm dentro de
    /// `details`, no `GET /profile/{slug}`, e somem quando a privacidade fecha.
    pub relationship_status: Option<RelationshipStatus>,
    pub country: Option<String>,
    pub state: Option<String>,
    pub languages: Vec<String>,
    pub interests: Vec<String>,
    pub who_i_want_to_meet: Option<String>,
    pub occupation: Option<String>,
    pub company: Option<String>,
    pub education: Option<String>,
    pub music: Vec<String>,
    pub movies: Vec<String>,
    pub books: Vec<String>,
    pub tv_shows: Vec<String>,
    pub foods: Vec<String>,
    pub sports: Vec<String>,
    pub pets: Option<String>,
    pub fashion_style: Option<String>,
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
            up.relationship_status AS "relationship_status: RelationshipStatus",
            up.country,
            up.state,
            up.languages,
            up.interests,
            up.who_i_want_to_meet,
            up.occupation,
            up.company,
            up.education,
            up.music,
            up.movies,
            up.books,
            up.tv_shows,
            up.foods,
            up.sports,
            up.pets,
            up.fashion_style,
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

    let languages = clean_list(payload.languages);
    let interests = clean_list(payload.interests);
    let music = clean_list(payload.music);
    let movies = clean_list(payload.movies);
    let books = clean_list(payload.books);
    let tv_shows = clean_list(payload.tv_shows);
    let foods = clean_list(payload.foods);
    let sports = clean_list(payload.sports);

    sqlx::query!(
        r#"
        UPDATE user_profile
        SET name = $1,
            status = $2,
            bio = $3,
            city = $4,
            birthdate = $5,
            relationship_status = $6,
            country = $7,
            state = $8,
            languages = $9,
            interests = $10,
            who_i_want_to_meet = $11,
            occupation = $12,
            company = $13,
            education = $14,
            music = $15,
            movies = $16,
            books = $17,
            tv_shows = $18,
            foods = $19,
            sports = $20,
            pets = $21,
            fashion_style = $22,
            profile_visibility = COALESCE($23, profile_visibility),
            scrap_read = COALESCE($24, scrap_read),
            scrap_write = COALESCE($25, scrap_write),
            testimonial_write = COALESCE($26, testimonial_write),
            message_receive = COALESCE($27, message_receive),
            show_profile_visits = COALESCE($28, show_profile_visits)
        WHERE user_id = $29
        "#,
        payload.name,
        payload.status,
        payload.bio,
        payload.city,
        payload.birthdate,
        payload.relationship_status as Option<RelationshipStatus>,
        payload.country,
        payload.state,
        &languages,
        &interests,
        payload.who_i_want_to_meet,
        payload.occupation,
        payload.company,
        payload.education,
        &music,
        &movies,
        &books,
        &tv_shows,
        &foods,
        &sports,
        payload.pets,
        payload.fashion_style,
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

/// Tira o espaço das pontas e descarta item vazio: o formulário manda linha em
/// branco quando a pessoa apaga um item da lista.
fn clean_list(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect()
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

    pub relationship_status: Option<RelationshipStatus>,

    #[validate(length(max = 80))]
    #[schema(example = "Brasil")]
    pub country: Option<String>,

    #[validate(length(max = 80))]
    #[schema(example = "SP")]
    pub state: Option<String>,

    /// Listas do perfil detalhado. Item em branco é descartado no servidor, e
    /// omitir a lista limpa a lista, igual aos campos de texto.
    #[validate(length(max = 20), custom(function = "validate_list_items"))]
    #[serde(default)]
    #[schema(example = json!(["Português", "Inglês"]))]
    pub languages: Vec<String>,

    #[validate(length(max = 30), custom(function = "validate_list_items"))]
    #[serde(default)]
    #[schema(example = json!(["Rock progressivo", "Fliperama"]))]
    pub interests: Vec<String>,

    #[validate(length(max = 1000))]
    pub who_i_want_to_meet: Option<String>,

    #[validate(length(max = 120))]
    #[schema(example = "Desenvolvedor")]
    pub occupation: Option<String>,

    #[validate(length(max = 120))]
    pub company: Option<String>,

    #[validate(length(max = 200))]
    pub education: Option<String>,

    #[validate(length(max = 30), custom(function = "validate_list_items"))]
    #[serde(default)]
    pub music: Vec<String>,

    #[validate(length(max = 30), custom(function = "validate_list_items"))]
    #[serde(default)]
    pub movies: Vec<String>,

    #[validate(length(max = 30), custom(function = "validate_list_items"))]
    #[serde(default)]
    pub books: Vec<String>,

    #[validate(length(max = 30), custom(function = "validate_list_items"))]
    #[serde(default)]
    pub tv_shows: Vec<String>,

    #[validate(length(max = 30), custom(function = "validate_list_items"))]
    #[serde(default)]
    pub foods: Vec<String>,

    #[validate(length(max = 30), custom(function = "validate_list_items"))]
    #[serde(default)]
    pub sports: Vec<String>,

    #[validate(length(max = 120))]
    pub pets: Option<String>,

    #[validate(length(max = 120))]
    pub fashion_style: Option<String>,

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
        return Ok(
            HttpResponse::Ok().json(ApiResponse::new(ProfileVisitorsResponse {
                enabled: false,
                count_30d: 0,
                recent: Vec::new(),
            })),
        );
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

    Ok(
        HttpResponse::Ok().json(ApiResponse::new(ProfileVisitorsResponse {
            enabled: true,
            count_30d,
            recent,
        })),
    )
}

#[derive(Debug, Deserialize)]
pub struct BirthdayQuery {
    /// Fuso horário IANA de quem está chamando, como `America/Sao_Paulo`.
    pub tz: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct Birthday {
    pub profile: ProfileSummary,
    /// Dia do mês do aniversário.
    pub day: i32,
    /// Idade que a pessoa completa neste ano.
    pub age: i32,
    pub is_today: bool,
}

#[derive(Serialize, ToSchema)]
pub struct BirthdayResponse {
    /// Hoje no fuso recebido. Vai junto para a tela não depender do relógio local
    /// para saber de que dia a lista está falando.
    pub today: NaiveDate,
    /// O fuso que foi usado de verdade, que é o padrão quando `tz` não vem.
    pub time_zone: String,
    /// Aniversariantes do mês corrente, do dia 1 para o 31.
    pub birthdays: Vec<Birthday>,
}

#[utoipa::path(
    get,
    path = "/api/v1/me/birthday",
    description = "Aniversariantes do mês entre os amigos, no fuso de quem chama. Sem paginação: o mês tem no máximo os amigos do usuário",
    params(
        ("tz" = Option<String>, Query, description = "Fuso IANA de quem chama, como America/Sao_Paulo. Sem ele, o padrão do servidor"),
    ),
    responses(
        (status = 200, description = "Aniversariantes do mês", body = ApiResponse<BirthdayResponse>),
        (status = 400, description = "Fuso horário desconhecido", body = ApiErrorResponse),
        (status = 401, description = "Não autenticado", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Lista os aniversariantes do mês entre os amigos", skip_all)]
pub async fn birthday(
    query: Query<BirthdayQuery>,
    session: SessionData,
    pool: Data<PgPool>,
    storage: Data<Arc<dyn StorageService>>,
) -> Result<impl Responder, actix_web::Error> {
    let time_zone = query
        .tz
        .as_deref()
        .map(str::trim)
        .filter(|tz| !tz.is_empty())
        .unwrap_or(DEFAULT_TIME_ZONE)
        .to_owned();

    // `AT TIME ZONE` estoura erro de banco com um fuso que não existe, então a
    // mesma consulta cai em UTC e diz se o nome recebido é conhecido.
    let now = sqlx::query!(
        r#"
        SELECT
            (NOW() AT TIME ZONE COALESCE(
                (SELECT name FROM pg_timezone_names WHERE name = $1), 'UTC'
            ))::date AS "today!",
            EXISTS (SELECT 1 FROM pg_timezone_names WHERE name = $1) AS "known!"
        "#,
        time_zone,
    )
    .fetch_one(pool.get_ref())
    .await
    .map_err(db_err)?;

    if !now.known {
        return Ok(invalid_field(
            "tz",
            "unknown_time_zone",
            "Fuso horário desconhecido.",
        ));
    }

    let rows = sqlx::query!(
        r#"
        SELECT
            up.user_id AS id,
            up.slug,
            up.name,
            up.avatar_url,
            up.status,
            EXTRACT(DAY FROM up.birthdate)::int AS "day!",
            (EXTRACT(YEAR FROM $2::date) - EXTRACT(YEAR FROM up.birthdate))::int AS "age!",
            -- O mês já está filtrado no WHERE, então o dia basta. Quem nasceu em
            -- 29 de fevereiro aparece no mês, mas só é "hoje" em ano bissexto.
            (EXTRACT(DAY FROM up.birthdate) = EXTRACT(DAY FROM $2::date)) AS "is_today!"
        FROM friend f
        INNER JOIN auth_user au ON au.id = f.friend_id AND au.status = 'active'
        INNER JOIN user_profile up ON up.user_id = f.friend_id
        WHERE f.user_id = $1
          AND up.birthdate IS NOT NULL
          -- Perfil fechado para todo mundo some daqui também, igual no feed.
          AND up.profile_visibility <> 'nobody'
          AND EXTRACT(MONTH FROM up.birthdate) = EXTRACT(MONTH FROM $2::date)
        ORDER BY EXTRACT(DAY FROM up.birthdate), up.name
        "#,
        session.user_id,
        now.today,
    )
    .fetch_all(pool.get_ref())
    .await
    .map_err(db_err)?;

    let storage = storage.as_ref().as_ref();
    let birthdays = rows
        .into_iter()
        .map(|row| Birthday {
            profile: ProfileSummary {
                id: row.id,
                slug: row.slug,
                name: row.name,
                avatar_url: public_url_of(storage, row.avatar_url),
                status: row.status,
            },
            day: row.day,
            age: row.age,
            is_today: row.is_today,
        })
        .collect();

    Ok(HttpResponse::Ok().json(ApiResponse::new(BirthdayResponse {
        today: now.today,
        time_zone,
        birthdays,
    })))
}

/// Teto de cada contador. O endpoint é pollado a cada 30 segundos, então a
/// consulta não pode crescer junto da caixa: quem tem 300 mil não lidas paga o
/// mesmo que quem tem 100. O front mostra "99+" quando bate no teto.
const UNREAD_COUNT_CAP: i64 = 100;

#[derive(Serialize, ToSchema)]
pub struct UnreadResponse {
    /// Vai até 100. Chegou em 100, há 100 ou mais.
    pub notifications: i64,
    /// Mesmo teto.
    pub messages: i64,
    /// Atividades no feed mais novas que o `feed_seen_at` do usuário, com o
    /// mesmo teto.
    pub feed: i64,
}

#[utoipa::path(
    get,
    path = "/api/v1/me/unread",
    description = "Contadores para os badges, cada um limitado a 100. Endpoint leve, feito para polling a cada 30s",
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
                SELECT COUNT(*) FROM (
                SELECT 1 FROM notification n
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
                LIMIT $2
                ) AS capped
            ) AS "notifications!",
            (
                SELECT COUNT(*) FROM (
                SELECT 1 FROM message m
                INNER JOIN conversation_participant cp
                    ON cp.conversation_id = m.conversation_id AND cp.user_id = $1
                WHERE m.sender_id <> $1
                  AND m.created_at > cp.last_read_at
                  AND (cp.hidden_at IS NULL OR m.created_at > cp.hidden_at)
                LIMIT $2
                ) AS capped
            ) AS "messages!",
            (
                SELECT COUNT(*) FROM (
                SELECT 1 FROM activity a
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
                LIMIT $2
                ) AS capped
            ) AS "feed!"
        "#,
        session.user_id,
        UNREAD_COUNT_CAP,
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
