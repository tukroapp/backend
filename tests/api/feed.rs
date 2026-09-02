use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};

async fn json_of(response: reqwest::Response) -> Value {
    response.json().await.expect("response not JSON")
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

async fn become_fan(app: &TestApp, fan: &RegisteredUser, idol_slug: &str) {
    let response = app
        .api_client
        .post(format!("{}/api/v1/profile/{}/fan", app.address, idol_slug))
        .header(reqwest::header::AUTHORIZATION, fan.auth_header())
        .send()
        .await
        .expect("Failed to become fan");
    assert_eq!(response.status(), StatusCode::OK);
}

async fn update_profile(app: &TestApp, user: &RegisteredUser, body: Value) {
    let response = app
        .api_client
        .put(format!("{}/api/v1/me", app.address))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .json(&body)
        .send()
        .await
        .expect("Failed to update profile");
    assert_eq!(response.status(), StatusCode::OK);
}

async fn feed(app: &TestApp, user: &RegisteredUser) -> Value {
    let response = app
        .api_client
        .get(format!("{}/api/v1/me/feed", app.address))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to get feed");
    assert_eq!(response.status(), StatusCode::OK);
    json_of(response).await
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
async fn a_friends_activity_shows_in_the_feed() {
    let app = spawn_app().await;
    let me = register_and_login(&app, "Eu").await;
    let alice = register_and_login(&app, "Alice").await;
    let star = register_and_login(&app, "Star").await;
    befriend(&app, &me, &alice).await;

    become_fan(&app, &alice, &star.slug).await;

    let items = feed(&app, &me).await["results"].as_array().unwrap().clone();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["kind"], json!("became_fan"));
    assert_eq!(items[0]["actor"]["slug"], json!(alice.slug));
    assert_eq!(items[0]["target_user"]["slug"], json!(star.slug));

    drop_app(app).await;
}

#[tokio::test]
async fn an_idols_activity_needs_a_public_profile() {
    let app = spawn_app().await;
    let me = register_and_login(&app, "Fan").await;
    let vitoria = register_and_login(&app, "Vitoria").await;
    let other = register_and_login(&app, "Other").await;
    become_fan(&app, &me, &vitoria.slug).await;

    // Perfil público por padrão: a atividade da Vitória chega.
    become_fan(&app, &vitoria, &other.slug).await;
    assert_eq!(
        feed(&app, &me).await["results"].as_array().unwrap().len(),
        1
    );

    // Ela fecha o perfil para amigos: a visibilidade é avaliada na leitura, então
    // até a atividade antiga some para o fã que não é amigo.
    update_profile(
        &app,
        &vitoria,
        json!({ "name": "Vitoria", "profile_visibility": "friends" }),
    )
    .await;

    assert!(
        feed(&app, &me).await["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    drop_app(app).await;
}

#[tokio::test]
async fn a_fan_who_is_also_a_friend_still_sees_a_private_idol() {
    let app = spawn_app().await;
    let me = register_and_login(&app, "Fan").await;
    let vitoria = register_and_login(&app, "Vitoria").await;
    let other = register_and_login(&app, "Other").await;
    become_fan(&app, &me, &vitoria.slug).await;
    befriend(&app, &me, &vitoria).await;

    update_profile(
        &app,
        &vitoria,
        json!({ "name": "Vitoria", "profile_visibility": "friends" }),
    )
    .await;
    become_fan(&app, &vitoria, &other.slug).await;

    assert_eq!(
        feed(&app, &me).await["results"].as_array().unwrap().len(),
        1
    );

    drop_app(app).await;
}

#[tokio::test]
async fn my_own_activity_and_strangers_activity_are_excluded() {
    let app = spawn_app().await;
    let me = register_and_login(&app, "Eu").await;
    let stranger = register_and_login(&app, "Estranho").await;
    let star = register_and_login(&app, "Star").await;

    become_fan(&app, &me, &star.slug).await;
    become_fan(&app, &stranger, &star.slug).await;

    assert!(
        feed(&app, &me).await["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    drop_app(app).await;
}

#[tokio::test]
async fn the_new_counter_tracks_unseen_activity() {
    let app = spawn_app().await;
    let me = register_and_login(&app, "Eu").await;
    let alice = register_and_login(&app, "Alice").await;
    let a = register_and_login(&app, "A").await;
    let b = register_and_login(&app, "B").await;
    befriend(&app, &me, &alice).await;

    become_fan(&app, &alice, &a.slug).await;
    become_fan(&app, &alice, &b.slug).await;

    assert_eq!(unread(&app, &me).await["results"]["feed"], json!(2));

    let seen = app
        .api_client
        .post(format!("{}/api/v1/me/feed/seen", app.address))
        .header(reqwest::header::AUTHORIZATION, me.auth_header())
        .send()
        .await
        .expect("Failed to mark seen");
    assert_eq!(seen.status(), StatusCode::NO_CONTENT);

    assert_eq!(unread(&app, &me).await["results"]["feed"], json!(0));

    // O feed em si continua trazendo tudo, o "seen" só zera o contador.
    assert_eq!(
        feed(&app, &me).await["results"].as_array().unwrap().len(),
        2
    );

    drop_app(app).await;
}

#[tokio::test]
async fn changing_the_status_shows_up_once() {
    let app = spawn_app().await;
    let me = register_and_login(&app, "Eu").await;
    let alice = register_and_login(&app, "Alice").await;
    befriend(&app, &me, &alice).await;

    update_profile(
        &app,
        &alice,
        json!({ "name": "Alice", "status": "de férias!" }),
    )
    .await;
    // Segundo PUT sem mudar o status não gera atividade nova.
    update_profile(
        &app,
        &alice,
        json!({ "name": "Alice", "status": "de férias!", "city": "Recife" }),
    )
    .await;

    let items = feed(&app, &me).await["results"].as_array().unwrap().clone();
    let status_items: Vec<&Value> = items
        .iter()
        .filter(|item| item["kind"] == json!("status_changed"))
        .collect();
    assert_eq!(status_items.len(), 1);
    assert_eq!(status_items[0]["text"], json!("de férias!"));

    drop_app(app).await;
}

#[tokio::test]
async fn adding_media_shows_up_with_the_file() {
    let app = spawn_app().await;
    let me = register_and_login(&app, "Eu").await;
    let alice = register_and_login(&app, "Alice").await;
    befriend(&app, &me, &alice).await;

    let png = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0xDA, 0x63, 0xFC,
        0xCF, 0xC0, 0x50, 0x0F, 0x00, 0x04, 0x85, 0x01, 0x80, 0x84, 0xA9, 0x8C, 0x21, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    let part = reqwest::multipart::Part::bytes(png).file_name("foto.png");
    let form = reqwest::multipart::Form::new().part("file", part);
    let upload = app
        .api_client
        .post(format!("{}/api/v1/me/media", app.address))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .multipart(form)
        .send()
        .await
        .expect("Failed to upload");
    assert_eq!(upload.status(), StatusCode::CREATED);

    let items = feed(&app, &me).await["results"].as_array().unwrap().clone();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["kind"], json!("media_added"));
    assert_eq!(items[0]["media"]["kind"], json!("photo"));
    assert!(
        items[0]["media"]["url"]
            .as_str()
            .unwrap()
            .contains("/media/")
    );

    drop_app(app).await;
}
