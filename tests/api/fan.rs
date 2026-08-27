use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};

async fn json_of(response: reqwest::Response) -> Value {
    response.json().await.expect("response not JSON")
}

async fn become_fan(app: &TestApp, fan: &RegisteredUser, idol_slug: &str) -> reqwest::Response {
    app.api_client
        .post(format!("{}/api/v1/profile/{}/fan", app.address, idol_slug))
        .header(reqwest::header::AUTHORIZATION, fan.auth_header())
        .send()
        .await
        .expect("Failed to become fan")
}

async fn stop_fan(app: &TestApp, fan: &RegisteredUser, idol_slug: &str) -> reqwest::Response {
    app.api_client
        .delete(format!("{}/api/v1/profile/{}/fan", app.address, idol_slug))
        .header(reqwest::header::AUTHORIZATION, fan.auth_header())
        .send()
        .await
        .expect("Failed to stop being fan")
}

async fn profile(app: &TestApp, slug: &str, viewer: &RegisteredUser) -> Value {
    let response = app
        .api_client
        .get(format!("{}/api/v1/profile/{}", app.address, slug))
        .header(reqwest::header::AUTHORIZATION, viewer.auth_header())
        .send()
        .await
        .expect("Failed to get profile");
    assert_eq!(response.status(), StatusCode::OK);
    json_of(response).await
}

#[tokio::test]
async fn becoming_a_fan_is_unilateral_and_needs_no_friendship() {
    let app = spawn_app().await;
    let idol = register_and_login(&app, "Idolo").await;
    let fan = register_and_login(&app, "Fa").await;

    let response = become_fan(&app, &fan, &idol.slug).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_of(response).await;
    assert_eq!(body["results"]["is_fan"], json!(true));
    assert_eq!(body["results"]["fans_count"], json!(1));

    // Idempotente.
    let again = json_of(become_fan(&app, &fan, &idol.slug).await).await;
    assert_eq!(again["results"]["fans_count"], json!(1));

    let idol_profile = profile(&app, &idol.slug, &fan).await;
    assert_eq!(idol_profile["results"]["fans_count"], json!(1));
    assert_eq!(idol_profile["results"]["is_fan"], json!(true));
    assert_eq!(idol_profile["results"]["can_be_fan"], json!(true));

    drop_app(app).await;
}

#[tokio::test]
async fn stopping_being_a_fan_is_idempotent() {
    let app = spawn_app().await;
    let idol = register_and_login(&app, "Idolo").await;
    let fan = register_and_login(&app, "Fa").await;

    become_fan(&app, &fan, &idol.slug).await;

    let response = stop_fan(&app, &fan, &idol.slug).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_of(response).await;
    assert_eq!(body["results"]["is_fan"], json!(false));
    assert_eq!(body["results"]["fans_count"], json!(0));

    // De novo, sem erro.
    assert_eq!(stop_fan(&app, &fan, &idol.slug).await.status(), StatusCode::OK);

    assert_eq!(
        profile(&app, &idol.slug, &fan).await["results"]["is_fan"],
        json!(false)
    );

    drop_app(app).await;
}

#[tokio::test]
async fn cannot_be_a_fan_of_yourself_or_while_logged_out() {
    let app = spawn_app().await;
    let user = register_and_login(&app, "Fulano").await;

    assert_eq!(
        become_fan(&app, &user, &user.slug).await.status(),
        StatusCode::BAD_REQUEST
    );

    let anon = app
        .api_client
        .post(format!("{}/api/v1/profile/{}/fan", app.address, user.slug))
        .send()
        .await
        .expect("Failed to send");
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);

    let own = profile(&app, &user.slug, &user).await;
    assert_eq!(own["results"]["can_be_fan"], json!(false));

    drop_app(app).await;
}

#[tokio::test]
async fn a_blocked_user_cannot_become_a_fan_and_blocking_drops_the_fan() {
    let app = spawn_app().await;
    let idol = register_and_login(&app, "Idolo").await;
    let fan = register_and_login(&app, "Fa").await;

    become_fan(&app, &fan, &idol.slug).await;
    assert_eq!(
        profile(&app, &idol.slug, &idol).await["results"]["fans_count"],
        json!(1)
    );

    // Idol bloqueia o fã: a linha de fã some.
    app.api_client
        .post(format!("{}/api/v1/block/{}", app.address, fan.slug))
        .header(reqwest::header::AUTHORIZATION, idol.auth_header())
        .send()
        .await
        .expect("Failed to block");

    assert_eq!(
        profile(&app, &idol.slug, &idol).await["results"]["fans_count"],
        json!(0)
    );

    // E não consegue virar fã de novo enquanto bloqueado.
    assert_eq!(
        become_fan(&app, &fan, &idol.slug).await.status(),
        StatusCode::NOT_FOUND
    );

    drop_app(app).await;
}

#[tokio::test]
async fn the_fan_list_is_public_and_paginated() {
    let app = spawn_app().await;
    let idol = register_and_login(&app, "Idolo").await;
    let a = register_and_login(&app, "Ana").await;
    let b = register_and_login(&app, "Bia").await;

    become_fan(&app, &a, &idol.slug).await;
    become_fan(&app, &b, &idol.slug).await;

    let response = app
        .api_client
        .get(format!("{}/api/v1/profile/{}/fan", app.address, idol.slug))
        .send()
        .await
        .expect("Failed to list fans");
    assert_eq!(response.status(), StatusCode::OK);

    let body = json_of(response).await;
    let fans = body["results"].as_array().unwrap();
    assert_eq!(fans.len(), 2);
    let slugs: Vec<&str> = fans.iter().map(|f| f["slug"].as_str().unwrap()).collect();
    assert!(slugs.contains(&a.slug.as_str()));
    assert!(slugs.contains(&b.slug.as_str()));

    drop_app(app).await;
}
