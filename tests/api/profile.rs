use reqwest::StatusCode;

use crate::helpers::{TestApp, drop_app, register_and_login, spawn_app};

pub async fn get_profile(app: &TestApp, slug: &str) -> reqwest::Response {
    app.api_client
        .get(format!("{}/api/v1/profile/{}", &app.address, slug))
        .send()
        .await
        .expect("Failed to get profile")
}

#[tokio::test]
async fn profile_returns_200_for_active_account() {
    // Arrange
    let app = spawn_app().await;
    let user = register_and_login(&app, "Ativo da Silva").await;
    let slug = user.slug;

    // Act
    let response = get_profile(&app, &slug).await;
    let status = response.status();
    let body: serde_json::Value = response.json().await.expect("profile response not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["results"]["slug"], slug);
    assert_eq!(body["results"]["name"], "Ativo da Silva");
    // perfil público não expõe e-mail
    assert!(body["results"].get("email").is_none());
}

async fn search(app: &TestApp, query: &str) -> reqwest::Response {
    app.api_client
        .get(format!("{}/api/v1/profile?{}", &app.address, query))
        .send()
        .await
        .expect("Failed to search profiles")
}

#[tokio::test]
async fn search_finds_profiles_by_name_and_by_slug_prefix() {
    // Arrange
    let app = spawn_app().await;
    let user = register_and_login(&app, "Fulano de Tal").await;
    register_and_login(&app, "Beltrano Sobrinho").await;

    // Act
    let by_name: serde_json::Value = search(&app, "q=fulano de tal")
        .await
        .json()
        .await
        .expect("search response not JSON");
    let by_slug: serde_json::Value = search(&app, &format!("q={}", &user.slug[..6]))
        .await
        .json()
        .await
        .expect("search response not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(by_name["results"].as_array().unwrap().len(), 1);
    assert_eq!(by_name["results"][0]["slug"], user.slug);
    assert_eq!(by_name["results"][0]["name"], "Fulano de Tal");
    assert_eq!(by_name["next_cursor"], serde_json::Value::Null);
    assert_eq!(by_slug["results"].as_array().unwrap().len(), 1);
    assert_eq!(by_slug["results"][0]["slug"], user.slug);
}

#[tokio::test]
async fn search_walks_every_page_with_the_cursor() {
    // Arrange
    let app = spawn_app().await;
    let mut expected = Vec::new();
    for i in 0..3 {
        let user = register_and_login(&app, &format!("Fulano de Tal {i}")).await;
        expected.push(user.slug);
    }

    // Act
    let mut seen = Vec::new();
    let mut query = "q=fulano de tal&limit=2".to_string();
    loop {
        let body: serde_json::Value = search(&app, &query)
            .await
            .json()
            .await
            .expect("search response not JSON");

        for profile in body["results"].as_array().unwrap() {
            seen.push(profile["slug"].as_str().unwrap().to_string());
        }

        match body["next_cursor"].as_str() {
            Some(cursor) => query = format!("q=fulano de tal&limit=2&cursor={cursor}"),
            None => break,
        }
    }

    // Tear down
    drop_app(app).await;

    // Assert
    seen.sort();
    expected.sort();
    assert_eq!(seen, expected);
}

#[tokio::test]
async fn search_rejects_terms_shorter_than_two_characters() {
    // Arrange
    let app = spawn_app().await;

    // Act
    let response = search(&app, "q=a").await;
    let status = response.status();
    let body: serde_json::Value = response.json().await.expect("search response not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["q"][0]["code"], "too_short");
}

#[tokio::test]
async fn search_hides_the_logged_user_from_its_own_results() {
    // Arrange
    let app = spawn_app().await;
    let user = register_and_login(&app, "Fulano de Tal").await;

    // Act
    let anonymous: serde_json::Value = search(&app, "q=fulano de tal")
        .await
        .json()
        .await
        .expect("search response not JSON");

    let logged: serde_json::Value = app
        .api_client
        .get(format!("{}/api/v1/profile?q=fulano de tal", &app.address))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to search profiles")
        .json()
        .await
        .expect("search response not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(anonymous["results"].as_array().unwrap().len(), 1);
    assert!(logged["results"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn profile_returns_404_for_unknown_slug() {
    // Arrange
    let app = spawn_app().await;

    // Act
    let response = get_profile(&app, "nao-existe-esse-slug").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn profile_returns_404_for_suspended_account() {
    // Arrange
    let app = spawn_app().await;
    let user = register_and_login(&app, "Suspenso da Silva").await;
    let (slug, email) = (user.slug, user.email);

    sqlx::query!(
        "UPDATE auth_user SET status = 'suspended' WHERE email = $1",
        email
    )
    .execute(&app.db_pool)
    .await
    .expect("Failed to suspend user");

    // Act
    let response = get_profile(&app, &slug).await;

    // Tear down
    drop_app(app).await;

    // Assert: mesmo 404 de slug inexistente, sem distinguir os casos
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deleting_auth_user_cascades_to_profile() {
    // Arrange
    let app = spawn_app().await;
    let user = register_and_login(&app, "Para Deletar").await;
    let (slug, email) = (user.slug, user.email);

    // Act
    sqlx::query!("DELETE FROM auth_user WHERE email = $1", email)
        .execute(&app.db_pool)
        .await
        .expect("Failed to delete auth_user");

    let profile_exists = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM user_profile WHERE slug = $1) AS "exists!""#,
        slug
    )
    .fetch_one(&app.db_pool)
    .await
    .expect("query failed");

    // Tear down
    drop_app(app).await;

    // Assert
    assert!(
        !profile_exists,
        "user_profile deveria ter sido removido em cascade"
    );
}
