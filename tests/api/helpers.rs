use argon2::{
    Argon2, Params, PasswordHasher,
    password_hash::{SaltString, rand_core::OsRng},
};
use regex::Regex;
use serde_json::json;
use sqlx::{AssertSqlSafe, Connection, Executor, PgConnection, PgPool, postgres::PgPoolOptions};
use std::sync::{LazyLock, RwLock};
use tukro_backend::{
    configuration::{DatabaseSettings, Environment, get_configuration},
    startup::{Application, RedisPool, get_connection_pool, get_redis_pool},
    telemetry::{get_subscriber, init_subscriber},
};
use uuid::Uuid;
use wiremock::MockServer;

static TRACING: LazyLock<()> = LazyLock::new(|| {
    let default_filter_level = "info".to_string();
    let subscriber_name = "test".to_string();

    if std::env::var("TEST_LOG").is_ok() {
        let subscriber = get_subscriber(subscriber_name, default_filter_level, std::io::stdout);
        init_subscriber(subscriber);
    } else {
        let subscriber = get_subscriber(subscriber_name, default_filter_level, std::io::sink);
        init_subscriber(subscriber);
    }
});

#[allow(dead_code)]
pub struct TestApp {
    pub port: u16,
    pub address: String,
    pub db_pool: PgPool,
    pub redis_pool: RedisPool,
    pub test_user: TestUser,
    pub api_client: reqwest::Client,
    pub database_name: String,
    pub mock_google_server: MockServer,
    pub server_handle: tokio::task::JoinHandle<Result<(), std::io::Error>>,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        self.server_handle.abort();

        let database_name = self.database_name.clone();
        let cleanup = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to build cleanup runtime");

            runtime.block_on(async move {
                let config = get_configuration().expect("Failed to read configuration.");

                let mut connection = PgConnection::connect_with(
                    &DatabaseSettings {
                        database_name: "postgres".to_string(),
                        username: "postgres".to_string(),
                        password: "password".into(),
                        ..config.database.clone()
                    }
                    .connect_options(),
                )
                .await
                .expect("Failed to connect to Postgres for cleanup");

                let drop_db_query = format!(
                    r#"DROP DATABASE IF EXISTS "{}" WITH (FORCE);"#,
                    database_name
                );
                connection
                    .execute(AssertSqlSafe(drop_db_query))
                    .await
                    .expect("Failed to drop test database");
            });
        });

        let _ = cleanup.join();
    }
}

impl TestApp {
    pub async fn post_login<Body>(&self, body: &Body) -> reqwest::Response
    where
        Body: serde::Serialize,
    {
        self.api_client
            .post(format!("{}/api/v1/auth/login", &self.address))
            .json(body)
            .send()
            .await
            .expect("Failed to execute request")
    }
}

/// Usuário criado pelos endpoints públicos (register + login), usado quando o
/// teste precisa de mais de um usuário autenticado.
#[allow(dead_code)]
pub struct RegisteredUser {
    pub slug: String,
    pub email: String,
    pub token: String,
}

#[allow(dead_code)]
impl RegisteredUser {
    pub fn auth_header(&self) -> String {
        format!("Token {}", self.token)
    }
}

/// Cadastra e autentica um usuário, devolvendo slug, e-mail e token. O e-mail
/// já sai verificado: as ações que aparecem para terceiros exigem isso, e quase
/// todo teste quer uma conta em pleno funcionamento. Quem testa o próprio
/// bloqueio usa `register_and_login_unverified`.
#[allow(dead_code)]
pub async fn register_and_login(app: &TestApp, name: &str) -> RegisteredUser {
    let user = register_and_login_unverified(app, name).await;

    sqlx::query!(
        r#"
        UPDATE auth_user SET is_email_verified = TRUE
        WHERE id = (SELECT user_id FROM user_profile WHERE slug = $1)
        "#,
        user.slug,
    )
    .execute(&app.db_pool)
    .await
    .expect("Failed to verify the test user email");

    user
}

/// O mesmo cadastro, com o e-mail ainda por confirmar.
#[allow(dead_code)]
pub async fn register_and_login_unverified(app: &TestApp, name: &str) -> RegisteredUser {
    let email = format!("{}@test.com", Uuid::new_v4());
    let password = "supersecret123";

    let response = app
        .api_client
        .post(format!("{}/api/v1/auth/register", &app.address))
        .json(&json!({ "email": email, "password": password, "name": name }))
        .send()
        .await
        .expect("Failed to register");

    let body: serde_json::Value = response.json().await.expect("register response not JSON");
    let slug = body["results"]["slug"]
        .as_str()
        .expect("slug not found")
        .to_string();

    let response = app
        .post_login(&json!({ "email": email, "password": password }))
        .await;

    let body: serde_json::Value = response.json().await.expect("login response not JSON");
    let token = body["results"]["token"]
        .as_str()
        .expect("token not found")
        .to_string();

    RegisteredUser { slug, email, token }
}

pub struct TestUser {
    pub id: Uuid,
    pub email: String,
    pub password: String,
    pub token: RwLock<String>,
}

impl TestUser {
    pub fn generate() -> Self {
        Self {
            id: Uuid::now_v7(),
            email: format!("{}@test.com", Uuid::new_v4()),
            password: Uuid::new_v4().to_string(),
            token: RwLock::new(String::new()),
        }
    }

    pub async fn store(&self, pool: &PgPool) {
        let salt = SaltString::generate(&mut OsRng);
        let password_hash = Argon2::new(
            argon2::Algorithm::Argon2id,
            argon2::Version::V0x13,
            Params::new(15000, 2, 1, None).unwrap(),
        )
        .hash_password(self.password.as_bytes(), &salt)
        .unwrap()
        .to_string();

        sqlx::query!(
            r#"
            INSERT INTO auth_user (id, email, password_hash)
            VALUES ($1, $2, $3)
            "#,
            self.id,
            self.email,
            password_hash,
        )
        .execute(pool)
        .await
        .expect("Failed to create test user");

        self.store_profile(pool).await;
    }

    fn slug(&self) -> String {
        format!("u{}", &Uuid::new_v4().simple().to_string()[..20])
    }

    async fn store_profile(&self, pool: &PgPool) {
        sqlx::query!(
            r#"
            INSERT INTO user_profile (user_id, slug, name)
            VALUES ($1, $2, $3)
            "#,
            self.id,
            self.slug(),
            "Test User",
        )
        .execute(pool)
        .await
        .expect("Failed to create test user profile");
    }

    #[allow(dead_code)]
    pub async fn store_google_oauth(&self, pool: &PgPool) -> Uuid {
        let google_id = Uuid::new_v4();
        sqlx::query!(
            r#"
            INSERT INTO auth_user (id, email, google_id)
            VALUES ($1, $2, $3)
            "#,
            self.id,
            self.email,
            google_id.to_string(),
        )
        .execute(pool)
        .await
        .expect("Failed to create test user with Google OAuth");

        self.store_profile(pool).await;

        google_id
    }

    pub fn set_token(&self, token: &str) {
        *self.token.write().unwrap() = token.to_string();
    }

    pub fn token(&self) -> String {
        self.token.read().unwrap().clone()
    }

    /// Valor do header `Authorization` para requisições autenticadas.
    pub fn auth_header(&self) -> String {
        format!("Token {}", self.token())
    }

    pub async fn login(&self, app: &TestApp) {
        let res = app
            .post_login(&json!({
                "email": &self.email,
                "password": &self.password
            }))
            .await;

        let body: serde_json::Value = res.json().await.expect("login response is not valid JSON");

        let token = body["results"]["token"]
            .as_str()
            .expect("token not found in login response")
            .to_string();

        self.set_token(&token);
    }

    #[allow(dead_code)]
    pub async fn logout(&self, app: &TestApp) -> reqwest::Response {
        app.api_client
            .post(format!("{}/api/v1/auth/logout", &app.address))
            .header(reqwest::header::AUTHORIZATION, self.auth_header())
            .send()
            .await
            .expect("Failed to execute request")
    }

    pub async fn send_verification_email(&self, app: &TestApp) {
        app.api_client
            .post(format!(
                "{}/api/v1/auth/send-verification-email",
                &app.address
            ))
            .header(reqwest::header::AUTHORIZATION, self.auth_header())
            .send()
            .await
            .expect("Failed to execute request");
    }
}

pub async fn spawn_app() -> TestApp {
    LazyLock::force(&TRACING);

    let database_name = Uuid::now_v7().to_string();
    let mock_google_server = MockServer::start().await;

    let configuration = {
        let mut c = get_configuration().expect("Failed to read configuration.");
        c.database.database_name = database_name.clone();
        c.application.port = 0;
        // O `cfg!(test)` do código não enxerga o teste de integração, que linka a
        // lib compilada sem ele. Sem forçar aqui, um `.env` apontando para
        // produção liga o rate limit e manda o upload para o S3.
        c.application.environment = Environment::Development;
        c.application.google_auth_uri = format!("{}/auth", mock_google_server.uri());
        c.application.google_token_uri = format!("{}/token", mock_google_server.uri());
        c.application.google_user_info_uri = format!("{}/userinfo", mock_google_server.uri());
        c.application.google_client_id = "test_id".to_string();
        c.application.google_client_secret = "test_secret".to_string();
        c
    };

    configure_database(&configuration.database).await;

    let application = Application::build(configuration.clone())
        .await
        .expect("Failed to build application");

    let application_port = application.port();

    let server_handle = tokio::spawn(application.run_until_stopped());

    let client = reqwest::Client::builder()
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    let test_app = TestApp {
        port: application_port,
        address: format!("http://localhost:{}", application_port),
        db_pool: get_connection_pool(&configuration.database),
        redis_pool: get_redis_pool(&configuration.redis),
        api_client: client,
        test_user: TestUser::generate(),
        database_name,
        mock_google_server,
        server_handle,
    };

    test_app.test_user.store(&test_app.db_pool).await;
    test_app
}

pub async fn drop_app(app: TestApp) {
    app.db_pool.close().await;
    // The database is dropped by `TestApp`'s `Drop` impl, which also runs when an
    // assert panics, so no database is leaked on test failure.
    drop(app);
}

async fn configure_database(config: &DatabaseSettings) -> PgPool {
    let maintenance_settings = DatabaseSettings {
        database_name: "postgres".to_string(),
        username: "postgres".to_string(),
        password: "password".into(),
        ..config.clone()
    };
    let mut connection = PgConnection::connect_with(&maintenance_settings.connect_options())
        .await
        .expect("Failed to connect to Postgres");

    let create_db_query = format!(r#"CREATE DATABASE "{}";"#, config.database_name);
    connection
        .execute(AssertSqlSafe(create_db_query))
        .await
        .expect("Failed to create database");

    let connection_pool = PgPoolOptions::new()
        .max_connections(config.max_connections)
        .min_connections(0)
        .connect_with(config.connect_options())
        .await
        .expect("Failed to connect to Postgres.");

    sqlx::migrate!("./migrations")
        .run(&connection_pool)
        .await
        .expect("Failed to migrate the database.");
    connection_pool
}

#[allow(dead_code)]
pub fn find_uuid_in(s: &str) -> Option<Uuid> {
    let uuid_re =
        Regex::new(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
            .expect("Failed to compile UUID regex");

    uuid_re
        .find(s)
        .and_then(|m| Uuid::parse_str(m.as_str()).ok())
}
