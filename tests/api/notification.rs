use reqwest::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::friend::befriend;
use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};

async fn list(app: &TestApp, user: &RegisteredUser, query: &str) -> serde_json::Value {
    app.api_client
        .get(format!("{}/api/v1/notification?{}", &app.address, query))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to list notifications")
        .json()
        .await
        .expect("notification list not JSON")
}

/// Os `kind` recebidos por `user`, do mais novo para o mais antigo.
async fn kinds(app: &TestApp, user: &RegisteredUser) -> Vec<String> {
    let body = list(app, user, "").await;
    body["results"]
        .as_array()
        .expect("results não é lista")
        .iter()
        .map(|item| item["kind"].as_str().unwrap().to_string())
        .collect()
}

async fn unread_count(app: &TestApp, user: &RegisteredUser) -> i64 {
    let body: serde_json::Value = app
        .api_client
        .get(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to get me")
        .json()
        .await
        .expect("me not JSON");

    body["results"]["unread_notifications_count"]
        .as_i64()
        .expect("unread_notifications_count ausente")
}

async fn mark_read(app: &TestApp, user: &RegisteredUser, id: &str) -> reqwest::Response {
    app.api_client
        .post(format!("{}/api/v1/notification/{}/read", &app.address, id))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to mark as read")
}

async fn post_scrap(app: &TestApp, author: &RegisteredUser, slug: &str, body: &str) -> String {
    let response = app
        .api_client
        .post(format!("{}/api/v1/profile/{}/scrap", &app.address, slug))
        .header(reqwest::header::AUTHORIZATION, author.auth_header())
        .json(&json!({ "body": body }))
        .send()
        .await
        .expect("Failed to post scrap");
    assert_eq!(response.status(), StatusCode::CREATED);

    let body: serde_json::Value = response.json().await.expect("scrap not JSON");
    body["results"]["id"].as_str().unwrap().to_string()
}

async fn open_wall(app: &TestApp, owner: &RegisteredUser) {
    let response = app
        .api_client
        .put(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, owner.auth_header())
        .json(&json!({ "name": "Dono", "scrap_write": "everyone" }))
        .send()
        .await
        .expect("Failed to open wall");
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_friend_request_notifies_the_addressee_and_the_answer_notifies_back() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    // Act
    befriend(&app, &bob, &alice).await;

    let received_by_alice = list(&app, &alice, "").await;
    let received_by_bob = list(&app, &bob, "").await;

    // Tear down
    drop_app(app).await;

    // Assert
    let to_alice = received_by_alice["results"].as_array().unwrap();
    assert_eq!(to_alice.len(), 1);
    assert_eq!(to_alice[0]["kind"], "friend_request");
    assert_eq!(to_alice[0]["actor"]["slug"], bob.slug);
    assert_eq!(to_alice[0]["target_slug"], bob.slug);
    assert!(to_alice[0]["friend_request_id"].is_string());
    assert!(to_alice[0]["read_at"].is_null());

    let to_bob = received_by_bob["results"].as_array().unwrap();
    assert_eq!(to_bob.len(), 1);
    assert_eq!(to_bob[0]["kind"], "friend_accepted");
    assert_eq!(to_bob[0]["actor"]["slug"], alice.slug);
}

#[tokio::test]
async fn a_scrap_notifies_the_wall_owner_and_a_reply_notifies_the_scrap_author() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    open_wall(&app, &alice).await;

    // Act
    let scrap = post_scrap(&app, &bob, &alice.slug, "oi!").await;

    let response = app
        .api_client
        .post(format!("{}/api/v1/scrap/{}/reply", &app.address, scrap))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .json(&json!({ "body": "oi, tudo bem?" }))
        .send()
        .await
        .expect("Failed to reply");
    assert_eq!(response.status(), StatusCode::CREATED);

    let to_alice = list(&app, &alice, "").await;
    let to_bob = list(&app, &bob, "").await;

    // Tear down
    drop_app(app).await;

    // Assert
    let to_alice = to_alice["results"].as_array().unwrap();
    assert_eq!(to_alice.len(), 1);
    assert_eq!(to_alice[0]["kind"], "scrap_received");
    assert_eq!(to_alice[0]["actor"]["slug"], bob.slug);
    // O link leva para o mural onde o recado está, não para o autor
    assert_eq!(to_alice[0]["target_slug"], alice.slug);

    let to_bob = to_bob["results"].as_array().unwrap();
    assert_eq!(to_bob.len(), 1);
    assert_eq!(to_bob[0]["kind"], "scrap_reply");
    assert_eq!(to_bob[0]["actor"]["slug"], alice.slug);
    assert_eq!(to_bob[0]["target_slug"], alice.slug);
}

#[tokio::test]
async fn writing_on_your_own_wall_notifies_nobody() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;

    // Act
    post_scrap(&app, &alice, &alice.slug, "anotação minha").await;

    // Tear down
    let count = unread_count(&app, &alice).await;
    let received = kinds(&app, &alice).await;
    drop_app(app).await;

    // Assert
    assert_eq!(count, 0);
    assert!(received.is_empty());
}

#[tokio::test]
async fn a_testimonial_notifies_on_arrival_and_on_approval() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    befriend(&app, &bob, &alice).await;

    let response = app
        .api_client
        .post(format!(
            "{}/api/v1/profile/{}/testimonial",
            &app.address, alice.slug
        ))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .json(&json!({ "body": "gente boa" }))
        .send()
        .await
        .expect("Failed to write testimonial");
    assert_eq!(response.status(), StatusCode::CREATED);

    let created: serde_json::Value = response.json().await.expect("testimonial not JSON");
    let id = created["results"]["id"].as_str().unwrap().to_string();

    // Act
    let to_alice = kinds(&app, &alice).await;

    let response = app
        .api_client
        .post(format!("{}/api/v1/testimonial/{}/accept", &app.address, id))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .send()
        .await
        .expect("Failed to accept");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let to_bob = list(&app, &bob, "").await;

    // Tear down
    drop_app(app).await;

    // Assert
    // O friend_request do befriend também está na conta dela, mais antigo
    assert_eq!(to_alice, vec!["testimonial_received", "friend_request"]);

    let to_bob = to_bob["results"].as_array().unwrap();
    // friend_accepted da amizade + testimonial_accepted, o mais novo primeiro
    assert_eq!(to_bob[0]["kind"], "testimonial_accepted");
    assert_eq!(to_bob[0]["actor"]["slug"], alice.slug);
    assert_eq!(to_bob[0]["target_slug"], alice.slug);
    assert!(to_bob[0]["testimonial_id"].is_string());
}

#[tokio::test]
async fn rejecting_a_testimonial_notifies_nobody() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    befriend(&app, &bob, &alice).await;

    let created: serde_json::Value = app
        .api_client
        .post(format!(
            "{}/api/v1/profile/{}/testimonial",
            &app.address, alice.slug
        ))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .json(&json!({ "body": "gente boa" }))
        .send()
        .await
        .expect("Failed to write testimonial")
        .json()
        .await
        .expect("testimonial not JSON");
    let id = created["results"]["id"].as_str().unwrap().to_string();

    let before = kinds(&app, &bob).await;

    // Act
    let response = app
        .api_client
        .post(format!("{}/api/v1/testimonial/{}/reject", &app.address, id))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .send()
        .await
        .expect("Failed to reject");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let after = kinds(&app, &bob).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(before, after, "recusar não pode avisar o autor");
}

#[tokio::test]
async fn reading_a_notification_drops_the_unread_count() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    open_wall(&app, &alice).await;

    post_scrap(&app, &bob, &alice.slug, "primeiro").await;
    post_scrap(&app, &bob, &alice.slug, "segundo").await;

    let body = list(&app, &alice, "").await;
    let id = body["results"][0]["id"].as_str().unwrap().to_string();

    // Act
    let before = unread_count(&app, &alice).await;
    let marked = mark_read(&app, &alice, &id).await;
    let after = unread_count(&app, &alice).await;

    let unread_only = list(&app, &alice, "unread=true").await;

    // Marcar de novo não muda a data já registrada
    let read_at = list(&app, &alice, "").await["results"][0]["read_at"].clone();
    mark_read(&app, &alice, &id).await;
    let read_at_again = list(&app, &alice, "").await["results"][0]["read_at"].clone();

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(before, 2);
    assert_eq!(marked.status(), StatusCode::NO_CONTENT);
    assert_eq!(after, 1);
    assert_eq!(unread_only["results"].as_array().unwrap().len(), 1);
    assert_eq!(read_at, read_at_again);
}

#[tokio::test]
async fn read_all_clears_every_unread_notification() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    open_wall(&app, &alice).await;

    post_scrap(&app, &bob, &alice.slug, "primeiro").await;
    post_scrap(&app, &bob, &alice.slug, "segundo").await;

    // Act
    let response = app
        .api_client
        .post(format!("{}/api/v1/notification/read-all", &app.address))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .send()
        .await
        .expect("Failed to read all");
    let status = response.status();
    let body: serde_json::Value = response.json().await.expect("read-all not JSON");

    let count = unread_count(&app, &alice).await;
    let unread_only = list(&app, &alice, "unread=true").await;
    let all = list(&app, &alice, "").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["results"]["unread_count"], 0);
    assert_eq!(count, 0);
    assert!(unread_only["results"].as_array().unwrap().is_empty());
    assert_eq!(
        all["results"].as_array().unwrap().len(),
        2,
        "ler não apaga a notificação"
    );
}

#[tokio::test]
async fn blocking_hides_the_notifications_of_the_blocked_user() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let carol = register_and_login(&app, "Carol").await;
    open_wall(&app, &alice).await;

    post_scrap(&app, &bob, &alice.slug, "do bob").await;
    post_scrap(&app, &carol, &alice.slug, "da carol").await;

    let before = unread_count(&app, &alice).await;

    // Act
    let response = app
        .api_client
        .post(format!("{}/api/v1/block/{}", &app.address, bob.slug))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .send()
        .await
        .expect("Failed to block");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let after = unread_count(&app, &alice).await;
    let visible = list(&app, &alice, "").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(before, 2);
    assert_eq!(after, 1, "a notificação antiga do bloqueado some da conta");
    let visible = visible["results"].as_array().unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0]["actor"]["slug"], carol.slug);
}

#[tokio::test]
async fn deleting_the_scrap_deletes_its_notification() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    open_wall(&app, &alice).await;

    let scrap = post_scrap(&app, &bob, &alice.slug, "oi").await;

    // Act
    let response = app
        .api_client
        .delete(format!("{}/api/v1/scrap/{}", &app.address, scrap))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .send()
        .await
        .expect("Failed to delete scrap");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let count = unread_count(&app, &alice).await;
    let received = kinds(&app, &alice).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(count, 0);
    assert!(
        received.is_empty(),
        "a notificação some junto com o que a originou"
    );
}

#[tokio::test]
async fn a_notification_belongs_only_to_its_owner() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    open_wall(&app, &alice).await;

    post_scrap(&app, &bob, &alice.slug, "oi").await;
    let body = list(&app, &alice, "").await;
    let id = body["results"][0]["id"].as_str().unwrap().to_string();

    // Act
    let read_by_other = mark_read(&app, &bob, &id).await;
    let deleted_by_other = app
        .api_client
        .delete(format!("{}/api/v1/notification/{}", &app.address, id))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .send()
        .await
        .expect("Failed to delete notification");

    let deleted_by_owner = app
        .api_client
        .delete(format!("{}/api/v1/notification/{}", &app.address, id))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .send()
        .await
        .expect("Failed to delete notification");

    let left = kinds(&app, &alice).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(read_by_other.status(), StatusCode::NOT_FOUND);
    assert_eq!(deleted_by_other.status(), StatusCode::NOT_FOUND);
    assert_eq!(deleted_by_owner.status(), StatusCode::NO_CONTENT);
    assert!(left.is_empty());
}

#[tokio::test]
async fn notifications_walk_every_page_with_the_cursor() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    open_wall(&app, &alice).await;

    let mut expected = Vec::new();
    for i in 0..3 {
        post_scrap(&app, &bob, &alice.slug, &format!("recado {i}")).await;
        expected.push(i);
    }

    // Act
    let mut seen = Vec::new();
    let mut query = "limit=2".to_string();
    loop {
        let body = list(&app, &alice, &query).await;

        for item in body["results"].as_array().unwrap() {
            seen.push(item["id"].as_str().unwrap().to_string());
        }

        match body["next_cursor"].as_str() {
            Some(cursor) => query = format!("limit=2&cursor={cursor}"),
            None => break,
        }
    }

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(seen.len(), expected.len());
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), seen.len(), "a paginação repetiu itens");
}

#[tokio::test]
async fn unknown_notification_returns_404() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let unknown = Uuid::now_v7().to_string();

    // Act
    let marked = mark_read(&app, &alice, &unknown).await;
    let deleted = app
        .api_client
        .delete(format!("{}/api/v1/notification/{}", &app.address, unknown))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .send()
        .await
        .expect("Failed to delete notification");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(marked.status(), StatusCode::NOT_FOUND);
    assert_eq!(deleted.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn notifications_require_authentication() {
    // Arrange
    let app = spawn_app().await;

    // Act
    let response = app
        .api_client
        .get(format!("{}/api/v1/notification", &app.address))
        .send()
        .await
        .expect("Failed to list notifications");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
