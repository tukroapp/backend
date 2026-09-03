use reqwest::StatusCode;
use serde_json::json;

use crate::helpers::{drop_app, spawn_app};

const ADMIN_EMAIL: &str = "admin@tukro.app";
const ADMIN_PASSWORD: &str = "senha-do-administrador";

#[tokio::test]
async fn the_administrator_reaches_the_moderation_panel() {
    // Arrange
    let app = spawn_app().await;
    app.create_admin(ADMIN_EMAIL, ADMIN_PASSWORD).await;

    // Act
    let response = app
        .post_login(&json!({ "email": ADMIN_EMAIL, "password": ADMIN_PASSWORD }))
        .await;
    let status = response.status();
    let body: serde_json::Value = response.json().await.expect("login not JSON");
    let token = body["results"]["token"]
        .as_str()
        .expect("token not found")
        .to_string();

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
async fn the_admin_password_can_be_changed_on_the_first_login() {
    // Arrange
    let app = spawn_app().await;
    app.create_admin(ADMIN_EMAIL, ADMIN_PASSWORD).await;

    let response = app
        .post_login(&json!({ "email": ADMIN_EMAIL, "password": ADMIN_PASSWORD }))
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
            "old_password": ADMIN_PASSWORD,
            "new_password": "uma-senha-bem-mais-longa",
            "new_password_confirmation": "uma-senha-bem-mais-longa",
        }))
        .send()
        .await
        .expect("Failed to change password");
    let changed_status = changed.status();

    let with_old = app
        .post_login(&json!({ "email": ADMIN_EMAIL, "password": ADMIN_PASSWORD }))
        .await;
    let with_new = app
        .post_login(&json!({ "email": ADMIN_EMAIL, "password": "uma-senha-bem-mais-longa" }))
        .await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(changed_status, StatusCode::NO_CONTENT);
    assert_eq!(with_old.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(with_new.status(), StatusCode::OK);
}
