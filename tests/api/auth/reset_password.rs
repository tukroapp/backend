use crate::helpers::{TestApp, drop_app, find_uuid_in, spawn_app};
use reqwest::StatusCode;
use serde_json::json;
use tukro_backend::utils::compute_sha_hash;
use uuid::Uuid;

async fn request_password_reset_token(app: &TestApp) -> Uuid {
    // Act
    let response = app
        .api_client
        .post(format!("{}/api/v1/auth/forgot-password", &app.address))
        .json(&json!({ "email": &app.test_user.email }))
        .send()
        .await
        .expect("Failed to send forgot-password request");

    // Assert
    assert!(
        response.status().is_success(),
        "Forgot password request failed"
    );

    // Act
    let sent_email = sqlx::query!(
        r#"
        SELECT html_content
        FROM email_queue
        WHERE to_address = $1
        "#,
        &[app.test_user.email.clone()]
    )
    .fetch_one(&app.db_pool)
    .await
    .expect("Failed to fetch sent email");

    find_uuid_in(&sent_email.html_content).expect("No UUID (token) found in email content")
}

#[tokio::test]
async fn forgot_password_check_with_valid_token_works() {
    // Arrange
    let app = spawn_app().await;

    // Act
    let token_sent = request_password_reset_token(&app).await;

    let response = app
        .api_client
        .get(format!(
            "{}/api/v1/auth/reset-password?token={}",
            &app.address, token_sent
        ))
        .send()
        .await
        .expect("Failed to send check-token request");

    // Tear down
    drop_app(app).await;

    // Assert
    assert!(response.status().is_success(), "Check token request failed");
}

#[tokio::test]
async fn forgot_password_check_with_wrong_token_fails() {
    // Arrange
    let app = spawn_app().await;

    // Act
    let random_token = Uuid::new_v4();

    let response = app
        .api_client
        .get(format!(
            "{}/api/v1/auth/reset-password?token={}",
            &app.address, random_token
        ))
        .send()
        .await
        .expect("Failed to send check-token request");

    // Tear down
    drop_app(app).await;

    // Assert
    assert!(
        response.status().is_client_error(),
        "Check token request did not fail"
    );

    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("Token inválido ou expirado"),
        "The API did not return 400 Bad Request for invalid token"
    );
}

#[tokio::test]
async fn forgot_password_check_without_token_fails() {
    // Arrange
    let app = spawn_app().await;

    let response = app
        .api_client
        .get(format!("{}/api/v1/auth/reset-password", &app.address))
        .send()
        .await
        .expect("Failed to send check-token request");

    // Tear down
    drop_app(app).await;

    // Assert
    assert!(
        response.status().is_client_error(),
        "Check token request did not fail"
    );
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("Token inválido ou expirado"),
        "The API did not return 400 Bad Request for missing token"
    );
}

#[tokio::test]
async fn forgot_password_sends_email_and_stores_token() {
    // Arrange
    let app = spawn_app().await;

    // Act
    let token_sent = request_password_reset_token(&app).await;

    // Assert
    let token_hash_sent = compute_sha_hash(&token_sent.to_string());

    let token_stored = sqlx::query!(
        r#"
        SELECT token_hash
        FROM verification_token
        WHERE user_id = $1 AND kind = 'password_reset'
        "#,
        app.test_user.id
    )
    .fetch_one(&app.db_pool)
    .await
    .expect("Failed to fetch password reset token");

    // Tear down
    drop_app(app).await;

    assert_eq!(token_stored.token_hash, token_hash_sent);
}

#[tokio::test]
async fn reset_password_allows_login_with_new_password() {
    // Arrange
    let app = spawn_app().await;
    let token_sent = request_password_reset_token(&app).await;
    let new_password = "newpassword123";

    // Act 1
    let response_reset = app
        .api_client
        .post(format!("{}/api/v1/auth/reset-password", &app.address))
        .json(&json!({
            "token": token_sent,
            "new_password": new_password,
            "new_password_confirmation": new_password
        }))
        .send()
        .await
        .expect("Failed to send reset-password request");

    // Assert 1
    assert!(
        response_reset.status().is_success(),
        "Reset password request failed"
    );

    // Act 2
    let response_login = app
        .api_client
        .post(format!("{}/api/v1/auth/login", &app.address))
        .json(&json!({
            "email": &app.test_user.email,
            "password": new_password // Usa a nova senha
        }))
        .send()
        .await
        .expect("Failed to send login request");

    // Tear down
    drop_app(app).await;

    // Assert 2
    assert!(
        response_login.status().is_success(),
        "Login with new password failed"
    );
}

#[tokio::test]
async fn reset_password_fails_with_mismatched_passwords() {
    // Arrange
    let app = spawn_app().await;
    let token = request_password_reset_token(&app).await;

    // Act
    let response = app
        .api_client
        .post(format!("{}/api/v1/auth/reset-password", &app.address))
        .json(&json!({
            "token": token,
            "new_password": "newpassword123",
            "new_password_confirmation": "DIFFERENTpassword456"
        }))
        .send()
        .await
        .expect("Failed to send request");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(
        StatusCode::BAD_REQUEST,
        response.status(),
        "The API did not return 400 Bad Request for mismatched passwords"
    );
}

#[tokio::test]
async fn reset_password_fails_with_invalid_token() {
    // Arrange
    let app = spawn_app().await;
    let invalid_token = Uuid::nil();

    // Act
    let response = app
        .api_client
        .post(format!("{}/api/v1/auth/reset-password", &app.address))
        .json(&json!({
            "token": invalid_token,
            "new_password": "newpassword123",
            "new_password_confirmation": "newpassword123"
        }))
        .send()
        .await
        .expect("Failed to send request");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(
        StatusCode::BAD_REQUEST,
        response.status(),
        "The API did not return 400 Bad Request for invalid token"
    );
}

#[tokio::test]
async fn login_with_old_password_fails_after_reset() {
    // Arrange
    let app = spawn_app().await;
    let new_password = "a-brand-new-password";
    let old_password = app.test_user.password.clone();

    // Act 1
    let token = request_password_reset_token(&app).await;
    let response_reset = app
        .api_client
        .post(format!("{}/api/v1/auth/reset-password", &app.address))
        .json(&json!({
            "token": token,
            "new_password": new_password,
            "new_password_confirmation": new_password
        }))
        .send()
        .await
        .expect("Failed to send reset-password request");

    // Assert 1
    assert!(
        response_reset.status().is_success(),
        "Password reset (setup) failed"
    );

    // Act 2
    let response_login = app
        .api_client
        .post(format!("{}/api/v1/auth/login", &app.address))
        .json(&json!({
            "email": &app.test_user.email,
            "password": old_password // Usando a senha antiga
        }))
        .send()
        .await
        .expect("Failed to send login request");

    // Tear down
    drop_app(app).await;

    // Assert 2
    assert_eq!(
        StatusCode::UNAUTHORIZED,
        response_login.status(),
        "Login with old password did not return 401 Unauthorized"
    );
}

#[tokio::test]
async fn reset_password_revokes_all_sessions() {
    // Arrange
    let app = spawn_app().await;
    app.test_user.login(&app).await;

    // Act
    let token = request_password_reset_token(&app).await;
    let response_reset = app
        .api_client
        .post(format!("{}/api/v1/auth/reset-password", &app.address))
        .json(&json!({
            "token": token,
            "new_password": "a-brand-new-password",
            "new_password_confirmation": "a-brand-new-password"
        }))
        .send()
        .await
        .expect("Failed to send reset-password request");

    // A sessão que existia antes do reset deixou de ser válida
    let session_after_reset = app
        .api_client
        .post(format!(
            "{}/api/v1/auth/send-verification-email",
            &app.address
        ))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .send()
        .await
        .expect("Failed to send request");

    // Tear down
    drop_app(app).await;

    // Assert
    assert!(response_reset.status().is_success());
    assert_eq!(session_after_reset.status(), StatusCode::UNAUTHORIZED);
}
