use std::{env, net::TcpListener, sync::Arc, time::Duration};

use anyhow::Context;
use deadpool_redis::Config;
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::fs;

use crate::{
    configuration::{DatabaseSettings, Environment, RedisSettings, Settings},
    services::storage::{LocalStorage, S3Storage, StorageService},
};

use super::{AppState, RedisPool, RunConfig, run};

pub struct Application {
    port: u16,
    server: actix_web::dev::Server,
}

impl Application {
    pub async fn build(configuration: Settings) -> Result<Self, std::io::Error> {
        let connection_pool = get_connection_pool(&configuration.database);
        let redis_pool = get_redis_pool(&configuration.redis).map_err(std::io::Error::other)?;

        let address = format!(
            "{}:{}",
            configuration.application.host, configuration.application.port
        );

        let listener = TcpListener::bind(address)?;
        let port = listener.local_addr().unwrap().port();

        let current_dir = env::current_dir().expect("Falha ao determinar diretório atual");
        let media_dir = current_dir.join("media");

        if !media_dir.exists() {
            tracing::info!(
                "Diretório de uploads não encontrado, criando em: {:?}",
                media_dir.to_str()
            );

            fs::create_dir_all(&media_dir).await?;
        }

        let is_dev =
            cfg!(test) || configuration.application.environment == Environment::Development;

        let app_state = AppState {
            environment: configuration.application.environment,
            domain: configuration.application.domain,
            google_client_id: configuration.application.google_client_id,
            google_client_secret: configuration.application.google_client_secret,
            google_redirect_uri: configuration.application.google_redirect_uri,
            google_auth_uri: configuration.application.google_auth_uri,
            google_token_uri: configuration.application.google_token_uri,
            google_user_info_uri: configuration.application.google_user_info_uri,
            frontend_url: configuration.application.frontend_url,
            backend_url: configuration.application.base_url,
            secret_key: configuration.application.secret_key,
            media_dir: media_dir.clone(),
            from_email: configuration.email.smtp_username.clone(),
        };

        // Em desenvolvimento e nos testes os arquivos ficam no disco, servidos
        // pelo mount estático /media, para não depender do bucket.
        let storage_service: Arc<dyn StorageService> = if is_dev {
            // URL absoluta do próprio backend: o SPA roda em outra porta e uma
            // URL relativa apontaria para o servidor do front.
            Arc::new(LocalStorage::new(
                media_dir.to_str().unwrap(),
                &format!("{}/media", app_state.backend_url),
            ))
        } else {
            Arc::new(
                S3Storage::new(
                    &configuration.application.s3_endpoint_url,
                    &configuration.application.s3_access_key_id,
                    &configuration.application.s3_secret_access_key,
                    &configuration.application.s3_bucket,
                    &configuration.application.s3_public_base_url,
                )
                .await,
            )
        };

        let server = run(RunConfig {
            listener,
            db_pool: connection_pool,
            redis_pool,
            app_state,
            cors_allowed_origins: configuration.application.cors_allowed_origins,
            media_dir,
            email_settings: configuration.email,
            workers: configuration.application.workers,
            storage_service,
        })?;

        Ok(Self { port, server })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub async fn run_until_stopped(self) -> Result<(), std::io::Error> {
        self.server.await
    }
}

pub fn get_connection_pool(configuration: &DatabaseSettings) -> PgPool {
    PgPoolOptions::new()
        .max_connections(configuration.max_connections)
        .min_connections(0)
        .idle_timeout(Duration::from_secs(10))
        .connect_lazy_with(configuration.connect_options())
}

pub fn get_redis_pool(configuration: &RedisSettings) -> Result<RedisPool, anyhow::Error> {
    Config::from_url(&configuration.url)
        .create_pool(Some(deadpool_redis::Runtime::Tokio1))
        .context("Failed to create Redis pool")
}
