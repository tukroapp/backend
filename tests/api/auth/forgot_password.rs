use reqwest::StatusCode;
use serde_json::json;

use crate::helpers::{drop_app, spawn_app};

#[tokio::test]
async fn forgot_password_works() {
    // Arrange
    let app = spawn_app().await;

    // Act
    let response = app
        .api_client
        .post(format!("{}/api/v1/auth/forgot-password", &app.address))
        .json(&json!({
            "email": &app.test_user.email,
        }))
        .send()
        .await
        .expect("Failed to send request");

    let sent_email = sqlx::query!(
        r#"
        SELECT html_content
        FROM email_queue
        WHERE to_address = $1
        "#,
        &[app.test_user.email.clone()]
    )
    .fetch_optional(&app.db_pool)
    .await
    .expect("Failed to fetch sent email");

    let token = sqlx::query!(
        r#"
        SELECT token_hash
        FROM verification_token
        WHERE user_id = $1 AND kind = 'password_reset'
        "#,
        app.test_user.id
    )
    .fetch_optional(&app.db_pool)
    .await
    .expect("Failed to fetch password reset token");

    // Tear down
    drop_app(app).await;

    // Assert
    assert!(response.status().is_success());
    assert!(sent_email.is_some());
    assert!(token.is_some());
}

#[tokio::test]
async fn forgot_password_with_unknown_email_works() {
    // Arrange
    let app = spawn_app().await;
    let unknown_email = "unknown@test.com";

    // Act
    let response = app
        .api_client
        .post(format!("{}/api/v1/auth/forgot-password", &app.address))
        .json(&json!({
            "email": unknown_email,
        }))
        .send()
        .await
        .expect("Failed to send request");

    let sent_email = sqlx::query!(
        r#"
        SELECT html_content
        FROM email_queue
        WHERE to_address = $1
        "#,
        &[unknown_email.to_string()]
    )
    .fetch_optional(&app.db_pool)
    .await
    .expect("Failed to fetch sent email");

    let token = sqlx::query!(
        r#"
        SELECT token_hash
        FROM verification_token
        WHERE kind = 'password_reset'
        "#,
    )
    .fetch_optional(&app.db_pool)
    .await
    .expect("Failed to fetch password reset token");

    // Tear down
    drop_app(app).await;

    // Assert
    assert!(response.status().is_success());
    assert!(sent_email.is_none());
    assert!(token.is_none());
}

#[tokio::test]
async fn forgot_password_with_invalid_email_format_fails() {
    // Arrange
    let app = spawn_app().await;
    let invalid_email = "invalid-email-format";

    // Act
    let response = app
        .api_client
        .post(format!("{}/api/v1/auth/forgot-password", &app.address))
        .json(&json!({
            "email": invalid_email,
        }))
        .send()
        .await
        .expect("Failed to send request");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status().as_u16(), StatusCode::BAD_REQUEST.as_u16());
}

#[tokio::test]
async fn forgot_password_with_missing_email_fails() {
    // Arrange
    let app = spawn_app().await;

    // Act
    let response = app
        .api_client
        .post(format!("{}/api/v1/auth/forgot-password", &app.address))
        .json(&json!({}))
        .send()
        .await
        .expect("Failed to send request");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status().as_u16(), StatusCode::BAD_REQUEST.as_u16());
}
