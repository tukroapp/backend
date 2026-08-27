use reqwest::StatusCode;
use serde_json::json;

use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};

async fn report(
    app: &TestApp,
    reporter: Option<&RegisteredUser>,
    slug: &str,
    payload: serde_json::Value,
) -> reqwest::Response {
    let mut request = app
        .api_client
        .post(format!("{}/api/v1/report/{}", &app.address, slug))
        .json(&payload);

    if let Some(reporter) = reporter {
        request = request.header(reqwest::header::AUTHORIZATION, reporter.auth_header());
    }

    request.send().await.expect("Failed to report profile")
}

/// Quantas denúncias abertas existem contra `slug`, lido direto do banco porque
/// não há endpoint de leitura: a fila é do painel de admin, que ainda não existe.
async fn open_reports_against(app: &TestApp, slug: &str) -> i64 {
    sqlx::query_scalar!(
        r#"
        SELECT COUNT(*) AS "count!"
        FROM report r
        INNER JOIN user_profile up ON up.user_id = r.reported_user_id
        WHERE up.slug = $1 AND r.status = 'open'
        "#,
        slug,
    )
    .fetch_one(&app.db_pool)
    .await
    .expect("Failed to count reports")
}

#[tokio::test]
async fn reporting_a_profile_registers_the_report() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    // Act
    let response = report(
        &app,
        Some(&bob),
        &alice.slug,
        json!({ "reason": "harassment", "details": "  perseguindo no mural  " }),
    )
    .await;

    let status = response.status();
    let body: serde_json::Value = response.json().await.expect("report not JSON");

    let stored = sqlx::query!(
        r#"
        SELECT r.reason::TEXT AS "reason!", r.details, r.status::TEXT AS "status!"
        FROM report r
        INNER JOIN user_profile up ON up.user_id = r.reported_user_id
        WHERE up.slug = $1
        "#,
        alice.slug,
    )
    .fetch_one(&app.db_pool)
    .await
    .expect("report não foi gravado");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::CREATED);
    assert!(body["results"]["id"].is_string());
    assert!(body["results"]["created_at"].is_string());

    assert_eq!(stored.reason, "harassment");
    assert_eq!(stored.details.as_deref(), Some("perseguindo no mural"));
    // Nasce na fila do moderador
    assert_eq!(stored.status, "open");
}

#[tokio::test]
async fn the_same_profile_can_only_be_reported_once_by_the_same_user() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let carol = register_and_login(&app, "Carol").await;

    let first = report(&app, Some(&bob), &alice.slug, json!({ "reason": "spam" })).await;

    // Act
    let again = report(
        &app,
        Some(&bob),
        &alice.slug,
        json!({ "reason": "nudity", "details": "outro motivo" }),
    )
    .await;

    // Outra pessoa denunciando o mesmo perfil continua valendo
    let by_carol = report(&app, Some(&carol), &alice.slug, json!({ "reason": "spam" })).await;

    // E o bob pode denunciar outro perfil
    let other_target = report(&app, Some(&bob), &carol.slug, json!({ "reason": "spam" })).await;

    let against_alice = open_reports_against(&app, &alice.slug).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(first.status(), StatusCode::CREATED);
    assert_eq!(again.status(), StatusCode::CONFLICT);
    assert_eq!(by_carol.status(), StatusCode::CREATED);
    assert_eq!(other_target.status(), StatusCode::CREATED);
    assert_eq!(
        against_alice, 2,
        "a segunda denúncia do bob não pode entrar"
    );
}

#[tokio::test]
async fn reporting_yourself_returns_400() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;

    // Act
    let response = report(&app, Some(&alice), &alice.slug, json!({ "reason": "spam" })).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_unknown_reason_returns_400() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    // Act
    let unknown = report(
        &app,
        Some(&bob),
        &alice.slug,
        json!({ "reason": "porque sim" }),
    )
    .await;
    let missing = report(&app, Some(&bob), &alice.slug, json!({})).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
    assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn details_longer_than_the_limit_return_400() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    // Act
    let response = report(
        &app,
        Some(&bob),
        &alice.slug,
        json!({ "reason": "other", "details": "a".repeat(2001) }),
    )
    .await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_blocked_profile_can_still_be_reported() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    // Bob bloqueia a Alice e mesmo assim precisa poder denunciar
    let response = app
        .api_client
        .post(format!("{}/api/v1/block/{}", &app.address, alice.slug))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .send()
        .await
        .expect("Failed to block");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // Act
    let response = report(
        &app,
        Some(&bob),
        &alice.slug,
        json!({ "reason": "harassment" }),
    )
    .await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn reporting_an_unknown_profile_returns_404() {
    // Arrange
    let app = spawn_app().await;
    let bob = register_and_login(&app, "Bob").await;

    // Act
    let response = report(&app, Some(&bob), "nao-existe", json!({ "reason": "spam" })).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn reporting_requires_authentication() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;

    // Act
    let response = report(&app, None, &alice.slug, json!({ "reason": "spam" })).await;
    let stored = open_reports_against(&app, &alice.slug).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(stored, 0);
}
