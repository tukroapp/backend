use anyhow::Context;
use derive_more::Display;
use dotenvy::var;
use serde::Deserialize;
use serde_aux::field_attributes::deserialize_number_from_string;
use sqlx::postgres::{PgConnectOptions, PgSslMode};

#[derive(Deserialize, Clone, Display, Debug, PartialEq, Eq)]
pub enum Environment {
    #[display("development")]
    Development,

    #[display("production")]
    Production,
}

#[derive(Deserialize, Clone)]
pub struct ApplicationSettings {
    pub environment: Environment,
    #[serde(deserialize_with = "deserialize_number_from_string")]
    pub port: u16,
    pub host: String,
    pub base_url: String,
    pub domain: String,
    pub cors_allowed_origins: Vec<String>,
    pub frontend_url: String,
    pub google_client_id: String,
    pub google_client_secret: String,
    pub google_redirect_uri: String,
    pub google_auth_uri: String,
    pub google_token_uri: String,
    pub google_user_info_uri: String,
    pub secret_key: String,
    pub workers: usize,
    pub s3_endpoint_url: String,
    pub s3_access_key_id: String,
    pub s3_secret_access_key: String,
    pub s3_bucket: String,
    pub s3_public_base_url: String,
}

#[derive(Deserialize, Clone)]
pub struct DatabaseSettings {
    pub username: String,
    pub password: String,
    pub database_name: String,
    pub host: String,
    #[serde(deserialize_with = "deserialize_number_from_string")]
    pub port: u16,
    pub require_ssl: bool,
    pub max_connections: u32,
}

#[derive(Deserialize, Clone, Debug)]
pub struct EmailSettings {
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_username: String,
    pub smtp_password: String,
}

#[derive(Deserialize, Clone)]
pub struct RedisSettings {
    pub url: String,
}

#[derive(Deserialize, Clone)]
pub struct Settings {
    pub application: ApplicationSettings,
    pub database: DatabaseSettings,
    pub email: EmailSettings,
    pub redis: RedisSettings,
}

impl DatabaseSettings {
    pub fn connect_options(&self) -> PgConnectOptions {
        let ssl_mode = if self.require_ssl {
            PgSslMode::Require
        } else {
            PgSslMode::Prefer
        };

        PgConnectOptions::new()
            .host(&self.host)
            .port(self.port)
            .username(&self.username)
            .password(&self.password)
            .database(&self.database_name)
            .ssl_mode(ssl_mode)
    }
}

pub fn get_configuration() -> Result<Settings, anyhow::Error> {
    dotenvy::dotenv().ok();

    let env = var("APP_ENVIRONMENT").unwrap_or_else(|_| "dev".into());
    let environment = match env.as_str() {
        "dev" => Environment::Development,
        "prod" => Environment::Production,
        other => {
            return Err(anyhow::anyhow!(
                "Unsupported APP_ENVIRONMENT value: {}",
                other
            ));
        }
    };

    let cors_allowed_origins: Vec<String> = var("CORS_ALLOWED_ORIGINS")
        .context("CORS_ALLOWED_ORIGINS must be set.")?
        .trim()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if cors_allowed_origins.is_empty() {
        return Err(anyhow::anyhow!(
            "CORS_ALLOWED_ORIGINS needs at least one origin."
        ));
    }

    let application_settings = ApplicationSettings {
        environment,
        cors_allowed_origins,
        port: var("APP_PORT")
            .context("APP_PORT must be set")?
            .parse()
            .context("APP_PORT must be a valid number")?,
        host: var("APP_HOST").context("APP_HOST must be set")?,
        base_url: var("APP_BASE_URL").context("APP_BASE_URL must be set")?,
        domain: var("APP_DOMAIN").context("APP_DOMAIN must be set")?,
        frontend_url: var("APP_FRONTEND_URL").context("APP_FRONTEND_URL must be set")?,
        google_client_id: var("GOOGLE_CLIENT_ID").context("GOOGLE_CLIENT_ID must be set")?,
        google_client_secret: var("GOOGLE_CLIENT_SECRET")
            .context("GOOGLE_CLIENT_SECRET must be set")?,
        google_redirect_uri: var("GOOGLE_REDIRECT_URI")
            .context("GOOGLE_REDIRECT_URI must be set")?,
        google_auth_uri: var("GOOGLE_AUTH_URI").context("GOOGLE_AUTH_URI must be set")?,
        google_token_uri: var("GOOGLE_TOKEN_URI").context("GOOGLE_TOKEN_URI must be set")?,
        google_user_info_uri: var("GOOGLE_USER_INFO_URI")
            .context("GOOGLE_USER_INFO_URI must be set")?,
        secret_key: var("SECRET_KEY").context("SECRET_KEY must be set")?,
        workers: var("APP_WORKERS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(1)
            }),

        s3_endpoint_url: var("S3_ENDPOINT_URL").context("S3_ENDPOINT_URL must be set")?,
        s3_access_key_id: var("S3_ACCESS_KEY_ID").context("S3_ACCESS_KEY_ID must be set")?,
        s3_secret_access_key: var("S3_SECRET_ACCESS_KEY")
            .context("S3_SECRET_ACCESS_KEY must be set")?,
        s3_bucket: var("S3_BUCKET").context("S3_BUCKET must be set")?,
        s3_public_base_url: var("S3_PUBLIC_BASE_URL").context("S3_PUBLIC_BASE_URL must be set")?,
    };

    let database_settings = DatabaseSettings {
        username: var("DB_USERNAME").context("DB_USERNAME must be set")?,
        password: var("DB_PASSWORD").context("DB_PASSWORD must be set")?,
        database_name: var("DB_NAME").context("DB_NAME must be set")?,
        host: var("DB_HOST").context("DB_HOST must be set")?,
        port: var("DB_PORT")
            .context("DB_PORT must be set")?
            .parse()
            .context("DB_PORT must be a valid number")?,
        require_ssl: var("DB_REQUIRE_SSL").map(|s| s == "true").unwrap_or(false),
        max_connections: var("DB_MAX_CONNECTIONS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10),
    };

    let email_username = var("EMAIL_USERNAME").expect("EMAIL_USERNAME must be set");
    let email_password = var("EMAIL_PASSWORD").expect("EMAIL_PASSWORD must be set");
    let email_smtp_server = var("EMAIL_SMTP_SERVER").expect("EMAIL_SMTP_SERVER must be set");
    let email_smtp_port = var("EMAIL_SMTP_PORT")
        .expect("EMAIL_SMTP_PORT must be set")
        .parse::<u16>()
        .expect("Failed to parse EMAIL_SMTP_PORT");

    let email_settings = EmailSettings {
        smtp_host: email_smtp_server,
        smtp_port: email_smtp_port,
        smtp_username: email_username,
        smtp_password: email_password,
    };

    let redis_settings = RedisSettings {
        url: var("REDIS_URL").context("REDIS_URL must be set")?,
    };

    Ok(Settings {
        application: application_settings,
        database: database_settings,
        email: email_settings,
        redis: redis_settings,
    })
}
