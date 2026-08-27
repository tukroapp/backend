use reqwest::StatusCode;
use serde_json::Value;

use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};

async fn view_profile(app: &TestApp, slug: &str, viewer: Option<&RegisteredUser>) {
    let request = app
        .api_client
        .get(format!("{}/api/v1/profile/{}", app.address, slug));

    let request = match viewer {
        Some(viewer) => request.header(reqwest::header::AUTHORIZATION, viewer.auth_header()),
        None => request,
    };

    let response = request.send().await.expect("Failed to view profile");
    assert_eq!(response.status(), StatusCode::OK);
}

async fn my_visitors(app: &TestApp, user: &RegisteredUser) -> Value {
    let response = app
        .api_client
        .get(format!("{}/api/v1/me/visitor", app.address))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to get visitors");
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.expect("visitors response not JSON")
}

#[tokio::test]
async fn a_logged_in_visitor_shows_up_in_the_visitor_list() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let visitor = register_and_login(&app, "Visitante").await;

    view_profile(&app, &owner.slug, Some(&visitor)).await;

    let body = my_visitors(&app, &owner).await;
    assert_eq!(body["results"]["enabled"], serde_json::json!(true));
    assert_eq!(body["results"]["count_30d"], serde_json::json!(1));
    let recent = body["results"]["recent"].as_array().unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0]["profile"]["slug"], serde_json::json!(visitor.slug));
    assert!(recent[0]["visited_at"].is_string());

    drop_app(app).await;
}

#[tokio::test]
async fn self_views_and_anonymous_views_do_not_count() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;

    view_profile(&app, &owner.slug, Some(&owner)).await;
    view_profile(&app, &owner.slug, None).await;

    let body = my_visitors(&app, &owner).await;
    assert_eq!(body["results"]["count_30d"], serde_json::json!(0));
    assert!(body["results"]["recent"].as_array().unwrap().is_empty());

    drop_app(app).await;
}

#[tokio::test]
async fn the_same_visitor_is_counted_once_the_count_is_distinct() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let a = register_and_login(&app, "Ana").await;
    let b = register_and_login(&app, "Bia").await;

    view_profile(&app, &owner.slug, Some(&a)).await;
    view_profile(&app, &owner.slug, Some(&a)).await;
    view_profile(&app, &owner.slug, Some(&b)).await;

    let body = my_visitors(&app, &owner).await;
    assert_eq!(body["results"]["count_30d"], serde_json::json!(2));
    assert_eq!(body["results"]["recent"].as_array().unwrap().len(), 2);

    drop_app(app).await;
}

async fn set_show_visits(app: &TestApp, user: &RegisteredUser, show: bool) {
    let response = app
        .api_client
        .put(format!("{}/api/v1/me", app.address))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .json(&serde_json::json!({ "name": "Fulano", "show_profile_visits": show }))
        .send()
        .await
        .expect("Failed to update profile");
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn disabling_the_toggle_hides_your_own_visitor_list() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let visitor = register_and_login(&app, "Visitante").await;

    set_show_visits(&app, &owner, false).await;
    view_profile(&app, &owner.slug, Some(&visitor)).await;

    let body = my_visitors(&app, &owner).await;
    assert_eq!(body["results"]["enabled"], serde_json::json!(false));
    assert_eq!(body["results"]["count_30d"], serde_json::json!(0));
    assert!(body["results"]["recent"].as_array().unwrap().is_empty());

    drop_app(app).await;
}

#[tokio::test]
async fn a_visitor_with_the_toggle_off_is_not_recorded_anywhere() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let shy = register_and_login(&app, "Timido").await;

    set_show_visits(&app, &shy, false).await;
    view_profile(&app, &owner.slug, Some(&shy)).await;

    let body = my_visitors(&app, &owner).await;
    assert_eq!(body["results"]["enabled"], serde_json::json!(true));
    assert_eq!(body["results"]["count_30d"], serde_json::json!(0));

    drop_app(app).await;
}

#[tokio::test]
async fn a_blocked_visitor_drops_off_the_list() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let visitor = register_and_login(&app, "Chato").await;

    view_profile(&app, &owner.slug, Some(&visitor)).await;

    app.api_client
        .post(format!("{}/api/v1/block/{}", app.address, visitor.slug))
        .header(reqwest::header::AUTHORIZATION, owner.auth_header())
        .send()
        .await
        .expect("Failed to block");

    let body = my_visitors(&app, &owner).await;
    // A contagem crua do Redis não muda, mas o visitante some da lista hidratada.
    assert_eq!(body["results"]["count_30d"], serde_json::json!(1));
    assert!(body["results"]["recent"].as_array().unwrap().is_empty());

    drop_app(app).await;
}
