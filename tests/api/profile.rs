use chrono::{Months, Utc};
use reqwest::StatusCode;
use serde_json::json;

use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};

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

/// Preenche o perfil detalhado de um usuário já registrado. O `name` é
/// obrigatório no PUT, então vai junto sempre.
async fn update_profile(app: &TestApp, user: &RegisteredUser, name: &str, body: serde_json::Value) {
    let mut payload = body;
    payload["name"] = json!(name);

    let response = app
        .api_client
        .put(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .json(&payload)
        .send()
        .await
        .expect("Failed to update profile");

    assert_eq!(response.status(), StatusCode::OK);
}

/// Data de nascimento de quem tem exatamente `years` anos completos hoje.
fn birthdate_for_age(years: u32) -> String {
    (Utc::now().date_naive() - Months::new(years * 12)).to_string()
}

#[tokio::test]
async fn profile_hides_the_details_when_the_privacy_closes_the_profile() {
    // Arrange
    let app = spawn_app().await;
    let open = register_and_login(&app, "Perfil Aberto").await;
    let closed = register_and_login(&app, "Perfil Fechado").await;

    update_profile(
        &app,
        &open,
        "Perfil Aberto",
        json!({ "interests": ["Fliperama"], "occupation": "Desenvolvedor" }),
    )
    .await;
    update_profile(
        &app,
        &closed,
        "Perfil Fechado",
        json!({ "interests": ["Fliperama"], "profile_visibility": "friends" }),
    )
    .await;

    // Act
    let open_body: serde_json::Value = get_profile(&app, &open.slug)
        .await
        .json()
        .await
        .expect("profile response not JSON");
    let closed_body: serde_json::Value = get_profile(&app, &closed.slug)
        .await
        .json()
        .await
        .expect("profile response not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(
        open_body["results"]["details"]["interests"],
        json!(["Fliperama"])
    );
    assert_eq!(
        open_body["results"]["details"]["occupation"],
        "Desenvolvedor"
    );
    // o card público continua visível, só o detalhado some
    assert_eq!(closed_body["results"]["name"], "Perfil Fechado");
    assert_eq!(closed_body["results"]["details"], serde_json::Value::Null);
}

#[tokio::test]
async fn search_filters_by_city_and_by_interest() {
    // Arrange
    let app = spawn_app().await;
    let curitiba = register_and_login(&app, "Fulano de Curitiba").await;
    let sao_paulo = register_and_login(&app, "Fulano de Sao Paulo").await;

    update_profile(
        &app,
        &curitiba,
        "Fulano de Curitiba",
        json!({ "city": "Curitiba", "interests": ["Rock progressivo", "Fliperama"] }),
    )
    .await;
    update_profile(
        &app,
        &sao_paulo,
        "Fulano de Sao Paulo",
        json!({ "city": "São Paulo", "interests": ["Pagode"] }),
    )
    .await;

    // Act
    let by_city: serde_json::Value = search(&app, "city=curitiba")
        .await
        .json()
        .await
        .expect("search response not JSON");
    let by_interest: serde_json::Value = search(&app, "interest=rock")
        .await
        .json()
        .await
        .expect("search response not JSON");
    let combined: serde_json::Value = search(&app, "q=fulano&city=curitiba&interest=pagode")
        .await
        .json()
        .await
        .expect("search response not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(by_city["results"].as_array().unwrap().len(), 1);
    assert_eq!(by_city["results"][0]["slug"], curitiba.slug);
    assert_eq!(by_city["results"][0]["city"], "Curitiba");
    assert_eq!(by_interest["results"].as_array().unwrap().len(), 1);
    assert_eq!(by_interest["results"][0]["slug"], curitiba.slug);
    // os filtros somam, e ninguém é de Curitiba e curte pagode
    assert!(combined["results"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn search_filters_by_age_range() {
    // Arrange
    let app = spawn_app().await;
    let young = register_and_login(&app, "Fulano Novo").await;
    let older = register_and_login(&app, "Fulano Velho").await;
    let no_birthdate = register_and_login(&app, "Fulano Sem Data").await;

    update_profile(
        &app,
        &young,
        "Fulano Novo",
        json!({ "birthdate": birthdate_for_age(25) }),
    )
    .await;
    update_profile(
        &app,
        &older,
        "Fulano Velho",
        json!({ "birthdate": birthdate_for_age(50) }),
    )
    .await;

    // Act
    let body: serde_json::Value = search(&app, "q=fulano&min_age=24&max_age=26")
        .await
        .json()
        .await
        .expect("search response not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    let slugs: Vec<&str> = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|profile| profile["slug"].as_str().unwrap())
        .collect();
    assert_eq!(slugs, vec![young.slug.as_str()]);
    assert_eq!(body["results"][0]["age"], 25);
    assert!(!slugs.contains(&older.slug.as_str()));
    // sem data de nascimento fica de fora quando o filtro de idade é usado
    assert!(!slugs.contains(&no_birthdate.slug.as_str()));
}

#[tokio::test]
async fn search_without_a_term_lists_the_profiles_that_pass_the_filters() {
    // Arrange
    let app = spawn_app().await;
    let curitiba = register_and_login(&app, "Fulano de Curitiba").await;
    register_and_login(&app, "Beltrano Sobrinho").await;

    update_profile(
        &app,
        &curitiba,
        "Fulano de Curitiba",
        json!({ "city": "Curitiba" }),
    )
    .await;

    // Act
    let everyone: serde_json::Value = search(&app, "")
        .await
        .json()
        .await
        .expect("search response not JSON");
    let filtered: serde_json::Value = search(&app, "city=curitiba")
        .await
        .json()
        .await
        .expect("search response not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    // sem termo e sem filtro a busca lista todo mundo, inclusive o admin do seed
    assert!(everyone["results"].as_array().unwrap().len() >= 2);
    assert_eq!(filtered["results"].as_array().unwrap().len(), 1);
    assert_eq!(filtered["results"][0]["slug"], curitiba.slug);
}
