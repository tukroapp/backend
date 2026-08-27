use reqwest::StatusCode;
use serde_json::json;

use crate::friend::befriend;
use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};

async fn rate(
    app: &TestApp,
    rater: Option<&RegisteredUser>,
    slug: &str,
    trusty: i32,
    cool: i32,
    sexy: i32,
) -> reqwest::Response {
    let mut request = app
        .api_client
        .put(format!("{}/api/v1/profile/{}/rating", &app.address, slug))
        .json(&json!({ "trusty": trusty, "cool": cool, "sexy": sexy }));

    if let Some(rater) = rater {
        request = request.header(reqwest::header::AUTHORIZATION, rater.auth_header());
    }

    request.send().await.expect("Failed to rate profile")
}

async fn unrate(app: &TestApp, rater: &RegisteredUser, slug: &str) -> reqwest::Response {
    app.api_client
        .delete(format!("{}/api/v1/profile/{}/rating", &app.address, slug))
        .header(reqwest::header::AUTHORIZATION, rater.auth_header())
        .send()
        .await
        .expect("Failed to delete rating")
}

async fn unfriend(app: &TestApp, user: &RegisteredUser, slug: &str) {
    let response = app
        .api_client
        .delete(format!("{}/api/v1/friend/{}", &app.address, slug))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to unfriend");

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

async fn block(app: &TestApp, user: &RegisteredUser, slug: &str) {
    let response = app
        .api_client
        .post(format!("{}/api/v1/block/{}", &app.address, slug))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to block");

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

async fn profile_as(
    app: &TestApp,
    slug: &str,
    viewer: Option<&RegisteredUser>,
) -> serde_json::Value {
    let mut request = app
        .api_client
        .get(format!("{}/api/v1/profile/{}", &app.address, slug));

    if let Some(viewer) = viewer {
        request = request.header(reqwest::header::AUTHORIZATION, viewer.auth_header());
    }

    request
        .send()
        .await
        .expect("Failed to fetch profile")
        .json()
        .await
        .expect("profile not JSON")
}

#[tokio::test]
async fn a_friend_rates_and_the_average_shows_on_the_profile() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono do perfil").await;
    let friend = register_and_login(&app, "Amigo avaliador").await;
    befriend(&app, &friend, &owner).await;

    let response = rate(&app, Some(&friend), &owner.slug, 3, 2, 0).await;
    assert_eq!(response.status(), StatusCode::OK);

    let profile = profile_as(&app, &owner.slug, None).await;
    assert_eq!(profile["results"]["rating"]["trusty"]["average"], 3.0);
    assert_eq!(profile["results"]["rating"]["trusty"]["count"], 1);
    assert_eq!(profile["results"]["rating"]["cool"]["average"], 2.0);

    // Zero não é nota, é ausência de opinião, então não entra na média do eixo.
    assert!(profile["results"]["rating"]["sexy"]["average"].is_null());
    assert_eq!(profile["results"]["rating"]["sexy"]["count"], 0);

    drop_app(app).await;
}

#[tokio::test]
async fn the_average_ignores_who_did_not_rate_the_axis() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono do perfil").await;
    let first = register_and_login(&app, "Primeiro amigo").await;
    let second = register_and_login(&app, "Segundo amigo").await;
    befriend(&app, &first, &owner).await;
    befriend(&app, &second, &owner).await;

    rate(&app, Some(&first), &owner.slug, 3, 1, 0).await;
    rate(&app, Some(&second), &owner.slug, 1, 3, 2).await;

    let profile = profile_as(&app, &owner.slug, None).await;
    assert_eq!(profile["results"]["rating"]["trusty"]["average"], 2.0);
    assert_eq!(profile["results"]["rating"]["trusty"]["count"], 2);
    assert_eq!(profile["results"]["rating"]["sexy"]["average"], 2.0);
    assert_eq!(profile["results"]["rating"]["sexy"]["count"], 1);

    drop_app(app).await;
}

#[tokio::test]
async fn rating_again_replaces_the_previous_one() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono do perfil").await;
    let friend = register_and_login(&app, "Amigo avaliador").await;
    befriend(&app, &friend, &owner).await;

    rate(&app, Some(&friend), &owner.slug, 3, 3, 3).await;
    let response = rate(&app, Some(&friend), &owner.slug, 1, 1, 1).await;
    assert_eq!(response.status(), StatusCode::OK);

    let body: serde_json::Value = response.json().await.expect("summary not JSON");
    assert_eq!(body["results"]["trusty"]["average"], 1.0);
    assert_eq!(body["results"]["trusty"]["count"], 1);

    drop_app(app).await;
}

#[tokio::test]
async fn my_rating_comes_back_only_for_who_rated() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono do perfil").await;
    let friend = register_and_login(&app, "Amigo avaliador").await;
    let stranger = register_and_login(&app, "Desconhecido").await;
    befriend(&app, &friend, &owner).await;

    rate(&app, Some(&friend), &owner.slug, 2, 3, 1).await;

    let seen_by_rater = profile_as(&app, &owner.slug, Some(&friend)).await;
    assert_eq!(seen_by_rater["results"]["my_rating"]["trusty"], 2);
    assert_eq!(seen_by_rater["results"]["my_rating"]["sexy"], 1);

    let seen_by_stranger = profile_as(&app, &owner.slug, Some(&stranger)).await;
    assert!(seen_by_stranger["results"]["my_rating"].is_null());

    // Nem o dono do perfil enxerga quem avaliou: o agregado é anônimo.
    let seen_by_owner = profile_as(&app, &owner.slug, Some(&owner)).await;
    assert!(seen_by_owner["results"]["my_rating"].is_null());
    assert_eq!(seen_by_owner["results"]["rating"]["trusty"]["count"], 1);

    drop_app(app).await;
}

#[tokio::test]
async fn only_friends_can_rate() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono do perfil").await;
    let stranger = register_and_login(&app, "Desconhecido").await;

    let response = rate(&app, Some(&stranger), &owner.slug, 3, 3, 3).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let profile = profile_as(&app, &owner.slug, Some(&stranger)).await;
    assert_eq!(profile["results"]["can_rate"], false);

    drop_app(app).await;
}

#[tokio::test]
async fn can_rate_turns_on_with_the_friendship() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono do perfil").await;
    let friend = register_and_login(&app, "Amigo avaliador").await;
    befriend(&app, &friend, &owner).await;

    let profile = profile_as(&app, &owner.slug, Some(&friend)).await;
    assert_eq!(profile["results"]["can_rate"], true);

    // Ninguém avalia a si mesmo, nem sendo amigo de todo mundo.
    let own = profile_as(&app, &owner.slug, Some(&owner)).await;
    assert_eq!(own["results"]["can_rate"], false);

    drop_app(app).await;
}

#[tokio::test]
async fn rating_yourself_is_rejected() {
    let app = spawn_app().await;
    let user = register_and_login(&app, "Dono do perfil").await;

    let response = rate(&app, Some(&user), &user.slug, 3, 3, 3).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    drop_app(app).await;
}

#[tokio::test]
async fn anonymous_cannot_rate() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono do perfil").await;

    let response = rate(&app, None, &owner.slug, 3, 3, 3).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    drop_app(app).await;
}

#[tokio::test]
async fn values_out_of_range_and_the_empty_rating_are_rejected() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono do perfil").await;
    let friend = register_and_login(&app, "Amigo avaliador").await;
    befriend(&app, &friend, &owner).await;

    let response = rate(&app, Some(&friend), &owner.slug, 4, 0, 0).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = rate(&app, Some(&friend), &owner.slug, -1, 0, 0).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = rate(&app, Some(&friend), &owner.slug, 0, 0, 0).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    drop_app(app).await;
}

#[tokio::test]
async fn deleting_the_rating_removes_it_from_the_average() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono do perfil").await;
    let friend = register_and_login(&app, "Amigo avaliador").await;
    befriend(&app, &friend, &owner).await;

    rate(&app, Some(&friend), &owner.slug, 3, 3, 3).await;

    let response = unrate(&app, &friend, &owner.slug).await;
    assert_eq!(response.status(), StatusCode::OK);

    let body: serde_json::Value = response.json().await.expect("summary not JSON");
    assert_eq!(body["results"]["trusty"]["count"], 0);
    assert!(body["results"]["trusty"]["average"].is_null());

    // Segunda tentativa não acha mais nada para apagar.
    let response = unrate(&app, &friend, &owner.slug).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    drop_app(app).await;
}

#[tokio::test]
async fn undoing_the_friendship_erases_both_ratings() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono do perfil").await;
    let friend = register_and_login(&app, "Amigo avaliador").await;
    befriend(&app, &friend, &owner).await;

    rate(&app, Some(&friend), &owner.slug, 3, 3, 3).await;
    rate(&app, Some(&owner), &friend.slug, 2, 2, 2).await;

    unfriend(&app, &owner, &friend.slug).await;

    let profile = profile_as(&app, &owner.slug, None).await;
    assert_eq!(profile["results"]["rating"]["trusty"]["count"], 0);

    let other = profile_as(&app, &friend.slug, None).await;
    assert_eq!(other["results"]["rating"]["trusty"]["count"], 0);

    drop_app(app).await;
}

#[tokio::test]
async fn blocking_erases_the_ratings_too() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono do perfil").await;
    let friend = register_and_login(&app, "Amigo avaliador").await;
    let witness = register_and_login(&app, "Testemunha").await;
    befriend(&app, &friend, &owner).await;

    rate(&app, Some(&friend), &owner.slug, 3, 3, 3).await;
    block(&app, &owner, &friend.slug).await;

    let profile = profile_as(&app, &owner.slug, Some(&witness)).await;
    assert_eq!(profile["results"]["rating"]["trusty"]["count"], 0);

    drop_app(app).await;
}

#[tokio::test]
async fn rating_an_unknown_profile_returns_404() {
    let app = spawn_app().await;
    let user = register_and_login(&app, "Avaliador").await;

    let response = rate(&app, Some(&user), "nao-existe", 3, 3, 3).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    drop_app(app).await;
}
