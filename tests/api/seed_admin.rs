use reqwest::StatusCode;
use serde_json::json;

use crate::helpers::{drop_app, spawn_app};

/// A senha que `spawn_app` coloca em `ADMIN_PASSWORD_FIRST_ACCESS`.
const SEED_PASSWORD: &str = "primeiro-acesso-do-teste";

#[tokio::test]
async fn the_first_boot_creates_the_admin_from_the_environment() {
    // Arrange
    let app = spawn_app().await;

    // Act
    let response = app
        .post_login(&json!({ "email": "admin@tukro.app", "password": SEED_PASSWORD }))
        .await;
    let status = response.status();
    let body: serde_json::Value = response.json().await.expect("login not JSON");
    let token = body["results"]["token"]
        .as_str()
        .expect("token not found")
        .to_string();

    // O admin do seed manda no painel de moderação
    let queue = app
        .api_client
        .get(format!("{}/api/v1/admin/report", &app.address))
        .header(reqwest::header::AUTHORIZATION, format!("Token {token}"))
        .send()
        .await
        .expect("Failed to list the queue");

    let admins =
        sqlx::query_scalar!(r#"SELECT COUNT(*) AS "count!" FROM auth_user WHERE is_superuser"#)
            .fetch_one(&app.db_pool)
            .await
            .expect("Failed to count superusers");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::OK);
    assert_eq!(queue.status(), StatusCode::OK);
    assert_eq!(admins, 1);
}

#[tokio::test]
async fn a_second_boot_does_not_create_another_admin() {
    // Arrange
    let app = spawn_app().await;

    // Act
    // Sobe a aplicação de novo contra o mesmo banco: o seed tem que se conter.
    app.boot_again().await;

    let admins =
        sqlx::query_scalar!(r#"SELECT COUNT(*) AS "count!" FROM auth_user WHERE is_superuser"#)
            .fetch_one(&app.db_pool)
            .await
            .expect("Failed to count superusers");

    // A senha do primeiro acesso continua valendo porque ninguém a trocou ainda
    let login = app
        .post_login(&json!({ "email": "admin@tukro.app", "password": SEED_PASSWORD }))
        .await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(admins, 1, "o segundo boot criou outro admin");
    assert_eq!(login.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_admin_password_can_be_changed_on_the_first_login() {
    // Arrange
    let app = spawn_app().await;

    let response = app
        .post_login(&json!({ "email": "admin@tukro.app", "password": SEED_PASSWORD }))
        .await;
    let body: serde_json::Value = response.json().await.expect("login not JSON");
    let token = body["results"]["token"]
        .as_str()
        .expect("token not found")
        .to_string();

    // Act
    let changed = app
        .api_client
        .post(format!("{}/api/v1/auth/change-password", &app.address))
        .header(reqwest::header::AUTHORIZATION, format!("Token {token}"))
        .json(&json!({
            "old_password": SEED_PASSWORD,
            "new_password": "uma-senha-bem-mais-longa",
            "new_password_confirmation": "uma-senha-bem-mais-longa",
        }))
        .send()
        .await
        .expect("Failed to change password");
    let changed_status = changed.status();

    let with_old = app
        .post_login(&json!({ "email": "admin@tukro.app", "password": SEED_PASSWORD }))
        .await;
    let with_new = app
        .post_login(&json!({ "email": "admin@tukro.app", "password": "uma-senha-bem-mais-longa" }))
        .await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(changed_status, StatusCode::NO_CONTENT);
    assert_eq!(with_old.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(with_new.status(), StatusCode::OK);
}
