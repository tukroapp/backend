use reqwest::StatusCode;

use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};

async fn block(app: &TestApp, user: &RegisteredUser, slug: &str) -> reqwest::Response {
    app.api_client
        .post(format!("{}/api/v1/block/{}", &app.address, slug))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to block profile")
}

async fn unblock(app: &TestApp, user: &RegisteredUser, slug: &str) -> reqwest::Response {
    app.api_client
        .delete(format!("{}/api/v1/block/{}", &app.address, slug))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to unblock profile")
}

async fn list_blocks(app: &TestApp, user: &RegisteredUser) -> serde_json::Value {
    app.api_client
        .get(format!("{}/api/v1/block", &app.address))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to list blocks")
        .json()
        .await
        .expect("block list not JSON")
}

async fn profile_status(app: &TestApp, slug: &str, viewer: &RegisteredUser) -> reqwest::StatusCode {
    app.api_client
        .get(format!("{}/api/v1/profile/{}", &app.address, slug))
        .header(reqwest::header::AUTHORIZATION, viewer.auth_header())
        .send()
        .await
        .expect("Failed to get profile")
        .status()
}

async fn send_request(app: &TestApp, from: &RegisteredUser, to_slug: &str) -> reqwest::Response {
    app.api_client
        .post(format!(
            "{}/api/v1/friend/request/{}",
            &app.address, to_slug
        ))
        .header(reqwest::header::AUTHORIZATION, from.auth_header())
        .send()
        .await
        .expect("Failed to send friend request")
}

#[tokio::test]
async fn blocking_hides_the_profile_for_both_sides() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    // Act
    let response = block(&app, &alice, &bob.slug).await;
    let blocker_view = profile_status(&app, &bob.slug, &alice).await;
    let blocked_view = profile_status(&app, &alice.slug, &bob).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(blocker_view, StatusCode::NOT_FOUND);
    assert_eq!(blocked_view, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn blocking_undoes_the_friendship() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    send_request(&app, &alice, &bob.slug).await;
    let inbox: serde_json::Value = app
        .api_client
        .get(format!("{}/api/v1/friend/request", &app.address))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .send()
        .await
        .expect("Failed to list friend requests")
        .json()
        .await
        .expect("friend request list not JSON");
    let request_id = inbox["results"][0]["id"].as_str().unwrap().to_string();
    app.api_client
        .post(format!(
            "{}/api/v1/friend/request/{}/accept",
            &app.address, request_id
        ))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .send()
        .await
        .expect("Failed to accept friend request");

    // Act
    block(&app, &alice, &bob.slug).await;
    unblock(&app, &alice, &bob.slug).await;

    let relationship = app
        .api_client
        .get(format!("{}/api/v1/profile/{}", &app.address, bob.slug))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .send()
        .await
        .expect("Failed to get profile")
        .json::<serde_json::Value>()
        .await
        .expect("profile response not JSON")["results"]["relationship"]
        .as_str()
        .unwrap()
        .to_string();

    // Tear down
    drop_app(app).await;

    // Assert: desbloquear não restaura a amizade desfeita pelo bloqueio
    assert_eq!(relationship, "none");
}

#[tokio::test]
async fn blocking_cancels_pending_requests() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    send_request(&app, &alice, &bob.slug).await;

    // Act
    block(&app, &bob, &alice.slug).await;

    let inbox: serde_json::Value = app
        .api_client
        .get(format!("{}/api/v1/friend/request", &app.address))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .send()
        .await
        .expect("Failed to list friend requests")
        .json()
        .await
        .expect("friend request list not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert!(inbox["results"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn block_list_shows_blocked_profiles_and_unblock_clears_it() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    block(&app, &alice, &bob.slug).await;

    // Act
    let listed = list_blocks(&app, &alice).await;
    let removed = unblock(&app, &alice, &bob.slug).await;
    let after = list_blocks(&app, &alice).await;
    let visible_again = profile_status(&app, &bob.slug, &alice).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(listed["results"].as_array().unwrap().len(), 1);
    assert_eq!(listed["results"][0]["slug"], bob.slug);
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);
    assert!(after["results"].as_array().unwrap().is_empty());
    assert_eq!(visible_again, StatusCode::OK);
}

#[tokio::test]
async fn blocking_yourself_returns_400() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;

    // Act
    let response = block(&app, &alice, &alice.slug).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn unblocking_someone_who_is_not_blocked_returns_404() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    // Act
    let response = unblock(&app, &alice, &bob.slug).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn block_endpoints_require_authentication() {
    // Arrange
    let app = spawn_app().await;

    // Act
    let list = app
        .api_client
        .get(format!("{}/api/v1/block", &app.address))
        .send()
        .await
        .expect("Failed to list blocks");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(list.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_page_that_ends_exactly_on_the_limit_comes_without_a_cursor() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let carol = register_and_login(&app, "Carol").await;

    block(&app, &alice, &bob.slug).await;
    block(&app, &alice, &carol.slug).await;

    // Act
    let page: serde_json::Value = app
        .api_client
        .get(format!("{}/api/v1/block?limit=2", &app.address))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .send()
        .await
        .expect("Failed to list blocks")
        .json()
        .await
        .expect("block list not JSON");

    // Assert
    // A página fechou cheia, mas é o fim da lista: mandar cursor aqui faria o
    // cliente buscar uma página vazia.
    assert_eq!(2, page["results"].as_array().expect("results").len());
    assert!(page["next_cursor"].is_null());

    drop_app(app).await;
}
