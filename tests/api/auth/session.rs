use chrono::{Duration, Utc};
use deadpool_redis::redis::cmd;
use reqwest::StatusCode;
use serde_json::json;
use tukro_backend::{
    constants::{SESSION_REDIS_PREFIX, USER_SESSIONS_REDIS_PREFIX},
    startup::RedisPool,
    utils::{compute_sha_hash, gen_random_string},
};

/// Injeta uma sessão diretamente no Redis com um `created_at` arbitrário, replicando o que o
/// `persist_session` faz (chave da sessão + campo no índice reverso).
async fn seed_session(
    redis: &RedisPool,
    user_id: &uuid::Uuid,
    created_at: chrono::DateTime<Utc>,
) -> String {
    let token = gen_random_string(48);
    let token_hash = compute_sha_hash(&token);

    let value = json!({
        "user_id": user_id,
        "created_at": created_at.to_rfc3339(),
        "user_agent": null,
        "ip": null
    })
    .to_string();

    let mut conn = redis.get().await.expect("Failed to get Redis connection");

    cmd("SET")
        .arg(format!("{}{}", SESSION_REDIS_PREFIX, token_hash))
        .arg(value)
        .arg("EX")
        .arg(7 * 24 * 60 * 60)
        .query_async::<()>(&mut conn)
        .await
        .expect("Failed to seed session in Redis");

    cmd("HSET")
        .arg(format!("{}{}", USER_SESSIONS_REDIS_PREFIX, user_id))
        .arg(&token_hash)
        .arg(1)
        .query_async::<()>(&mut conn)
        .await
        .expect("Failed to seed session index in Redis");

    token
}

/// Sem teto absoluto: uma sessão antiga (aqui, criada há mais de um ano) continua válida
/// enquanto o idle TTL for renovado. Enquanto o usuário for ativo, o mesmo token desliza para
/// sempre.
#[tokio::test]
async fn old_session_is_still_valid() {
    // Arrange
    let app = crate::helpers::spawn_app().await;

    let created_at = Utc::now() - Duration::days(400);
    let token = seed_session(&app.redis_pool, &app.test_user.id, created_at).await;

    // Act
    let response = app
        .api_client
        .post(format!(
            "{}/api/v1/auth/send-verification-email",
            &app.address
        ))
        .header(reqwest::header::AUTHORIZATION, format!("Token {}", token))
        .send()
        .await
        .expect("Failed to send request");

    // Tear down
    crate::helpers::drop_app(app).await;

    // Assert
    assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
}
