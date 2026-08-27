use deadpool_redis::redis::cmd;
use reqwest::StatusCode;
use serde_json::json;
use tukro_backend::{constants::SESSION_REDIS_PREFIX, startup::RedisPool, utils::compute_sha_hash};

use crate::helpers::{drop_app, spawn_app};

async fn get_redis_session(redis: &RedisPool, token_hash: &str) -> Option<serde_json::Value> {
    let mut conn = redis.get().await.expect("Failed to get Redis connection");
    let raw: Option<String> = cmd("GET")
        .arg(format!("{}{}", SESSION_REDIS_PREFIX, token_hash))
        .query_async(&mut conn)
        .await
        .expect("Failed to query session from Redis");

    raw.map(|value| serde_json::from_str(&value).expect("session value is not valid JSON"))
}

#[tokio::test]
async fn login_works() {
    // Arrange
    let app = spawn_app().await;

    // Act
    let response = app
        .post_login(&json!({
            "email": app.test_user.email,
            "password": app.test_user.password
        }))
        .await;

    let status = response.status();
    let body: serde_json::Value = response.json().await.expect("login response is not JSON");

    let token = body["results"]["token"]
        .as_str()
        .expect("token not found in login response")
        .to_string();

    let token_hash = compute_sha_hash(&token);
    let session = get_redis_session(&app.redis_pool, &token_hash)
        .await
        .expect("session not found in Redis");

    let user_id = app.test_user.id.to_string();

    // Tear down
    drop_app(app).await;

    // Assert
    assert!(status.is_success());
    assert!(body["results"]["expires_at"].is_string());
    assert_eq!(session["user_id"], user_id);
    assert!(session["created_at"].is_string());
}

#[tokio::test]
async fn login_fails_with_wrong_credentials() {
    // Arrange
    let app = spawn_app().await;

    // Act
    let response = app
        .api_client
        .post(format!("{}/api/v1/auth/login", &app.address))
        .json(&json!({
            "email": "admin@admin.com",
            "password": "wrongpassword"
        }))
        .send()
        .await
        .expect("Failed to send request");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
