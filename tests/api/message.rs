use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};

async fn json_of(response: reqwest::Response) -> Value {
    response.json().await.expect("response not JSON")
}

async fn open_messages(app: &TestApp, user: &RegisteredUser) {
    let response = app
        .api_client
        .put(format!("{}/api/v1/me", app.address))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .json(&json!({ "name": "Fulano", "message_receive": "everyone" }))
        .send()
        .await
        .expect("Failed to update profile");
    assert_eq!(response.status(), StatusCode::OK);
}

async fn befriend(app: &TestApp, a: &RegisteredUser, b: &RegisteredUser) {
    sqlx::query(
        r#"
        INSERT INTO friend (user_id, friend_id)
        SELECT p1.user_id, p2.user_id FROM user_profile p1, user_profile p2
        WHERE p1.slug = $1 AND p2.slug = $2
        UNION ALL
        SELECT p2.user_id, p1.user_id FROM user_profile p1, user_profile p2
        WHERE p1.slug = $1 AND p2.slug = $2
        "#,
    )
    .bind(&a.slug)
    .bind(&b.slug)
    .execute(&app.db_pool)
    .await
    .expect("Failed to befriend");
}

async fn send(app: &TestApp, from: &RegisteredUser, to_slug: &str, body: &str) -> reqwest::Response {
    app.api_client
        .post(format!("{}/api/v1/profile/{}/message", app.address, to_slug))
        .header(reqwest::header::AUTHORIZATION, from.auth_header())
        .json(&json!({ "body": body }))
        .send()
        .await
        .expect("Failed to send message")
}

async fn inbox(app: &TestApp, user: &RegisteredUser) -> Value {
    let response = app
        .api_client
        .get(format!("{}/api/v1/me/conversation", app.address))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to list conversations");
    assert_eq!(response.status(), StatusCode::OK);
    json_of(response).await
}

async fn conversation_messages(app: &TestApp, user: &RegisteredUser, id: &str) -> reqwest::Response {
    app.api_client
        .get(format!(
            "{}/api/v1/me/conversation/{}/message",
            app.address, id
        ))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to list messages")
}

async fn mark_read(app: &TestApp, user: &RegisteredUser, id: &str) {
    let response = app
        .api_client
        .post(format!("{}/api/v1/me/conversation/{}/read", app.address, id))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to mark read");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

async fn unread(app: &TestApp, user: &RegisteredUser) -> Value {
    let response = app
        .api_client
        .get(format!("{}/api/v1/me/unread", app.address))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to get unread");
    assert_eq!(response.status(), StatusCode::OK);
    json_of(response).await
}

#[tokio::test]
async fn sending_creates_a_conversation_seen_by_both_sides() {
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    open_messages(&app, &bob).await;

    let response = send(&app, &alice, &bob.slug, "oi sumido").await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let sent = json_of(response).await;
    assert_eq!(sent["results"]["from_me"], json!(true));
    let conversation_id = sent["results"]["conversation_id"].as_str().unwrap().to_string();

    let bob_inbox = inbox(&app, &bob).await;
    let convs = bob_inbox["results"].as_array().unwrap();
    assert_eq!(convs.len(), 1);
    assert_eq!(convs[0]["id"], json!(conversation_id));
    assert_eq!(convs[0]["counterpart"]["slug"], json!(alice.slug));
    assert_eq!(convs[0]["unread_count"], json!(1));
    assert_eq!(convs[0]["last_message"]["preview"], json!("oi sumido"));
    assert_eq!(convs[0]["last_message"]["from_me"], json!(false));

    let alice_inbox = inbox(&app, &alice).await;
    assert_eq!(alice_inbox["results"][0]["unread_count"], json!(0));
    assert_eq!(alice_inbox["results"][0]["last_message"]["from_me"], json!(true));

    assert_eq!(unread(&app, &bob).await["results"]["messages"], json!(1));
    assert_eq!(unread(&app, &alice).await["results"]["messages"], json!(0));

    drop_app(app).await;
}

#[tokio::test]
async fn message_receive_friends_blocks_strangers_but_not_friends() {
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    // Default é 'friends' e eles não são amigos.
    let blocked = send(&app, &alice, &bob.slug, "oi").await;
    assert_eq!(blocked.status(), StatusCode::FORBIDDEN);

    befriend(&app, &alice, &bob).await;
    let ok = send(&app, &alice, &bob.slug, "oi de novo").await;
    assert_eq!(ok.status(), StatusCode::CREATED);

    drop_app(app).await;
}

#[tokio::test]
async fn cannot_message_yourself_or_without_a_session() {
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;

    let selfie = send(&app, &alice, &alice.slug, "eu mesmo").await;
    assert_eq!(selfie.status(), StatusCode::BAD_REQUEST);

    let anon = app
        .api_client
        .post(format!("{}/api/v1/profile/{}/message", app.address, alice.slug))
        .json(&json!({ "body": "oi" }))
        .send()
        .await
        .expect("Failed to send");
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);

    drop_app(app).await;
}

#[tokio::test]
async fn a_blocked_user_cannot_send() {
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    open_messages(&app, &bob).await;

    app.api_client
        .post(format!("{}/api/v1/block/{}", app.address, alice.slug))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .send()
        .await
        .expect("Failed to block");

    let response = send(&app, &alice, &bob.slug, "oi").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    drop_app(app).await;
}

#[tokio::test]
async fn reading_is_explicit_and_fetching_messages_has_no_side_effect() {
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    open_messages(&app, &bob).await;

    let id = json_of(send(&app, &alice, &bob.slug, "primeira").await).await["results"]
        ["conversation_id"]
        .as_str()
        .unwrap()
        .to_string();
    send(&app, &alice, &bob.slug, "segunda").await;

    // Buscar as mensagens não marca lido.
    let listed = conversation_messages(&app, &bob, &id).await;
    assert_eq!(listed.status(), StatusCode::OK);
    let messages = json_of(listed).await;
    assert_eq!(messages["results"].as_array().unwrap().len(), 2);
    // Ordem: mais nova primeiro.
    assert_eq!(messages["results"][0]["body"], json!("segunda"));
    assert_eq!(inbox(&app, &bob).await["results"][0]["unread_count"], json!(2));

    mark_read(&app, &bob, &id).await;
    assert_eq!(inbox(&app, &bob).await["results"][0]["unread_count"], json!(0));
    assert_eq!(unread(&app, &bob).await["results"]["messages"], json!(0));

    drop_app(app).await;
}

#[tokio::test]
async fn with_slug_is_404_until_the_first_message() {
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    open_messages(&app, &bob).await;

    let before = app
        .api_client
        .get(format!(
            "{}/api/v1/me/conversation/with/{}",
            app.address, bob.slug
        ))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .send()
        .await
        .expect("with slug");
    assert_eq!(before.status(), StatusCode::NOT_FOUND);

    send(&app, &alice, &bob.slug, "oi").await;

    let after = app
        .api_client
        .get(format!(
            "{}/api/v1/me/conversation/with/{}",
            app.address, bob.slug
        ))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .send()
        .await
        .expect("with slug");
    assert_eq!(after.status(), StatusCode::OK);
    assert_eq!(
        json_of(after).await["results"]["counterpart"]["slug"],
        json!(bob.slug)
    );

    drop_app(app).await;
}

#[tokio::test]
async fn hiding_a_conversation_removes_it_until_a_new_message_arrives() {
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    open_messages(&app, &bob).await;
    open_messages(&app, &alice).await;

    let id = json_of(send(&app, &alice, &bob.slug, "oi").await).await["results"]["conversation_id"]
        .as_str()
        .unwrap()
        .to_string();

    let hide = app
        .api_client
        .delete(format!("{}/api/v1/me/conversation/{}", app.address, id))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .send()
        .await
        .expect("hide");
    assert_eq!(hide.status(), StatusCode::NO_CONTENT);
    assert!(inbox(&app, &bob).await["results"].as_array().unwrap().is_empty());

    send(&app, &alice, &bob.slug, "voltei").await;
    let convs = inbox(&app, &bob).await;
    assert_eq!(convs["results"].as_array().unwrap().len(), 1);
    assert_eq!(convs["results"][0]["last_message"]["preview"], json!("voltei"));

    drop_app(app).await;
}

#[tokio::test]
async fn a_non_participant_cannot_read_a_conversation() {
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let mallory = register_and_login(&app, "Mallory").await;
    open_messages(&app, &bob).await;

    let id = json_of(send(&app, &alice, &bob.slug, "segredo").await).await["results"]
        ["conversation_id"]
        .as_str()
        .unwrap()
        .to_string();

    let response = conversation_messages(&app, &mallory, &id).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    drop_app(app).await;
}

#[tokio::test]
async fn a_reply_lands_in_the_same_conversation() {
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    befriend(&app, &alice, &bob).await;

    let id = json_of(send(&app, &alice, &bob.slug, "oi").await).await["results"]["conversation_id"]
        .as_str()
        .unwrap()
        .to_string();
    let reply = json_of(send(&app, &bob, &alice.slug, "oi de volta").await).await;
    assert_eq!(reply["results"]["conversation_id"], json!(id));

    let messages = json_of(conversation_messages(&app, &alice, &id).await).await;
    assert_eq!(messages["results"].as_array().unwrap().len(), 2);
    assert_eq!(inbox(&app, &alice).await["results"][0]["unread_count"], json!(1));

    drop_app(app).await;
}

#[tokio::test]
async fn me_exposes_message_receive_and_unread_count() {
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    befriend(&app, &alice, &bob).await;

    let me = app
        .api_client
        .get(format!("{}/api/v1/me", app.address))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .send()
        .await
        .expect("me");
    let body = json_of(me).await;
    assert_eq!(body["results"]["message_receive"], json!("friends"));
    assert_eq!(body["results"]["unread_messages_count"], json!(0));

    send(&app, &alice, &bob.slug, "oi").await;

    let me = app
        .api_client
        .get(format!("{}/api/v1/me", app.address))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .send()
        .await
        .expect("me");
    assert_eq!(json_of(me).await["results"]["unread_messages_count"], json!(1));

    drop_app(app).await;
}
