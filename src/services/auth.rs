use std::{
    future::{Ready, ready},
    sync::LazyLock,
};

use actix_web::http::header::USER_AGENT;
use actix_web::{
    FromRequest, HttpMessage, HttpRequest, HttpResponse,
    cookie::{
        Cookie, SameSite,
        time::{Duration, OffsetDateTime},
    },
    dev::Payload,
    error::InternalError,
};
use anyhow::{Context, anyhow};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use deadpool_redis::redis::cmd;
use derive_more::Display;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    configuration::Environment,
    constants::{
        SESSION_COOKIE_NAME, SESSION_REDIS_PREFIX, SESSION_TOKEN_LENGTH, SESSION_TTL_SECONDS,
        USER_SESSIONS_REDIS_PREFIX,
    },
    startup::{AppState, RedisPool},
    telemetry::spawn_blocking_with_tracing,
    utils::{
        ApiErrorResponse, compute_hash, compute_sha_hash, db_err, e500, gen_random_string,
        verify_hash,
    },
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionData {
    pub hash: String,
    pub user_id: Uuid,
}

impl SessionData {
    pub async fn is_superuser(&self, pool: &PgPool) -> Result<(), actix_web::Error> {
        let user = sqlx::query!(
            r#"
            SELECT is_superuser
            FROM auth_user
            WHERE id = $1 AND status = 'active'
            "#,
            self.user_id
        )
        .fetch_optional(pool)
        .await
        .map_err(db_err)?;

        match user {
            Some(u) if u.is_superuser => Ok(()),
            Some(_) => Err(InternalError::from_response(
                "Superuser privileges required",
                HttpResponse::Forbidden().json(ApiErrorResponse::from_single_validation_error(
                    "permission",
                    "permission_denied",
                    Some("Permissão negada"),
                    vec![("user_id", &self.user_id.to_string())],
                )),
            )
            .into()),
            None => Err(InternalError::from_response(
                "User not found",
                HttpResponse::NotFound().json(ApiErrorResponse::from_single_validation_error(
                    "user",
                    "user_not_found",
                    Some("Usuário não encontrado"),
                    vec![("user_id", &self.user_id.to_string())],
                )),
            )
            .into()),
        }
    }
}

impl FromRequest for SessionData {
    type Error = actix_web::Error;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _payload: &mut Payload) -> Self::Future {
        match req.extensions().get::<SessionData>() {
            Some(session) => ready(Ok(session.clone())),
            None => ready(Err(InternalError::from_response(
                "Session data not found in request extensions",
                HttpResponse::Unauthorized().json(ApiErrorResponse::from_single_validation_error(
                    "session",
                    "session_data_not_found",
                    Some("Dados de sessão não encontrados"),
                    vec![],
                )),
            )
            .into())),
        }
    }
}

#[derive(Display, Clone, Debug, Serialize, Deserialize)]
pub enum LoginType {
    #[display("Standard Login")]
    Standard(LoginCredentials),
    #[display("Google Login")]
    Google(GoogleUserInfo),
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct LoginCredentials {
    #[schema(example = "test@example.com")]
    pub email: String,
    #[schema(example = "minhasenhasupersegura")]
    pub password: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UserDb {
    pub id: Uuid,
    pub password_hash: Option<String>,
}

/// Espelha o enum `account_status` do banco. `Deactivated` é a saída voluntária,
/// que o próprio login desfaz; `Suspended` é a decisão da moderação, que só um
/// administrador desfaz.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[sqlx(type_name = "account_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    Active,
    Suspended,
    Deactivated,
    /// Conta já anonimizada pelo pedido de exclusão. Não volta.
    Deleted,
}

/// Suspensão em vigor, do jeito que o usuário barrado precisa ver: `until` nulo
/// é suspensão permanente.
#[derive(Debug, Clone)]
pub struct Suspension {
    pub until: Option<DateTime<Utc>>,
    pub reason: Option<String>,
}

impl Suspension {
    pub fn message(&self) -> String {
        match self.until {
            Some(until) => format!(
                "Sua conta está suspensa até {}.",
                until.format("%d/%m/%Y às %H:%M")
            ),
            None => "Sua conta está suspensa.".to_string(),
        }
    }

    pub fn error_response(&self) -> HttpResponse {
        let until = self.until.map(|until| until.to_rfc3339());
        let mut params = Vec::new();

        if let Some(ref until) = until {
            params.push(("suspended_until", until.as_str()));
        }
        if let Some(ref reason) = self.reason {
            params.push(("reason", reason.as_str()));
        }

        HttpResponse::Forbidden().json(ApiErrorResponse::from_single_validation_error(
            "account",
            "account_suspended",
            Some(&self.message()),
            params,
        ))
    }
}

/// Só a suspensão precisa sair estruturada daqui: o login por senha responde um
/// 403 com os detalhes, e o callback do Google os repassa na URL de redirect.
/// O resto do login continua sendo uma resposta HTTP já montada.
#[derive(Debug)]
pub enum LoginError {
    Suspended(Suspension),
    Other(actix_web::Error),
}

impl From<actix_web::Error> for LoginError {
    fn from(error: actix_web::Error) -> Self {
        Self::Other(error)
    }
}

impl From<LoginError> for actix_web::Error {
    fn from(error: LoginError) -> Self {
        match error {
            LoginError::Other(error) => error,
            LoginError::Suspended(suspension) => {
                InternalError::from_response("Account suspended", suspension.error_response())
                    .into()
            }
        }
    }
}

/// Porteiro comum aos dois caminhos de login. Conta desativada volta ao ar
/// sozinha, e suspensão com prazo vencido é liberada aqui: o login é o único
/// momento em que ela precisa ter acabado, então não há worker varrendo isso.
pub async fn ensure_account_can_login(
    user_id: &Uuid,
    status: AccountStatus,
    suspended_until: Option<DateTime<Utc>>,
    status_reason: Option<String>,
    transaction: &mut Transaction<'static, Postgres>,
) -> Result<(), LoginError> {
    match status {
        AccountStatus::Active => Ok(()),
        AccountStatus::Deactivated => Ok(reactivate_account(user_id, transaction).await?),
        // A conta anonimizada não tem mais credencial nem e-mail real, então na
        // prática ninguém chega aqui. O braço existe para o dia em que chegar.
        AccountStatus::Deleted => Err(LoginError::Other(e500(anyhow::anyhow!(
            "login attempt on a deleted account"
        )))),
        AccountStatus::Suspended => match suspended_until {
            Some(until) if until <= Utc::now() => {
                Ok(reactivate_account(user_id, transaction).await?)
            }
            _ => Err(LoginError::Suspended(Suspension {
                until: suspended_until,
                reason: status_reason,
            })),
        },
    }
}

/// Devolve a conta ao ar, seja saída voluntária ou suspensão já vencida. É aqui
/// que o pedido de exclusão é cancelado: entrar de novo dentro do prazo desfaz.
async fn reactivate_account(
    user_id: &Uuid,
    transaction: &mut Transaction<'static, Postgres>,
) -> Result<(), actix_web::Error> {
    sqlx::query!(
        r#"
        UPDATE auth_user
        SET status = 'active',
            status_reason = NULL,
            suspended_until = NULL,
            status_changed_at = NOW(),
            status_changed_by = NULL,
            deletion_scheduled_at = NULL
        WHERE id = $1
        "#,
        user_id
    )
    .execute(&mut **transaction)
    .await
    .map_err(db_err)?;

    Ok(())
}

#[derive(Deserialize, Debug, Serialize, Clone, ToSchema)]
pub struct GoogleUserInfo {
    pub id: String,
    pub email: String,
    pub name: String,
    pub picture: String,
}

#[derive(Serialize, Deserialize)]
pub struct SessionValue {
    pub user_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
}

pub struct SessionMeta {
    pub user_agent: Option<String>,
    pub ip: Option<String>,
}

pub struct NewSession {
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

impl SessionMeta {
    pub fn from_http_request(req: &HttpRequest) -> Self {
        let user_agent = req
            .headers()
            .get(USER_AGENT)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_string());

        // realip_remote_addr confia no X-Forwarded-For/Forwarded de qualquer peer, então o
        // client pode forjar esse valor. Só é confiável porque o Nginx na frente sobrescreve
        // o header. Enquanto for telemetria pura, tudo bem. Antes de usar para rate limiting
        // ou detecção de anomalia, garantir o hardening (Nginx sobrescrevendo, não anexando).
        let ip = req
            .connection_info()
            .realip_remote_addr()
            .map(|value| value.to_string());

        Self { user_agent, ip }
    }
}

static DUMMY_HASH: LazyLock<String> =
    LazyLock::new(|| compute_hash(Uuid::new_v4().to_string()).unwrap());

#[tracing::instrument(name = "Login do usuário", skip_all)]
pub async fn login(
    pool: &PgPool,
    redis: &RedisPool,
    login_type: LoginType,
    meta: SessionMeta,
) -> Result<NewSession, LoginError> {
    let mut tx = pool.begin().await.map_err(db_err)?;

    let user_id = match login_type {
        LoginType::Standard(creds) => {
            let user = validate_credentials(creds, &mut tx).await?;
            user.id
        }
        LoginType::Google(info) => resolve_google_user(info, &mut tx).await?,
    };

    update_last_login(&user_id, &mut tx).await?;

    tx.commit().await.map_err(db_err)?;

    Ok(persist_session(redis, &user_id, meta).await?)
}

fn account_conflict(message: &str, email: &str) -> LoginError {
    let error: actix_web::Error = InternalError::from_response(
        "Account conflict",
        HttpResponse::BadRequest().json(ApiErrorResponse::from_single_validation_error(
            "account_conflict",
            "account_conflict",
            Some(message),
            vec![("email", email)],
        )),
    )
    .into();

    LoginError::Other(error)
}

#[tracing::instrument(name = "Resolve o usuário do Google", skip_all)]
pub async fn resolve_google_user(
    info: GoogleUserInfo,
    transaction: &mut Transaction<'static, Postgres>,
) -> Result<Uuid, LoginError> {
    let user_by_id = sqlx::query!(
        r#"
        SELECT
            id,
            status AS "status: AccountStatus",
            suspended_until,
            status_reason,
            password_hash,
            email
        FROM auth_user
        WHERE google_id = $1
        "#,
        info.id
    )
    .fetch_optional(&mut **transaction)
    .await
    .map_err(e500)?;

    if let Some(user) = user_by_id {
        if user.password_hash.is_some() {
            tracing::warn!("Account conflict for Google user with email {}", info.email);
            return Err(account_conflict(
                "Esta conta possui uma senha definida. Por favor, faça login utilizando senha.",
                &info.email,
            ));
        }

        // Entrar pelo Google não pode furar suspensão em vigor, que é decisão
        // da moderação, ao contrário da desativação, que o próprio dono desfaz.
        ensure_account_can_login(
            &user.id,
            user.status,
            user.suspended_until,
            user.status_reason,
            transaction,
        )
        .await?;

        if user.email != info.email {
            sqlx::query!(
                r#"
                UPDATE auth_user
                SET email = $1, is_email_verified = true
                WHERE id = $2
                "#,
                info.email,
                user.id,
            )
            .execute(&mut **transaction)
            .await
            .map_err(e500)?;

            sqlx::query!(
                r#"
                UPDATE user_profile
                SET name = $1
                WHERE user_id = $2
                "#,
                info.name,
                user.id,
            )
            .execute(&mut **transaction)
            .await
            .map_err(e500)?;
        }

        return Ok(user.id);
    }

    let user_by_email = sqlx::query!(r#"SELECT id FROM auth_user WHERE email = $1"#, info.email)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(e500)?;

    if user_by_email.is_some() {
        tracing::warn!("Account conflict for Google user with email {}", info.email);
        return Err(account_conflict(
            "Este e-mail já está em uso por outra conta.",
            &info.email,
        ));
    }

    tracing::info!(
        "Creating new user for Google user with email {}",
        info.email
    );
    let created = sqlx::query!(
        r#"
        INSERT INTO auth_user (email, google_id, is_email_verified)
        VALUES ($1, $2, true)
        RETURNING id
        "#,
        info.email,
        info.id,
    )
    .fetch_one(&mut **transaction)
    .await
    .map_err(e500)?;

    crate::services::slug::create_user_profile(transaction, &created.id, &info.name).await?;

    Ok(created.id)
}

async fn update_last_login(
    user_id: &Uuid,
    transaction: &mut Transaction<'static, Postgres>,
) -> Result<(), actix_web::Error> {
    sqlx::query!(
        r#"
        UPDATE auth_user
        SET last_login = NOW()
        WHERE id = $1
        "#,
        user_id
    )
    .execute(&mut **transaction)
    .await
    .map_err(db_err)?;

    Ok(())
}

fn session_key(token_hash: &str) -> String {
    format!("{}{}", SESSION_REDIS_PREFIX, token_hash)
}

fn user_sessions_key(user_id: &Uuid) -> String {
    format!("{}{}", USER_SESSIONS_REDIS_PREFIX, user_id)
}

async fn persist_session(
    redis: &RedisPool,
    user_id: &Uuid,
    meta: SessionMeta,
) -> Result<NewSession, actix_web::Error> {
    let token = gen_random_string(SESSION_TOKEN_LENGTH);
    let token_hash = compute_sha_hash(&token);
    let index_key = user_sessions_key(user_id);

    let created_at = Utc::now();
    let expires_at = created_at + ChronoDuration::seconds(SESSION_TTL_SECONDS);

    let value = SessionValue {
        user_id: *user_id,
        created_at,
        user_agent: meta.user_agent,
        ip: meta.ip,
    };
    let value = serde_json::to_string(&value).map_err(e500)?;

    let mut conn = redis.get().await.map_err(e500)?;

    cmd("SET")
        .arg(session_key(&token_hash))
        .arg(value)
        .arg("EX")
        .arg(SESSION_TTL_SECONDS)
        .query_async::<()>(&mut conn)
        .await
        .map_err(e500)?;

    cmd("HSET")
        .arg(&index_key)
        .arg(&token_hash)
        .arg(1)
        .query_async::<()>(&mut conn)
        .await
        .map_err(e500)?;

    cmd("HEXPIRE")
        .arg(&index_key)
        .arg(SESSION_TTL_SECONDS)
        .arg("FIELDS")
        .arg(1)
        .arg(&token_hash)
        .query_async::<()>(&mut conn)
        .await
        .map_err(e500)?;

    Ok(NewSession { token, expires_at })
}

pub async fn touch_session(
    redis: &RedisPool,
    token_hash: &str,
) -> Result<Option<Uuid>, actix_web::Error> {
    let mut conn = redis.get().await.map_err(e500)?;

    let raw: Option<String> = cmd("GETEX")
        .arg(session_key(token_hash))
        .arg("EX")
        .arg(SESSION_TTL_SECONDS)
        .query_async(&mut conn)
        .await
        .map_err(e500)?;

    let Some(raw) = raw else {
        return Ok(None);
    };

    let value: SessionValue = serde_json::from_str(&raw).map_err(e500)?;

    cmd("HEXPIRE")
        .arg(user_sessions_key(&value.user_id))
        .arg(SESSION_TTL_SECONDS)
        .arg("FIELDS")
        .arg(1)
        .arg(token_hash)
        .query_async::<()>(&mut conn)
        .await
        .map_err(e500)?;

    Ok(Some(value.user_id))
}

pub async fn delete_session(
    redis: &RedisPool,
    user_id: &Uuid,
    token_hash: &str,
) -> Result<(), actix_web::Error> {
    let mut conn = redis.get().await.map_err(e500)?;

    cmd("DEL")
        .arg(session_key(token_hash))
        .query_async::<()>(&mut conn)
        .await
        .map_err(e500)?;

    cmd("HDEL")
        .arg(user_sessions_key(user_id))
        .arg(token_hash)
        .query_async::<()>(&mut conn)
        .await
        .map_err(e500)?;

    Ok(())
}

pub async fn delete_all_sessions(
    redis: &RedisPool,
    user_id: &Uuid,
) -> Result<(), actix_web::Error> {
    let index_key = user_sessions_key(user_id);

    let mut conn = redis.get().await.map_err(e500)?;

    let token_hashes: Vec<String> = cmd("HKEYS")
        .arg(&index_key)
        .query_async(&mut conn)
        .await
        .map_err(e500)?;

    for token_hash in &token_hashes {
        cmd("DEL")
            .arg(session_key(token_hash))
            .query_async::<()>(&mut conn)
            .await
            .map_err(e500)?;
    }

    cmd("DEL")
        .arg(&index_key)
        .query_async::<()>(&mut conn)
        .await
        .map_err(e500)?;

    Ok(())
}

pub async fn delete_other_sessions(
    redis: &RedisPool,
    user_id: &Uuid,
    keep_hash: &str,
) -> Result<(), actix_web::Error> {
    let index_key = user_sessions_key(user_id);

    let mut conn = redis.get().await.map_err(e500)?;

    let token_hashes: Vec<String> = cmd("HKEYS")
        .arg(&index_key)
        .query_async(&mut conn)
        .await
        .map_err(e500)?;

    for token_hash in &token_hashes {
        if token_hash == keep_hash {
            continue;
        }

        cmd("DEL")
            .arg(session_key(token_hash))
            .query_async::<()>(&mut conn)
            .await
            .map_err(e500)?;

        cmd("HDEL")
            .arg(&index_key)
            .arg(token_hash)
            .query_async::<()>(&mut conn)
            .await
            .map_err(e500)?;
    }

    Ok(())
}

#[tracing::instrument(name = "Valida as credenciais do usuário", skip_all)]
pub async fn validate_credentials(
    credentials: LoginCredentials,
    transaction: &mut Transaction<'static, Postgres>,
) -> Result<UserDb, LoginError> {
    let mut expected_password_hash = DUMMY_HASH.clone();

    // O status só é avaliado depois da senha conferir, para a resposta não
    // revelar a situação da conta de quem não provou ser o dono.
    let user_db = sqlx::query!(
        r#"
        SELECT
            id,
            password_hash,
            status AS "status: AccountStatus",
            suspended_until,
            status_reason
        FROM auth_user
        WHERE email = $1
        "#,
        credentials.email
    )
    .fetch_optional(&mut **transaction)
    .await
    .map_err(db_err)?;

    if let Some(ref user) = user_db
        && let Some(ref hash) = user.password_hash
    {
        expected_password_hash = hash.clone();
    };

    spawn_blocking_with_tracing(move || {
        verify_hash(expected_password_hash, credentials.password.clone())
    })
    .await
    .context("Failed to verify password")
    .map_err(e500)?
    .map_err(e500)?;

    let user = user_db.ok_or_else(|| e500(anyhow!("Credenciais inválidas")))?;

    ensure_account_can_login(
        &user.id,
        user.status,
        user.suspended_until,
        user.status_reason,
        transaction,
    )
    .await?;

    Ok(UserDb {
        id: user.id,
        password_hash: user.password_hash,
    })
}

pub fn make_token_cookie(state: &AppState, token: &str) -> Cookie<'static> {
    let mut builder = Cookie::build(SESSION_COOKIE_NAME, token.to_string())
        .path("/")
        .http_only(false)
        .expires(OffsetDateTime::now_utc().saturating_add(Duration::seconds(SESSION_TTL_SECONDS)));

    if state.environment == Environment::Production {
        builder = builder
            .secure(true)
            .same_site(SameSite::Lax)
            .domain(state.domain.clone());
    } else {
        builder = builder.secure(false).same_site(SameSite::Lax);
    }

    builder.finish()
}
