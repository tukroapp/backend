use reqwest::StatusCode;

use crate::helpers::{drop_app, spawn_app};

#[tokio::test]
async fn send_verification_email_succeeds() {
    // Arrange
    let app = spawn_app().await;
    app.test_user.login(&app).await;

    // Act
    let response = app
        .api_client
        .post(format!(
            "{}/api/v1/auth/send-verification-email",
            &app.address
        ))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .send()
        .await
        .expect("Failed to send request");

    let email = sqlx::query!(
        r#"
        SELECT to_address
        FROM email_queue
        WHERE to_address = $1
        "#,
        &[app.test_user.email.clone()]
    )
    .fetch_optional(&app.db_pool)
    .await
    .expect("Failed to fetch email from email_queue");

    // Tear down
    drop_app(app).await;

    // Assert
    assert!(response.status().is_success());
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(email.is_some());
}

#[tokio::test]
async fn send_verification_email_fails_if_already_verified() {
    // Arrange
    let app = spawn_app().await;
    app.test_user.login(&app).await;

    sqlx::query!(
        r#"
        UPDATE auth_user
        SET is_email_verified = true
        WHERE id = $1
        "#,
        app.test_user.id
    )
    .execute(&app.db_pool)
    .await
    .expect("Failed to mark email as verified");

    // Act
    let response = app
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
    assert!(response.status().is_client_error());
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn send_verification_email_fails_if_not_logged_in() {
    // Arrange
    let app = spawn_app().await;

    // Act
    let response = app
        .api_client
        .post(format!(
            "{}/api/v1/auth/send-verification-email",
            &app.address
        ))
        .header(reqwest::header::AUTHORIZATION, "Token invalid_token")
        .send()
        .await
        .expect("Failed to send request");

    // Tear down
    drop_app(app).await;

    // Assert
    assert!(response.status().is_client_error());
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
