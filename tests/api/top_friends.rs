use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::friend::befriend;
use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};

async fn json_of(response: reqwest::Response) -> Value {
    response.json().await.expect("response not JSON")
}

async fn set_top(app: &TestApp, user: &RegisteredUser, slugs: Vec<&str>) -> reqwest::Response {
    app.api_client
        .put(format!("{}/api/v1/friend/top", &app.address))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .json(&json!({ "slugs": slugs }))
        .send()
        .await
        .expect("Failed to set top friends")
}

async fn profile_as(app: &TestApp, slug: &str, viewer: Option<&RegisteredUser>) -> Value {
    let mut request = app
        .api_client
        .get(format!("{}/api/v1/profile/{}", &app.address, slug));

    if let Some(viewer) = viewer {
        request = request.header(reqwest::header::AUTHORIZATION, viewer.auth_header());
    }

    json_of(request.send().await.expect("Failed to read profile")).await
}

async fn friend_list(
    app: &TestApp,
    slug: &str,
    viewer: Option<&RegisteredUser>,
    query: &str,
) -> Value {
    let mut request = app.api_client.get(format!(
        "{}/api/v1/profile/{}/friend?{}",
        &app.address, slug, query
    ));

    if let Some(viewer) = viewer {
        request = request.header(reqwest::header::AUTHORIZATION, viewer.auth_header());
    }

    json_of(request.send().await.expect("Failed to list friends")).await
}

fn slugs_of(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("esperava uma lista")
        .iter()
        .map(|item| item["slug"].as_str().expect("slug").to_string())
        .collect()
}

#[tokio::test]
async fn the_top_keeps_the_order_the_owner_chose() {
    // Arrange
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let ana = register_and_login(&app, "Ana").await;
    let bruno = register_and_login(&app, "Bruno").await;
    let carla = register_and_login(&app, "Carla").await;

    befriend(&app, &owner, &ana).await;
    befriend(&app, &owner, &bruno).await;
    befriend(&app, &owner, &carla).await;

    // Act
    let response = set_top(&app, &owner, vec![&carla.slug, &ana.slug]).await;
    let status = response.status();
    let body = json_of(response).await;

    let profile = profile_as(&app, &owner.slug, None).await;

    // Reordenar substitui a seleção inteira
    assert_eq!(
        set_top(&app, &owner, vec![&bruno.slug, &carla.slug, &ana.slug])
            .await
            .status(),
        StatusCode::OK
    );
    let reordered = profile_as(&app, &owner.slug, None).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["results"]["count"], 2);
    assert_eq!(
        slugs_of(&profile["results"]["top_friends"]),
        vec![carla.slug.clone(), ana.slug.clone()]
    );
    assert_eq!(
        slugs_of(&reordered["results"]["top_friends"]),
        vec![bruno.slug, carla.slug, ana.slug]
    );
}

#[tokio::test]
async fn an_empty_list_clears_the_top() {
    // Arrange
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let ana = register_and_login(&app, "Ana").await;
    befriend(&app, &owner, &ana).await;
    assert_eq!(
        set_top(&app, &owner, vec![&ana.slug]).await.status(),
        StatusCode::OK
    );

    // Act
    let response = set_top(&app, &owner, vec![]).await;
    let profile = profile_as(&app, &owner.slug, None).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        profile["results"]["top_friends"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // A amizade continua de pé, só saiu do destaque
    assert_eq!(profile["results"]["friends_count"], 1);
}

#[tokio::test]
async fn the_top_only_takes_friends_without_repeating() {
    // Arrange
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let ana = register_and_login(&app, "Ana").await;
    let estranho = register_and_login(&app, "Estranho").await;
    befriend(&app, &owner, &ana).await;

    // Act
    let stranger = set_top(&app, &owner, vec![&estranho.slug]).await;
    let repeated = set_top(&app, &owner, vec![&ana.slug, &ana.slug]).await;
    let unknown = set_top(&app, &owner, vec!["nao-existe"]).await;

    let many: Vec<String> = (0..10).map(|i| format!("amigo-{i}")).collect();
    let too_many = app
        .api_client
        .put(format!("{}/api/v1/friend/top", &app.address))
        .header(reqwest::header::AUTHORIZATION, owner.auth_header())
        .json(&json!({ "slugs": many }))
        .send()
        .await
        .expect("Failed to set top friends");

    let profile = profile_as(&app, &owner.slug, None).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(stranger.status(), StatusCode::BAD_REQUEST);
    assert_eq!(repeated.status(), StatusCode::BAD_REQUEST);
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
    assert_eq!(too_many.status(), StatusCode::BAD_REQUEST);
    // Nenhuma tentativa inválida pode ter deixado resíduo
    assert!(
        profile["results"]["top_friends"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn undoing_the_friendship_takes_the_person_off_the_top() {
    // Arrange
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let ana = register_and_login(&app, "Ana").await;
    let bruno = register_and_login(&app, "Bruno").await;
    befriend(&app, &owner, &ana).await;
    befriend(&app, &owner, &bruno).await;
    assert_eq!(
        set_top(&app, &owner, vec![&ana.slug, &bruno.slug])
            .await
            .status(),
        StatusCode::OK
    );

    // Act
    let response = app
        .api_client
        .delete(format!("{}/api/v1/friend/{}", &app.address, ana.slug))
        .header(reqwest::header::AUTHORIZATION, owner.auth_header())
        .send()
        .await
        .expect("Failed to remove friend");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let profile = profile_as(&app, &owner.slug, None).await;

    // O destaque continua editável depois disso
    let again = set_top(&app, &owner, vec![&bruno.slug]).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(
        slugs_of(&profile["results"]["top_friends"]),
        vec![bruno.slug]
    );
    assert_eq!(again.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_profile_shows_the_friends_in_common() {
    // Arrange
    let app = spawn_app().await;
    let viewer = register_and_login(&app, "Visitante").await;
    let target = register_and_login(&app, "Alvo").await;
    let comum1 = register_and_login(&app, "Comum Um").await;
    let comum2 = register_and_login(&app, "Comum Dois").await;
    let so_do_alvo = register_and_login(&app, "Só do Alvo").await;

    befriend(&app, &viewer, &comum1).await;
    befriend(&app, &viewer, &comum2).await;
    befriend(&app, &target, &comum1).await;
    befriend(&app, &target, &comum2).await;
    befriend(&app, &target, &so_do_alvo).await;

    // Act
    let seen = profile_as(&app, &target.slug, Some(&viewer)).await;
    let anonymous = profile_as(&app, &target.slug, None).await;
    let own = profile_as(&app, &viewer.slug, Some(&viewer)).await;

    let mutual_list = friend_list(&app, &target.slug, Some(&viewer), "mutual=true").await;
    let full_list = friend_list(&app, &target.slug, Some(&viewer), "").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(seen["results"]["mutual_friends_count"], 2);
    let mut mutual = slugs_of(&seen["results"]["mutual_friends"]);
    mutual.sort();
    let mut expected = vec![comum1.slug.clone(), comum2.slug.clone()];
    expected.sort();
    assert_eq!(mutual, expected);

    // Anônimo não tem amigos em comum com ninguém
    assert!(anonymous["results"]["mutual_friends_count"].is_null());
    // Nem faz sentido no próprio perfil
    assert!(own["results"]["mutual_friends_count"].is_null());

    let mut listed = slugs_of(&mutual_list["results"]);
    listed.sort();
    assert_eq!(listed, expected);
    assert_eq!(full_list["results"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn a_private_friend_list_hides_the_top_and_the_common_friends() {
    // Arrange
    let app = spawn_app().await;
    let viewer = register_and_login(&app, "Visitante").await;
    let target = register_and_login(&app, "Alvo").await;
    let comum = register_and_login(&app, "Comum").await;

    befriend(&app, &viewer, &comum).await;
    befriend(&app, &target, &comum).await;
    assert_eq!(
        set_top(&app, &target, vec![&comum.slug]).await.status(),
        StatusCode::OK
    );

    let response = app
        .api_client
        .put(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, target.auth_header())
        .json(&json!({ "name": "Alvo", "profile_visibility": "nobody" }))
        .send()
        .await
        .expect("Failed to close the profile");
    assert_eq!(response.status(), StatusCode::OK);

    // Act
    let seen = profile_as(&app, &target.slug, Some(&viewer)).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(seen["results"]["friends_visible"], false);
    assert!(seen["results"]["mutual_friends_count"].is_null());
    assert!(
        seen["results"]["mutual_friends"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        seen["results"]["top_friends"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
