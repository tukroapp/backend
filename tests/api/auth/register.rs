use regex::Regex;
use reqwest::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::helpers::{TestApp, drop_app, spawn_app};

const SLUG_RE: &str = r"^[a-z0-9]([a-z0-9-]{1,28}[a-z0-9])$";

async fn register(app: &TestApp, email: &str, name: &str) -> reqwest::Response {
    app.api_client
        .post(format!("{}/api/v1/auth/register", &app.address))
        .json(&json!({
            "email": email,
            "password": "supersecret123",
            "name": name
        }))
        .send()
        .await
        .expect("Failed to send register request")
}

async fn slug_from(response: reqwest::Response) -> String {
    let body: serde_json::Value = response.json().await.expect("register response not JSON");
    body["results"]["slug"]
        .as_str()
        .expect("slug not found")
        .to_string()
}

/// Nenhum auth_user pode existir sem o user_profile correspondente. Se o cadastro não fosse
/// atômico, uma falha na criação do perfil deixaria um auth_user órfão aqui.
async fn orphan_auth_users(app: &TestApp) -> i64 {
    sqlx::query_scalar!(
        r#"
        SELECT COUNT(*) AS "count!"
        FROM auth_user au
        LEFT JOIN user_profile up ON up.user_id = au.id
        WHERE up.user_id IS NULL
        "#
    )
    .fetch_one(&app.db_pool)
    .await
    .expect("Failed to count orphans")
}

#[tokio::test]
async fn register_creates_auth_user_and_profile_atomically() {
    // Arrange
    let app = spawn_app().await;
    let email = format!("{}@test.com", Uuid::new_v4());

    // Act
    let response = register(&app, &email, "Fulano de Tal").await;
    let status = response.status();
    let slug = slug_from(response).await;

    let row = sqlx::query!(
        r#"
        SELECT au.id, up.name
        FROM auth_user au
        INNER JOIN user_profile up ON up.user_id = au.id
        WHERE au.email = $1 AND up.slug = $2
        "#,
        email,
        slug
    )
    .fetch_optional(&app.db_pool)
    .await
    .expect("query failed");

    let orphans = orphan_auth_users(&app).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::CREATED);
    assert!(row.is_some(), "auth_user + user_profile deveriam existir");
    assert_eq!(row.unwrap().name, "Fulano de Tal");
    assert_eq!(orphans, 0);
}

#[tokio::test]
async fn register_duplicate_email_conflicts_without_orphan() {
    // Arrange
    let app = spawn_app().await;
    let email = format!("{}@test.com", Uuid::new_v4());

    // Act
    let first = register(&app, &email, "Primeiro").await;
    let second = register(&app, &email, "Segundo").await;

    let orphans = orphan_auth_users(&app).await;

    // Tear down
    let first_status = first.status();
    let second_status = second.status();
    drop_app(app).await;

    // Assert
    assert_eq!(first_status, StatusCode::CREATED);
    assert_eq!(second_status, StatusCode::CONFLICT);
    assert_eq!(orphans, 0, "conflito não deve deixar auth_user órfão");
}

#[tokio::test]
async fn register_slug_collision_generates_distinct_valid_slug() {
    // Arrange
    let app = spawn_app().await;
    let re = Regex::new(SLUG_RE).unwrap();

    // Act: mesmo nome, e-mails distintos
    let first =
        slug_from(register(&app, &format!("{}@test.com", Uuid::new_v4()), "João Silva").await)
            .await;
    let second =
        slug_from(register(&app, &format!("{}@test.com", Uuid::new_v4()), "João Silva").await)
            .await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(first, "joao-silva");
    assert_ne!(first, second, "colisão deve gerar slug distinto");
    assert!(
        re.is_match(&second),
        "slug alternativo deve satisfazer o CHECK: {second}"
    );
}

#[tokio::test]
async fn register_symbol_only_name_falls_back_to_valid_slug() {
    // Arrange
    let app = spawn_app().await;
    let re = Regex::new(SLUG_RE).unwrap();

    // Act: nome que geraria slug vazio
    let slug =
        slug_from(register(&app, &format!("{}@test.com", Uuid::new_v4()), "$$$ ###").await).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert!(slug.starts_with("user-"), "esperado fallback, veio: {slug}");
    assert!(
        re.is_match(&slug),
        "fallback deve satisfazer o CHECK: {slug}"
    );
}
