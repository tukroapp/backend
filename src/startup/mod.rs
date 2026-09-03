mod application;
mod cors;
mod docs;
mod urls;

pub use application::{Application, get_connection_pool, get_redis_pool};
pub use cors::mount_cors_middleware;

pub type RedisPool = deadpool_redis::Pool;

use crate::{
    configuration::{EmailSettings, Environment},
    constants::MAX_UPLOAD_SIZE_BYTES,
    middlewares::common,
    services::{
        access_log::AccessLogPurgeWorker, account::DeletionWorker, email::EmailWorker,
        storage::StorageService,
    },
    utils::{
        CustomIpExtractor, UserSessionExtractor, json_parse_config, multipart_parse_config,
        not_found_service, path_parse_config, query_parse_config,
    },
};
use actix_files::Files;
use actix_governor::GovernorConfigBuilder;
use actix_web::{App, HttpServer, dev::Server, middleware::from_fn, web};
use sqlx::PgPool;
use std::{net::TcpListener, path::PathBuf, sync::Arc, time::Duration};
use tracing_actix_web::TracingLogger;
use urls::RateLimits;

#[derive(Clone, Debug)]
pub struct AppState {
    pub environment: Environment,
    pub frontend_url: String,
    pub backend_url: String,
    pub domain: String,
    pub google_client_id: String,
    pub google_client_secret: String,
    pub google_redirect_uri: String,
    pub google_auth_uri: String,
    pub google_token_uri: String,
    pub google_user_info_uri: String,
    pub secret_key: String,
    pub media_dir: PathBuf,
    /// Remetente de todo e-mail que sai. É a própria conta SMTP: mandar de um
    /// endereço que não é o autenticado costuma cair em spam ou ser recusado.
    pub from_email: String,
}

pub struct RunConfig {
    pub listener: TcpListener,
    pub db_pool: PgPool,
    pub redis_pool: RedisPool,
    pub app_state: AppState,
    pub cors_allowed_origins: Vec<String>,
    pub media_dir: PathBuf,
    pub email_settings: EmailSettings,
    pub workers: usize,
    pub storage_service: Arc<dyn StorageService>,
}

pub fn run(config: RunConfig) -> Result<Server, std::io::Error> {
    let RunConfig {
        listener,
        db_pool,
        redis_pool,
        app_state,
        cors_allowed_origins,
        media_dir,
        email_settings,
        workers,
        storage_service,
    } = config;
    let worker = EmailWorker::new(
        db_pool.clone(),
        email_settings.smtp_host,
        email_settings.smtp_port,
        email_settings.smtp_username,
        email_settings.smtp_password,
    );

    let db_pool = web::Data::new(db_pool);
    let redis_pool = web::Data::new(redis_pool);
    let app_state = web::Data::new(app_state);
    let storage_data = web::Data::new(storage_service);

    let is_dev = cfg!(test) || app_state.environment == Environment::Development;

    if !is_dev {
        tokio::spawn(async move { worker.run().await });

        let deletion_worker = DeletionWorker::new(
            db_pool.as_ref().clone(),
            redis_pool.as_ref().clone(),
            Arc::clone(storage_data.as_ref()),
        );
        tokio::spawn(async move { deletion_worker.run().await });

        let access_log_worker = AccessLogPurgeWorker::new(db_pool.as_ref().clone());
        tokio::spawn(async move { access_log_worker.run().await });
    }

    let strict_rate_limit_config = GovernorConfigBuilder::default()
        .seconds_per_request(20)
        .burst_size(5)
        .key_extractor(CustomIpExtractor)
        .permissive(is_dev)
        .use_headers()
        .finish()
        .unwrap();

    let permissive_rate_limit_config = GovernorConfigBuilder::default()
        .requests_per_second(2)
        .burst_size(20)
        .key_extractor(CustomIpExtractor)
        .permissive(is_dev)
        .use_headers()
        .finish()
        .unwrap();

    let auth_rate_limit_config = GovernorConfigBuilder::default()
        .requests_per_second(10)
        .burst_size(100)
        .key_extractor(UserSessionExtractor)
        .permissive(is_dev)
        .use_headers()
        .finish()
        .unwrap();

    let auth_limiter = auth_rate_limit_config.limiter().clone();
    let strict_limiter = strict_rate_limit_config.limiter().clone();
    let permissive_limiter = permissive_rate_limit_config.limiter().clone();

    // Garbage collection task for rate limiter state
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(300));
        loop {
            interval.tick().await;
            strict_limiter.retain_recent();
            strict_limiter.shrink_to_fit();
            permissive_limiter.retain_recent();
            permissive_limiter.shrink_to_fit();
            auth_limiter.retain_recent();
            auth_limiter.shrink_to_fit();
            tracing::info!("Cleaned up rate limiter state");
        }
    });

    let server = HttpServer::new(move || {
        let cors = mount_cors_middleware(&app_state.environment, cors_allowed_origins.clone());

        let app = App::new()
            .wrap(cors)
            .wrap(from_fn(common))
            .wrap(TracingLogger::default())
            .service(urls::api_urls(&RateLimits {
                strict: &strict_rate_limit_config,
                permissive: &permissive_rate_limit_config,
                auth: &auth_rate_limit_config,
            }))
            .default_service(web::route().to(not_found_service))
            .app_data(multipart_parse_config(MAX_UPLOAD_SIZE_BYTES))
            .app_data(json_parse_config())
            .app_data(path_parse_config())
            .app_data(query_parse_config())
            .app_data(db_pool.clone())
            .app_data(redis_pool.clone())
            .app_data(app_state.clone())
            .app_data(storage_data.clone());

        if app_state.environment == Environment::Development {
            // Só em dev os uploads ficam no disco; em produção quem serve é o
            // bucket, e o diretório nem existe.
            app.service(Files::new("/media", &media_dir).use_last_modified(true))
                .service(docs::swagger_ui())
        } else {
            app
        }
    })
    .workers(workers)
    .listen(listener)?
    .run();

    Ok(server)
}
