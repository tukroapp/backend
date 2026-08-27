use reqwest::StatusCode;
use serde_json::json;

use crate::helpers::{drop_app, spawn_app};

async fn login_extra_session(app: &crate::helpers::TestApp) -> String {
    let body: serde_json::Value = app
        .post_login(&json!({
            "email": &app.test_user.email,
            "password": &app.test_user.password
        }))
        .await
        .json()
        .await
        .expect("login response is not JSON");

    body["results"]["token"]
        .as_str()
        .expect("token not found")
        .to_string()
}

async fn session_status(app: &crate::helpers::TestApp, token: &str) -> StatusCode {
    app.api_client
        .post(format!(
            "{}/api/v1/auth/send-verification-email",
            &app.address
        ))
        .header(reqwest::header::AUTHORIZATION, format!("Token {}", token))
        .send()
        .await
        .expect("Failed to send request")
        .status()
}

#[tokio::test]
async fn logout_all_revokes_every_session() {
    // Arrange
    let app = spawn_app().await;
    app.test_user.login(&app).await;
    let other_token = login_extra_session(&app).await;

    // Act
    let response = app
        .api_client
        .post(format!("{}/api/v1/auth/logout-all", &app.address))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .json(&json!({ "password": &app.test_user.password }))
        .send()
        .await
        .expect("Failed to send request");

    let current_status = session_status(&app, &app.test_user.token()).await;
    let other_status = session_status(&app, &other_token).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert!(response.status().is_success());
    assert_eq!(current_status, StatusCode::UNAUTHORIZED);
    assert_eq!(other_status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn logout_all_fails_with_wrong_password() {
    // Arrange
    let app = spawn_app().await;
    app.test_user.login(&app).await;

    // Act
    let response = app
        .api_client
        .post(format!("{}/api/v1/auth/logout-all", &app.address))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .json(&json!({ "password": "wrongpassword" }))
        .send()
        .await
        .expect("Failed to send request");

    let current_status = session_status(&app, &app.test_user.token()).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_ne!(current_status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn logout_all_fails_without_authentication() {
    // Arrange
    let app = spawn_app().await;

    // Act
    let response = app
        .api_client
        .post(format!("{}/api/v1/auth/logout-all", &app.address))
        .json(&json!({ "password": "whatever" }))
        .send()
        .await
        .expect("Failed to send request");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn logout_works() {
    // Arrange
    let app = spawn_app().await;
    app.test_user.login(&app).await;

    // Act
    let logout_response = app
        .api_client
        .post(format!("{}/api/v1/auth/logout", &app.address))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .send()
        .await
        .expect("Failed to send request");

    // Tear down
    drop_app(app).await;

    // Assert
    assert!(logout_response.status().is_success());
}

#[tokio::test]
async fn logout_fails_without_authentication() {
    // Arrange
    let app = spawn_app().await;

    // Act
    let response = app
        .api_client
        .post(format!("{}/api/v1/auth/logout", &app.address))
        .send()
        .await
        .expect("Failed to send request");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn double_logout_fails() {
    // Arrange
    let app = spawn_app().await;
    app.test_user.login(&app).await;

    // Act
    let logout_response = app
        .api_client
        .post(format!("{}/api/v1/auth/logout", &app.address))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .send()
        .await
        .expect("Failed to send request");

    let second_logout_response = app
        .api_client
        .post(format!("{}/api/v1/auth/logout", &app.address))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .send()
        .await
        .expect("Failed to send request");

    // Tear down
    drop_app(app).await;

    // Assert
    assert!(logout_response.status().is_success());
    assert_eq!(second_logout_response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn logout_fails_with_invalid_session() {
    // Arrange
    let app = spawn_app().await;
    app.test_user.login(&app).await;

    // Act
    let response = app
        .api_client
        .post(format!("{}/api/v1/auth/logout", &app.address))
        .header(reqwest::header::AUTHORIZATION, "Token invalid_token")
        .send()
        .await
        .expect("Failed to send request");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
