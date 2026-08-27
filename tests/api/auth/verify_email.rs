use reqwest::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::helpers::{drop_app, find_uuid_in, spawn_app};

#[tokio::test]
async fn verify_email_succeeds_with_valid_data() {
    // Arrange
    let app = spawn_app().await;
    app.test_user.login(&app).await;
    app.test_user.send_verification_email(&app).await;

    let saved_email = sqlx::query!(
        "SELECT html_content FROM email_queue WHERE to_address = $1",
        &[app.test_user.email.clone()]
    )
    .fetch_all(&app.db_pool)
    .await
    .expect("Failed to fetch saved email.");

    let uuid_from_email = find_uuid_in(&saved_email[0].html_content.clone()).unwrap();

    let verify_response = app
        .api_client
        .post(format!("{}/api/v1/auth/verify-email", &app.address,))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .json(&json!({ "token": uuid_from_email }))
        .send()
        .await
        .expect("Failed to send request");

    let saved_user = sqlx::query!(
        "SELECT id, email, is_email_verified FROM auth_user WHERE email = $1",
        app.test_user.email
    )
    .fetch_one(&app.db_pool)
    .await
    .expect("Failed to fetch saved user.");

    let is_used = sqlx::query!(
        "SELECT used_at FROM verification_token WHERE kind = 'email_verification' AND user_id = $1",
        saved_user.id
    )
    .fetch_one(&app.db_pool)
    .await
    .expect("Failed to fetch saved token.");

    // Tear down
    drop_app(app).await;

    // Assert
    assert!(is_used.used_at.is_some(), "Token should be marked as used.");
    assert!(saved_user.is_email_verified, "Email should be verified.");
    assert!(
        !saved_email.is_empty(),
        "Expected a confirmation email to be queued."
    );
    assert!(
        saved_email.len() == 1,
        "Expected exactly one confirmation email to be queued."
    );
    assert_eq!(
        verify_response.status().as_u16(),
        StatusCode::NO_CONTENT,
        "Expected 204 No Content on successful email verification."
    );
}

#[tokio::test]
async fn verify_email_fails_without_login() {
    // Arrange
    let app = spawn_app().await;

    let verify_response = app
        .api_client
        .post(format!("{}/api/v1/auth/verify-email", &app.address,))
        .header(reqwest::header::AUTHORIZATION, "Token invalid_token")
        .json(&json!({ "token": Uuid::nil() }))
        .send()
        .await
        .expect("Failed to send request");
    let response_status = verify_response.status();

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(
        response_status,
        StatusCode::UNAUTHORIZED,
        "Expected 401 Unauthorized when verifying email without login."
    );
}

#[tokio::test]
async fn verify_email_fails_with_invalid_token() {
    // Arrange
    let app = spawn_app().await;
    app.test_user.login(&app).await;

    app.test_user.send_verification_email(&app).await;

    let verify_response = app
        .api_client
        .post(format!("{}/api/v1/auth/verify-email", &app.address,))
        .json(&json!({ "token": Uuid::nil() }))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .send()
        .await
        .expect("Failed to send request");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(
        StatusCode::BAD_REQUEST,
        verify_response.status().as_u16(),
        "Expected 400 Bad Request for invalid token."
    );
}

#[tokio::test]
async fn verify_email_fails_with_short_token() {
    // Arrange
    let app = spawn_app().await;
    app.test_user.login(&app).await;

    app.test_user.send_verification_email(&app).await;

    let verify_response = app
        .api_client
        .post(format!("{}/api/v1/auth/verify-email", &app.address,))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .json(&json!({ "token": "" }))
        .send()
        .await
        .expect("Failed to send request");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(StatusCode::BAD_REQUEST, verify_response.status().as_u16());
}
