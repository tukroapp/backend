use reqwest::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::friend::befriend;
use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};

async fn set_privacy(app: &TestApp, user: &RegisteredUser, value: &str) {
    let response = app
        .api_client
        .put(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .json(&json!({ "name": "Dono do perfil", "testimonial_write": value }))
        .send()
        .await
        .expect("Failed to update privacy");

    assert_eq!(response.status(), StatusCode::OK);
}

async fn write_testimonial(
    app: &TestApp,
    author: Option<&RegisteredUser>,
    slug: &str,
    body: &str,
) -> reqwest::Response {
    let mut request = app
        .api_client
        .post(format!(
            "{}/api/v1/profile/{}/testimonial",
            &app.address, slug
        ))
        .json(&json!({ "body": body }));

    if let Some(author) = author {
        request = request.header(reqwest::header::AUTHORIZATION, author.auth_header());
    }

    request.send().await.expect("Failed to write testimonial")
}

async fn list_public(app: &TestApp, slug: &str, query: &str) -> serde_json::Value {
    app.api_client
        .get(format!(
            "{}/api/v1/profile/{}/testimonial?{}",
            &app.address, slug, query
        ))
        .send()
        .await
        .expect("Failed to list testimonials")
        .json()
        .await
        .expect("testimonial list not JSON")
}

async fn list_inbox(app: &TestApp, user: &RegisteredUser, query: &str) -> serde_json::Value {
    app.api_client
        .get(format!("{}/api/v1/testimonial?{}", &app.address, query))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to list inbox")
        .json()
        .await
        .expect("inbox not JSON")
}

async fn respond(
    app: &TestApp,
    user: &RegisteredUser,
    id: &str,
    action: &str,
) -> reqwest::Response {
    app.api_client
        .post(format!(
            "{}/api/v1/testimonial/{}/{}",
            &app.address, id, action
        ))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to respond to testimonial")
}

async fn edit(app: &TestApp, user: &RegisteredUser, id: &str, body: &str) -> reqwest::Response {
    app.api_client
        .put(format!("{}/api/v1/testimonial/{}", &app.address, id))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .json(&json!({ "body": body }))
        .send()
        .await
        .expect("Failed to edit testimonial")
}

async fn remove(app: &TestApp, user: &RegisteredUser, id: &str) -> reqwest::Response {
    app.api_client
        .delete(format!("{}/api/v1/testimonial/{}", &app.address, id))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to delete testimonial")
}

/// Escreve um depoimento e devolve o id gerado. O padrão de `testimonial_write`
/// é `friends`, então os dois viram amigos antes.
async fn testimonial_id(
    app: &TestApp,
    author: &RegisteredUser,
    owner: &RegisteredUser,
    body: &str,
) -> String {
    let response = write_testimonial(app, Some(author), &owner.slug, body).await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let body: serde_json::Value = response.json().await.expect("testimonial not JSON");
    body["results"]["id"]
        .as_str()
        .expect("testimonial without id")
        .to_string()
}

async fn profile_of(
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
        .expect("Failed to get profile")
        .json()
        .await
        .expect("profile not JSON")
}

#[tokio::test]
async fn a_new_testimonial_stays_pending_until_accepted() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    befriend(&app, &bob, &alice).await;

    // Act
    let id = testimonial_id(&app, &bob, &alice, "  a Alice é gente boa  ").await;

    let before = list_public(&app, &alice.slug, "").await;
    let inbox = list_inbox(&app, &alice, "status=pending").await;

    let accepted = respond(&app, &alice, &id, "accept").await;
    let after = list_public(&app, &alice.slug, "").await;
    let profile = profile_of(&app, &alice.slug, None).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert!(
        before["results"].as_array().unwrap().is_empty(),
        "depoimento pendente não pode aparecer no perfil"
    );
    assert_eq!(inbox["results"].as_array().unwrap().len(), 1);
    assert_eq!(inbox["results"][0]["profile"]["slug"], bob.slug);
    assert_eq!(inbox["results"][0]["body"], "a Alice é gente boa");
    assert!(inbox["results"][0]["responded_at"].is_null());

    assert_eq!(accepted.status(), StatusCode::NO_CONTENT);
    assert_eq!(after["results"].as_array().unwrap().len(), 1);
    assert_eq!(after["results"][0]["status"], "accepted");
    assert_eq!(after["results"][0]["profile"]["slug"], bob.slug);
    assert!(!after["results"][0]["responded_at"].is_null());
    assert_eq!(profile["results"]["testimonials_count"], 1);
}

#[tokio::test]
async fn a_rejected_testimonial_never_shows_up_on_the_profile() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    befriend(&app, &bob, &alice).await;

    let id = testimonial_id(&app, &bob, &alice, "texto qualquer").await;

    // Act
    let rejected = respond(&app, &alice, &id, "reject").await;
    let public = list_public(&app, &alice.slug, "").await;
    let sent = list_inbox(&app, &bob, "direction=sent").await;
    let profile = profile_of(&app, &alice.slug, None).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(rejected.status(), StatusCode::NO_CONTENT);
    assert!(public["results"].as_array().unwrap().is_empty());
    assert_eq!(profile["results"]["testimonials_count"], 0);

    // O autor enxerga a recusa na lista dos depoimentos que escreveu
    assert_eq!(sent["results"].as_array().unwrap().len(), 1);
    assert_eq!(sent["results"][0]["status"], "rejected");
    assert_eq!(sent["results"][0]["profile"]["slug"], alice.slug);
}

#[tokio::test]
async fn responding_twice_returns_404() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    befriend(&app, &bob, &alice).await;

    let id = testimonial_id(&app, &bob, &alice, "texto qualquer").await;
    respond(&app, &alice, &id, "accept").await;

    // Act
    let again = respond(&app, &alice, &id, "reject").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(again.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn only_the_profile_owner_can_respond() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let carol = register_and_login(&app, "Carol").await;
    befriend(&app, &bob, &alice).await;

    let id = testimonial_id(&app, &bob, &alice, "texto qualquer").await;

    // Act
    let by_author = respond(&app, &bob, &id, "accept").await;
    let by_stranger = respond(&app, &carol, &id, "accept").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(by_author.status(), StatusCode::NOT_FOUND);
    assert_eq!(by_stranger.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn editing_an_accepted_testimonial_sends_it_back_to_moderation() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    befriend(&app, &bob, &alice).await;

    let id = testimonial_id(&app, &bob, &alice, "texto original").await;
    respond(&app, &alice, &id, "accept").await;

    // Act
    let edited = edit(&app, &bob, &id, "texto editado").await;
    let edited_status = edited.status();
    let edited: serde_json::Value = edited.json().await.expect("testimonial not JSON");

    let public = list_public(&app, &alice.slug, "").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(edited_status, StatusCode::OK);
    assert_eq!(edited["results"]["body"], "texto editado");
    assert_eq!(edited["results"]["status"], "pending");
    assert!(edited["results"]["responded_at"].is_null());
    assert!(
        public["results"].as_array().unwrap().is_empty(),
        "o texto editado precisa ser aprovado de novo"
    );
}

#[tokio::test]
async fn only_the_author_can_edit() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    befriend(&app, &bob, &alice).await;

    let id = testimonial_id(&app, &bob, &alice, "texto original").await;

    // Act
    let by_profile_owner = edit(&app, &alice, &id, "texto de outro").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(by_profile_owner.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn author_and_profile_owner_can_delete() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let carol = register_and_login(&app, "Carol").await;
    befriend(&app, &bob, &alice).await;
    befriend(&app, &carol, &alice).await;

    let by_bob = testimonial_id(&app, &bob, &alice, "depoimento do bob").await;
    let by_carol = testimonial_id(&app, &carol, &alice, "depoimento da carol").await;

    // Act
    let by_stranger = remove(&app, &carol, &by_bob).await;
    let by_author = remove(&app, &bob, &by_bob).await;
    let by_profile_owner = remove(&app, &alice, &by_carol).await;
    let inbox = list_inbox(&app, &alice, "").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(by_stranger.status(), StatusCode::NOT_FOUND);
    assert_eq!(by_author.status(), StatusCode::NO_CONTENT);
    assert_eq!(by_profile_owner.status(), StatusCode::NO_CONTENT);
    assert!(inbox["results"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn the_same_author_can_write_several_testimonials() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    befriend(&app, &bob, &alice).await;

    // Act
    let first = testimonial_id(&app, &bob, &alice, "não aceita esse ainda").await;
    let second = testimonial_id(&app, &bob, &alice, "agora sim, pode aceitar").await;

    // Cada um passa pela moderação sozinho
    assert_eq!(
        respond(&app, &alice, &second, "accept").await.status(),
        StatusCode::NO_CONTENT
    );

    let inbox = list_inbox(&app, &alice, "status=pending").await;
    let public = list_public(&app, &alice.slug, "").await;
    let profile = profile_of(&app, &alice.slug, None).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_ne!(first, second);
    assert_eq!(inbox["results"].as_array().unwrap().len(), 1);
    assert_eq!(inbox["results"][0]["id"], first);

    assert_eq!(public["results"].as_array().unwrap().len(), 1);
    assert_eq!(public["results"][0]["id"], second);
    assert_eq!(profile["results"]["testimonials_count"], 1);
}

#[tokio::test]
async fn testimonial_write_privacy_is_respected() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let carol = register_and_login(&app, "Carol").await;
    befriend(&app, &bob, &alice).await;

    // O padrão já é `friends`, mas deixa explícito
    set_privacy(&app, &alice, "friends").await;

    // Act
    let friend = write_testimonial(&app, Some(&bob), &alice.slug, "sou amigo").await;
    let stranger = write_testimonial(&app, Some(&carol), &alice.slug, "sou estranho").await;
    let seen_by_stranger = profile_of(&app, &alice.slug, Some(&carol)).await;

    set_privacy(&app, &alice, "nobody").await;
    let after_closing = write_testimonial(&app, Some(&carol), &alice.slug, "e agora?").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(friend.status(), StatusCode::CREATED);
    assert_eq!(stranger.status(), StatusCode::FORBIDDEN);
    assert_eq!(seen_by_stranger["results"]["can_write_testimonial"], false);
    assert_eq!(after_closing.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn nobody_writes_a_testimonial_for_themselves() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;

    // Act
    let response = write_testimonial(&app, Some(&alice), &alice.slug, "eu sou ótima").await;
    let profile = profile_of(&app, &alice.slug, Some(&alice)).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(profile["results"]["can_write_testimonial"], false);
}

#[tokio::test]
async fn anonymous_cannot_write_a_testimonial() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;

    // Act
    let response = write_testimonial(&app, None, &alice.slug, "oi").await;
    let profile = profile_of(&app, &alice.slug, None).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(profile["results"]["can_write_testimonial"], false);
}

#[tokio::test]
async fn empty_testimonial_returns_400() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    befriend(&app, &bob, &alice).await;

    // Act
    let response = write_testimonial(&app, Some(&bob), &alice.slug, "   ").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn blocked_user_cannot_write_a_testimonial() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    let response = app
        .api_client
        .post(format!("{}/api/v1/block/{}", &app.address, bob.slug))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .send()
        .await
        .expect("Failed to block");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // Act
    let response = write_testimonial(&app, Some(&bob), &alice.slug, "oi").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn inbox_rejects_unknown_filters() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;

    // Act
    let direction = app
        .api_client
        .get(format!(
            "{}/api/v1/testimonial?direction=both",
            &app.address
        ))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .send()
        .await
        .expect("Failed to list inbox");

    let status = app
        .api_client
        .get(format!(
            "{}/api/v1/testimonial?status=maybe&limit=10",
            &app.address
        ))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .send()
        .await
        .expect("Failed to list inbox");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(direction.status(), StatusCode::BAD_REQUEST);
    assert_eq!(status.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn public_list_walks_every_page_with_the_cursor() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    set_privacy(&app, &alice, "everyone").await;

    let mut expected = Vec::new();
    for i in 0..3 {
        let author = register_and_login(&app, &format!("Autor {i}")).await;
        let id = testimonial_id(&app, &author, &alice, &format!("depoimento {i}")).await;
        assert_eq!(
            respond(&app, &alice, &id, "accept").await.status(),
            StatusCode::NO_CONTENT
        );
        expected.push(id);
    }

    // Act
    let mut seen = Vec::new();
    let mut query = "limit=2".to_string();
    loop {
        let body = list_public(&app, &alice.slug, &query).await;

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
    seen.sort();
    expected.sort();
    assert_eq!(seen, expected);
}

#[tokio::test]
async fn unknown_testimonial_returns_404() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let unknown = Uuid::now_v7().to_string();

    // Act
    let edited = edit(&app, &alice, &unknown, "oi").await;
    let removed = remove(&app, &alice, &unknown).await;
    let responded = respond(&app, &alice, &unknown, "accept").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(edited.status(), StatusCode::NOT_FOUND);
    assert_eq!(removed.status(), StatusCode::NOT_FOUND);
    assert_eq!(responded.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deleting_a_testimonial_keeps_the_row_for_moderation() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    befriend(&app, &bob, &alice).await;

    let id = testimonial_id(&app, &bob, &alice, "conteúdo problemático").await;
    assert_eq!(
        respond(&app, &alice, &id, "accept").await.status(),
        StatusCode::NO_CONTENT
    );

    // Act
    let deleted = remove(&app, &bob, &id).await;

    let public = list_public(&app, &alice.slug, "").await;
    let inbox = list_inbox(&app, &alice, "").await;
    let sent = list_inbox(&app, &bob, "direction=sent").await;
    let profile = profile_of(&app, &alice.slug, None).await;

    let stored = sqlx::query!(
        r#"SELECT body, deleted_at FROM testimonial WHERE id = $1"#,
        Uuid::parse_str(&id).unwrap(),
    )
    .fetch_one(&app.db_pool)
    .await
    .expect("o depoimento não pode sair do banco");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert!(public["results"].as_array().unwrap().is_empty());
    assert!(inbox["results"].as_array().unwrap().is_empty());
    assert!(sent["results"].as_array().unwrap().is_empty());
    assert_eq!(profile["results"]["testimonials_count"], 0);

    assert_eq!(stored.body, "conteúdo problemático");
    assert!(stored.deleted_at.is_some());
}

async fn set_profile_visibility(app: &TestApp, user: &RegisteredUser, value: &str) {
    let response = app
        .api_client
        .put(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .json(&json!({ "name": "Dono do perfil", "profile_visibility": value }))
        .send()
        .await
        .expect("Failed to update visibility");
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn testimonials_follow_the_profile_privacy() {
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let carol = register_and_login(&app, "Carol").await;
    befriend(&app, &bob, &alice).await;

    let id = testimonial_id(&app, &bob, &alice, "a Alice é gente boa").await;
    respond(&app, &alice, &id, "accept").await;

    // Perfil público: qualquer um vê.
    let response = app
        .api_client
        .get(format!("{}/api/v1/profile/{}/testimonial", app.address, alice.slug))
        .send()
        .await
        .expect("list");
    assert_eq!(response.status(), StatusCode::OK);

    set_profile_visibility(&app, &alice, "friends").await;

    // Estranho (e anônimo): 403.
    let stranger = app
        .api_client
        .get(format!("{}/api/v1/profile/{}/testimonial", app.address, alice.slug))
        .header(reqwest::header::AUTHORIZATION, carol.auth_header())
        .send()
        .await
        .expect("list");
    assert_eq!(stranger.status(), StatusCode::FORBIDDEN);

    let anon = app
        .api_client
        .get(format!("{}/api/v1/profile/{}/testimonial", app.address, alice.slug))
        .send()
        .await
        .expect("list");
    assert_eq!(anon.status(), StatusCode::FORBIDDEN);

    // Amigo continua vendo.
    let friend = app
        .api_client
        .get(format!("{}/api/v1/profile/{}/testimonial", app.address, alice.slug))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .send()
        .await
        .expect("list");
    assert_eq!(friend.status(), StatusCode::OK);
    assert_eq!(json_body(friend).await["results"].as_array().unwrap().len(), 1);

    // E o contador no perfil some para o estranho.
    let seen_by_stranger = profile_of(&app, &alice.slug, Some(&carol)).await;
    assert_eq!(seen_by_stranger["results"]["testimonials_visible"], json!(false));
    assert_eq!(seen_by_stranger["results"]["testimonials_count"], serde_json::Value::Null);

    drop_app(app).await;
}

async fn json_body(response: reqwest::Response) -> serde_json::Value {
    response.json().await.expect("not JSON")
}
