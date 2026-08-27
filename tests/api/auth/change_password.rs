use reqwest::StatusCode;
use serde_json::json;

use crate::helpers::{drop_app, spawn_app};

#[tokio::test]
async fn change_password_keeps_current_session_and_revokes_others() {
    // Arrange
    let app = spawn_app().await;

    // Sessão atual (usada para trocar a senha)
    app.test_user.login(&app).await;

    // Uma segunda sessão do mesmo usuário (outro dispositivo)
    let other_login: serde_json::Value = app
        .post_login(&json!({
            "email": &app.test_user.email,
            "password": &app.test_user.password
        }))
        .await
        .json()
        .await
        .expect("login response is not JSON");
    let other_token = other_login["results"]["token"]
        .as_str()
        .expect("token not found")
        .to_string();

    // Act
    let change_response = app
        .api_client
        .post(format!("{}/api/v1/auth/change-password", &app.address))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .json(&json!({
            "old_password": &app.test_user.password,
            "new_password": "newpassword123",
            "new_password_confirmation": "newpassword123"
        }))
        .send()
        .await
        .expect("Failed to send request");

    // A sessão atual continua válida, a outra foi revogada
    let current_session = app
        .api_client
        .post(format!(
            "{}/api/v1/auth/send-verification-email",
            &app.address
        ))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .send()
        .await
        .expect("Failed to send request");

    let other_session = app
        .api_client
        .post(format!(
            "{}/api/v1/auth/send-verification-email",
            &app.address
        ))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Token {}", other_token),
        )
        .send()
        .await
        .expect("Failed to send request");

    // Tear down
    drop_app(app).await;

    // Assert
    assert!(change_response.status().is_success());
    assert_ne!(current_session.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(other_session.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn change_password_works() {
    // Arrange
    let app = spawn_app().await;
    app.test_user.login(&app).await;

    let response = app
        .api_client
        .post(format!("{}/api/v1/auth/change-password", &app.address))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .json(&json!({
            "old_password": &app.test_user.password,
            "new_password": "newpassword123",
            "new_password_confirmation": "newpassword123"
        }))
        .send()
        .await
        .expect("Failed to send request");

    // Tear down
    drop_app(app).await;

    // Assert
    assert!(response.status().is_success());
}

#[tokio::test]
async fn change_password_fails_with_wrong_old_password() {
    // Arrange
    let app = spawn_app().await;
    app.test_user.login(&app).await;

    let response = app
        .api_client
        .post(format!("{}/api/v1/auth/change-password", &app.address))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .json(&json!({
            "old_password": "wrongoldpassword",
            "new_password": "newpassword123",
            "new_password_confirmation": "newpassword123"
        }))
        .send()
        .await
        .expect("Failed to send request");

    let response_status = response.status();
    let response_text = response.text().await.expect("Failed to read response text");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response_status, 400);
    assert!(response_text.contains("A senha antiga está incorreta"));
}

#[tokio::test]
async fn change_password_fails_with_mismatched_confirmation() {
    // Arrange
    let app = spawn_app().await;
    app.test_user.login(&app).await;

    let response = app
        .api_client
        .post(format!("{}/api/v1/auth/change-password", &app.address))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .json(&json!({
            "old_password": &app.test_user.password,
            "new_password": "newpassword123",
            "new_password_confirmation": "differentconfirmation"
        }))
        .send()
        .await
        .expect("Failed to send request");

    let response_status = response.status();
    let response_text = response.text().await.expect("Failed to read response text");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response_status, 400);
    assert!(response_text.contains("As senhas não coincidem"));
}

#[tokio::test]
async fn change_password_fails_without_authentication() {
    // Arrange
    let app = spawn_app().await;

    let response = app
        .api_client
        .post(format!("{}/api/v1/auth/change-password", &app.address))
        .header(reqwest::header::AUTHORIZATION, "Token invalid_token")
        .json(&json!({
            "old_password": "someoldpassword",
            "new_password": "newpassword123",
            "new_password_confirmation": "newpassword123"
        }))
        .send()
        .await
        .expect("Failed to send request");

    let response_status = response.status();

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response_status, 401);
}
