use reqwest::StatusCode;
use serde_json::json;

use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};

/// A senha que `register_and_login` usa, para os testes conseguirem tentar um
/// segundo login depois da suspensão.
const PASSWORD: &str = "supersecret123";

async fn promote(app: &TestApp, user: &RegisteredUser) {
    sqlx::query!(
        r#"
        UPDATE auth_user SET is_superuser = TRUE
        WHERE id = (SELECT user_id FROM user_profile WHERE slug = $1)
        "#,
        user.slug,
    )
    .execute(&app.db_pool)
    .await
    .expect("Failed to promote user");
}

async fn suspend(
    app: &TestApp,
    admin: &RegisteredUser,
    slug: &str,
    body: serde_json::Value,
) -> reqwest::Response {
    app.api_client
        .post(format!(
            "{}/api/v1/admin/user/{}/suspend",
            &app.address, slug
        ))
        .header(reqwest::header::AUTHORIZATION, admin.auth_header())
        .json(&body)
        .send()
        .await
        .expect("Failed to suspend")
}

async fn unsuspend(app: &TestApp, admin: &RegisteredUser, slug: &str) -> reqwest::Response {
    app.api_client
        .delete(format!(
            "{}/api/v1/admin/user/{}/suspend",
            &app.address, slug
        ))
        .header(reqwest::header::AUTHORIZATION, admin.auth_header())
        .send()
        .await
        .expect("Failed to unsuspend")
}

async fn login(app: &TestApp, user: &RegisteredUser) -> reqwest::Response {
    app.post_login(&json!({ "email": user.email, "password": PASSWORD }))
        .await
}

async fn get_me(app: &TestApp, user: &RegisteredUser) -> reqwest::Response {
    app.api_client
        .get(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to get me")
}

#[tokio::test]
async fn suspended_user_cannot_login() {
    // Arrange
    let app = spawn_app().await;
    let admin = register_and_login(&app, "Moderador").await;
    promote(&app, &admin).await;
    let bob = register_and_login(&app, "Bob Suspenso").await;

    // Act
    let suspended = suspend(&app, &admin, &bob.slug, json!({ "reason": "spam" })).await;
    let response = login(&app, &bob).await;
    let status = response.status();
    let body: serde_json::Value = response.json().await.expect("login response not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(suspended.status(), StatusCode::NO_CONTENT);
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["account"][0]["code"], "account_suspended");
}

#[tokio::test]
async fn suspending_drops_the_open_session() {
    // Arrange
    let app = spawn_app().await;
    let admin = register_and_login(&app, "Moderador").await;
    promote(&app, &admin).await;
    let bob = register_and_login(&app, "Bob Logado").await;

    let before = get_me(&app, &bob).await.status();

    // Act
    suspend(&app, &admin, &bob.slug, json!({ "reason": "spam" })).await;
    let after = get_me(&app, &bob).await.status();

    // Tear down
    drop_app(app).await;

    // Assert: o token não sobrevive à suspensão, sem esperar o TTL da sessão
    assert_eq!(before, StatusCode::OK);
    assert_eq!(after, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn scraps_from_suspended_user_disappear_from_the_wall() {
    // Arrange
    let app = spawn_app().await;
    let admin = register_and_login(&app, "Moderador").await;
    promote(&app, &admin).await;
    let alice = register_and_login(&app, "Alice Dona").await;
    let bob = register_and_login(&app, "Bob Autor").await;

    let opened = app
        .api_client
        .put(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .json(&json!({ "name": "Alice Dona", "scrap_write": "everyone" }))
        .send()
        .await
        .expect("Failed to open wall");
    assert_eq!(opened.status(), StatusCode::OK);

    let posted = app
        .api_client
        .post(format!(
            "{}/api/v1/profile/{}/scrap",
            &app.address, alice.slug
        ))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .json(&json!({ "body": "compre seguidores" }))
        .send()
        .await
        .expect("Failed to post scrap");
    assert_eq!(posted.status(), StatusCode::CREATED);

    // Act
    suspend(&app, &admin, &bob.slug, json!({ "reason": "spam" })).await;

    let wall: serde_json::Value = app
        .api_client
        .get(format!(
            "{}/api/v1/profile/{}/scrap",
            &app.address, alice.slug
        ))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .send()
        .await
        .expect("Failed to list wall")
        .json()
        .await
        .expect("wall not JSON");

    let profile: serde_json::Value = app
        .api_client
        .get(format!("{}/api/v1/profile/{}", &app.address, alice.slug))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .send()
        .await
        .expect("Failed to get profile")
        .json()
        .await
        .expect("profile not JSON");

    // Tear down
    drop_app(app).await;

    // Assert: some da listagem e da contagem, que precisam bater
    assert!(wall["results"].as_array().unwrap().is_empty());
    assert_eq!(profile["results"]["scraps_count"], 0);
}

#[tokio::test]
async fn unsuspending_brings_the_account_back() {
    // Arrange
    let app = spawn_app().await;
    let admin = register_and_login(&app, "Moderador").await;
    promote(&app, &admin).await;
    let bob = register_and_login(&app, "Bob Perdoado").await;

    suspend(&app, &admin, &bob.slug, json!({ "reason": "spam" })).await;

    // Act
    let revoked = unsuspend(&app, &admin, &bob.slug).await;
    let status = login(&app, &bob).await.status();

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn expired_suspension_is_cleared_on_login() {
    // Arrange
    let app = spawn_app().await;
    let admin = register_and_login(&app, "Moderador").await;
    promote(&app, &admin).await;
    let bob = register_and_login(&app, "Bob Cumpriu").await;

    suspend(&app, &admin, &bob.slug, json!({ "reason": "spam" })).await;

    // O endpoint só aceita prazo no futuro, então o vencimento é forjado aqui.
    sqlx::query!(
        r#"
        UPDATE auth_user SET suspended_until = NOW() - INTERVAL '1 hour'
        WHERE email = $1
        "#,
        bob.email,
    )
    .execute(&app.db_pool)
    .await
    .expect("Failed to expire suspension");

    // Act
    let status = login(&app, &bob).await.status();

    let account_status = sqlx::query_scalar!(
        r#"SELECT status::TEXT AS "status!" FROM auth_user WHERE email = $1"#,
        bob.email,
    )
    .fetch_one(&app.db_pool)
    .await
    .expect("Failed to read status");

    // Tear down
    drop_app(app).await;

    // Assert: o próprio login zera a suspensão vencida
    assert_eq!(status, StatusCode::OK);
    assert_eq!(account_status, "active");
}

#[tokio::test]
async fn suspension_with_past_deadline_is_rejected() {
    // Arrange
    let app = spawn_app().await;
    let admin = register_and_login(&app, "Moderador").await;
    promote(&app, &admin).await;
    let bob = register_and_login(&app, "Bob Qualquer").await;

    // Act
    let response = suspend(
        &app,
        &admin,
        &bob.slug,
        json!({ "reason": "spam", "until": "2020-01-01T00:00:00Z" }),
    )
    .await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_account_cannot_be_suspended() {
    // Arrange
    let app = spawn_app().await;
    let admin = register_and_login(&app, "Moderador").await;
    promote(&app, &admin).await;
    let other_admin = register_and_login(&app, "Outro Moderador").await;
    promote(&app, &other_admin).await;

    // Act
    let response = suspend(&app, &admin, &other_admin.slug, json!({ "reason": "spam" })).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn regular_user_cannot_suspend() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice Comum").await;
    let bob = register_and_login(&app, "Bob Comum").await;

    // Act
    let response = suspend(&app, &alice, &bob.slug, json!({ "reason": "spam" })).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
