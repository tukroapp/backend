use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

use tukro_backend::services::{account::anonymize_account, storage::LocalStorage};

use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};

/// Cadastra e autentica um usuário guardando a senha, que os endpoints de saída
/// exigem de volta.
struct Account {
    user: RegisteredUser,
    password: String,
}

const PASSWORD: &str = "supersecret123";

async fn register(app: &TestApp, name: &str) -> Account {
    Account {
        user: register_and_login(app, name).await,
        password: PASSWORD.to_string(),
    }
}

async fn json_of(response: reqwest::Response) -> Value {
    response.json().await.expect("response not JSON")
}

async fn deactivate(app: &TestApp, account: &Account, password: &str) -> reqwest::Response {
    app.api_client
        .delete(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, account.user.auth_header())
        .json(&json!({ "password": password }))
        .send()
        .await
        .expect("Failed to deactivate account")
}

async fn schedule_deletion(app: &TestApp, account: &Account) -> reqwest::Response {
    app.api_client
        .post(format!("{}/api/v1/me/deletion", &app.address))
        .header(reqwest::header::AUTHORIZATION, account.user.auth_header())
        .json(&json!({ "password": account.password }))
        .send()
        .await
        .expect("Failed to schedule deletion")
}

async fn login(app: &TestApp, account: &Account) -> reqwest::Response {
    app.post_login(&json!({
        "email": account.user.email,
        "password": account.password,
    }))
    .await
}

async fn account_row(app: &TestApp, slug: &str) -> (String, Option<chrono::DateTime<chrono::Utc>>) {
    let row = sqlx::query!(
        r#"
        SELECT au.status::TEXT AS "status!", au.deletion_scheduled_at
        FROM auth_user au
        INNER JOIN user_profile up ON up.user_id = au.id
        WHERE up.slug = $1
        "#,
        slug,
    )
    .fetch_one(&app.db_pool)
    .await
    .expect("conta não encontrada");

    (row.status, row.deletion_scheduled_at)
}

async fn user_id_of(app: &TestApp, slug: &str) -> Uuid {
    sqlx::query_scalar!(r#"SELECT user_id FROM user_profile WHERE slug = $1"#, slug)
        .fetch_one(&app.db_pool)
        .await
        .expect("perfil não encontrado")
}

fn storage() -> LocalStorage {
    LocalStorage::new("./uploads", "http://localhost/media")
}

async fn open_wall(app: &TestApp, owner: &Account) {
    let response = app
        .api_client
        .put(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, owner.user.auth_header())
        .json(&json!({
            "name": "Dono",
            "scrap_write": "everyone",
            "message_receive": "everyone",
        }))
        .send()
        .await
        .expect("Failed to open wall");
    assert_eq!(response.status(), StatusCode::OK);
}

async fn post_scrap(app: &TestApp, author: &Account, wall: &str, body: &str) {
    let response = app
        .api_client
        .post(format!("{}/api/v1/profile/{}/scrap", &app.address, wall))
        .header(reqwest::header::AUTHORIZATION, author.user.auth_header())
        .json(&json!({ "body": body }))
        .send()
        .await
        .expect("Failed to post scrap");
    assert_eq!(response.status(), StatusCode::CREATED);
}

async fn wall_of(app: &TestApp, slug: &str) -> Value {
    app.api_client
        .get(format!("{}/api/v1/profile/{}/scrap", &app.address, slug))
        .send()
        .await
        .expect("Failed to read wall")
        .json()
        .await
        .expect("wall not JSON")
}

async fn create_community(app: &TestApp, owner: &Account, name: &str) -> String {
    let response = app
        .api_client
        .post(format!("{}/api/v1/community", &app.address))
        .header(reqwest::header::AUTHORIZATION, owner.user.auth_header())
        .json(&json!({ "name": name, "is_private": false }))
        .send()
        .await
        .expect("Failed to create community");

    json_of(response).await["results"]["slug"]
        .as_str()
        .expect("slug not found")
        .to_string()
}

async fn join(app: &TestApp, user: &Account, slug: &str) {
    let response = app
        .api_client
        .post(format!("{}/api/v1/community/{}/member", &app.address, slug))
        .header(reqwest::header::AUTHORIZATION, user.user.auth_header())
        .send()
        .await
        .expect("Failed to join community");
    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn deactivating_hides_the_profile_and_drops_the_sessions() {
    // Arrange
    let app = spawn_app().await;
    let alice = register(&app, "Alice").await;

    // Act
    let response = deactivate(&app, &alice, PASSWORD).await;

    let profile = app
        .api_client
        .get(format!(
            "{}/api/v1/profile/{}",
            &app.address, alice.user.slug
        ))
        .send()
        .await
        .expect("Failed to read profile");

    // A sessão que ela tinha aberta não vale mais
    let me = app
        .api_client
        .get(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, alice.user.auth_header())
        .send()
        .await
        .expect("Failed to read me");

    let (status, scheduled) = account_row(&app, &alice.user.slug).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(profile.status(), StatusCode::NOT_FOUND);
    assert_eq!(me.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(status, "deactivated");
    // Desativar não agenda exclusão nenhuma
    assert!(scheduled.is_none());
}

#[tokio::test]
async fn logging_in_again_brings_the_account_back() {
    // Arrange
    let app = spawn_app().await;
    let alice = register(&app, "Alice").await;
    assert_eq!(
        deactivate(&app, &alice, PASSWORD).await.status(),
        StatusCode::NO_CONTENT
    );

    // Act
    let response = login(&app, &alice).await;
    let status = response.status();

    let profile = app
        .api_client
        .get(format!(
            "{}/api/v1/profile/{}",
            &app.address, alice.user.slug
        ))
        .send()
        .await
        .expect("Failed to read profile");

    let (account_status, _) = account_row(&app, &alice.user.slug).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::OK);
    assert_eq!(profile.status(), StatusCode::OK);
    assert_eq!(account_status, "active");
}

#[tokio::test]
async fn closing_the_account_requires_the_right_password() {
    // Arrange
    let app = spawn_app().await;
    let alice = register(&app, "Alice").await;

    // Act
    let wrong = deactivate(&app, &alice, "senha-errada").await;
    let empty = app
        .api_client
        .delete(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, alice.user.auth_header())
        .json(&json!({}))
        .send()
        .await
        .expect("Failed to deactivate");
    let anonymous = app
        .api_client
        .delete(format!("{}/api/v1/me", &app.address))
        .json(&json!({ "password": PASSWORD }))
        .send()
        .await
        .expect("Failed to deactivate");

    let (status, _) = account_row(&app, &alice.user.slug).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(wrong.status(), StatusCode::BAD_REQUEST);
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(status, "active");
}

#[tokio::test]
async fn asking_for_deletion_schedules_it_and_logging_in_cancels() {
    // Arrange
    let app = spawn_app().await;
    let alice = register(&app, "Alice").await;

    // Act
    let response = schedule_deletion(&app, &alice).await;
    let status = response.status();
    let body = json_of(response).await;

    let (account_status, scheduled) = account_row(&app, &alice.user.slug).await;

    // Entrar de novo dentro do prazo desfaz o pedido
    assert_eq!(login(&app, &alice).await.status(), StatusCode::OK);
    let (after_login, scheduled_after_login) = account_row(&app, &alice.user.slug).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::OK);
    assert!(body["results"]["scheduled_for"].is_string());
    assert_eq!(account_status, "deactivated");

    let scheduled = scheduled.expect("a exclusão não foi agendada");
    let days = (scheduled - chrono::Utc::now()).num_days();
    assert!((28..=30).contains(&days), "prazo inesperado: {days} dias");

    assert_eq!(after_login, "active");
    assert!(scheduled_after_login.is_none());
}

#[tokio::test]
async fn the_deletion_anonymizes_the_account_and_keeps_the_public_content() {
    // Arrange
    let app = spawn_app().await;
    let alice = register(&app, "Alice").await;
    let bob = register(&app, "Bob").await;

    open_wall(&app, &bob).await;
    post_scrap(&app, &alice, &bob.user.slug, "recado da alice").await;

    // Uma conversa privada, que some junto com a conta
    let response = app
        .api_client
        .post(format!(
            "{}/api/v1/profile/{}/message",
            &app.address, bob.user.slug
        ))
        .header(reqwest::header::AUTHORIZATION, alice.user.auth_header())
        .json(&json!({ "body": "oi bob" }))
        .send()
        .await
        .expect("Failed to send message");
    assert_eq!(response.status(), StatusCode::CREATED);

    let alice_id = user_id_of(&app, &alice.user.slug).await;

    // Act
    anonymize_account(&app.db_pool, &app.redis_pool, &storage(), alice_id)
        .await
        .expect("anonymize failed");

    let wall = wall_of(&app, &bob.user.slug).await;

    let account = sqlx::query!(
        r#"
        SELECT
            au.status::TEXT AS "status!",
            au.email::TEXT AS "email!",
            au.password_hash,
            up.slug,
            up.name,
            up.bio,
            up.city
        FROM auth_user au
        INNER JOIN user_profile up ON up.user_id = au.id
        WHERE au.id = $1
        "#,
        alice_id,
    )
    .fetch_one(&app.db_pool)
    .await
    .expect("conta sumiu do banco");

    let conversations = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM conversation WHERE user_lo = $1 OR user_hi = $1"#,
        alice_id,
    )
    .fetch_one(&app.db_pool)
    .await
    .expect("Failed to count conversations");

    let login_again = login(&app, &alice).await;

    // Tear down
    drop_app(app).await;

    // Assert
    // O recado continua no mural do bob, agora assinado pelo perfil anonimizado
    let results = wall["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["body"], "recado da alice");
    assert_eq!(results[0]["author"]["name"], "Usuário excluído");

    assert_eq!(account.status, "deleted");
    assert!(account.email.ends_with("@tukro.invalid"));
    assert!(account.password_hash.is_none());
    assert!(account.slug.starts_with("deleted-"));
    assert_eq!(account.name, "Usuário excluído");
    assert!(account.bio.is_none());
    assert!(account.city.is_none());

    assert_eq!(conversations, 0, "a conversa privada tinha que sumir");

    // Não há mais como entrar: o e-mail antigo não existe mais
    assert_eq!(login_again.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_deletion_hands_the_community_over_to_the_oldest_admin() {
    // Arrange
    let app = spawn_app().await;
    let owner = register(&app, "Dono").await;
    let member = register(&app, "Membro").await;
    let admin = register(&app, "Moderador").await;

    let slug = create_community(&app, &owner, "Comunidade").await;
    join(&app, &member, &slug).await;
    join(&app, &admin, &slug).await;

    let response = app
        .api_client
        .put(format!(
            "{}/api/v1/community/{}/member/{}/role",
            &app.address, slug, admin.user.slug
        ))
        .header(reqwest::header::AUTHORIZATION, owner.user.auth_header())
        .json(&json!({ "role": "admin" }))
        .send()
        .await
        .expect("Failed to promote member");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let owner_id = user_id_of(&app, &owner.user.slug).await;

    // Act
    anonymize_account(&app.db_pool, &app.redis_pool, &storage(), owner_id)
        .await
        .expect("anonymize failed");

    let new_owner = sqlx::query_scalar!(
        r#"
        SELECT up.slug
        FROM community_member cm
        INNER JOIN user_profile up ON up.user_id = cm.user_id
        WHERE cm.community_id = (SELECT id FROM community WHERE slug = $1)
          AND cm.role = 'owner'
        "#,
        slug,
    )
    .fetch_optional(&app.db_pool)
    .await
    .expect("Failed to read owner");

    let deleted_at =
        sqlx::query_scalar!(r#"SELECT deleted_at FROM community WHERE slug = $1"#, slug,)
            .fetch_one(&app.db_pool)
            .await
            .expect("comunidade sumiu");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(new_owner.as_deref(), Some(admin.user.slug.as_str()));
    assert!(
        deleted_at.is_none(),
        "a comunidade tinha que continuar no ar"
    );
}

#[tokio::test]
async fn a_community_with_nobody_left_goes_down_with_the_owner() {
    // Arrange
    let app = spawn_app().await;
    let owner = register(&app, "Dono").await;
    let slug = create_community(&app, &owner, "Comunidade Solitária").await;
    let owner_id = user_id_of(&app, &owner.user.slug).await;

    // Act
    anonymize_account(&app.db_pool, &app.redis_pool, &storage(), owner_id)
        .await
        .expect("anonymize failed");

    let deleted_at =
        sqlx::query_scalar!(r#"SELECT deleted_at FROM community WHERE slug = $1"#, slug,)
            .fetch_one(&app.db_pool)
            .await
            .expect("comunidade sumiu");

    let listing = app
        .api_client
        .get(format!("{}/api/v1/community", &app.address))
        .send()
        .await
        .expect("Failed to list communities");
    let listing = json_of(listing).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert!(deleted_at.is_some());
    assert!(listing["results"].as_array().unwrap().is_empty());
}
