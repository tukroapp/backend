use reqwest::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::friend::befriend;
use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};

async fn set_privacy(app: &TestApp, user: &RegisteredUser, field: &str, value: &str) {
    let response = app
        .api_client
        .put(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .json(&json!({ "name": "Dono do mural", field: value }))
        .send()
        .await
        .expect("Failed to update privacy");

    assert_eq!(response.status(), StatusCode::OK);
}

/// O padrão de `scrap_write` é `friends`, então os testes com estranhos abrem o
/// mural antes.
async fn open_wall(app: &TestApp, owner: &RegisteredUser) {
    set_privacy(app, owner, "scrap_write", "everyone").await;
}

async fn post_scrap(
    app: &TestApp,
    author: Option<&RegisteredUser>,
    slug: &str,
    body: &str,
) -> reqwest::Response {
    let mut request = app
        .api_client
        .post(format!("{}/api/v1/profile/{}/scrap", &app.address, slug))
        .json(&json!({ "body": body }));

    if let Some(author) = author {
        request = request.header(reqwest::header::AUTHORIZATION, author.auth_header());
    }

    request.send().await.expect("Failed to post scrap")
}

async fn list_wall(
    app: &TestApp,
    slug: &str,
    viewer: Option<&RegisteredUser>,
    query: &str,
) -> reqwest::Response {
    let mut request = app.api_client.get(format!(
        "{}/api/v1/profile/{}/scrap?{}",
        &app.address, slug, query
    ));

    if let Some(viewer) = viewer {
        request = request.header(reqwest::header::AUTHORIZATION, viewer.auth_header());
    }

    request.send().await.expect("Failed to list wall")
}

async fn post_reply(
    app: &TestApp,
    author: &RegisteredUser,
    scrap_id: &str,
    body: &str,
) -> reqwest::Response {
    app.api_client
        .post(format!("{}/api/v1/scrap/{}/reply", &app.address, scrap_id))
        .header(reqwest::header::AUTHORIZATION, author.auth_header())
        .json(&json!({ "body": body }))
        .send()
        .await
        .expect("Failed to reply scrap")
}

async fn list_replies(
    app: &TestApp,
    scrap_id: &str,
    viewer: Option<&RegisteredUser>,
) -> reqwest::Response {
    let mut request = app
        .api_client
        .get(format!("{}/api/v1/scrap/{}/reply", &app.address, scrap_id));

    if let Some(viewer) = viewer {
        request = request.header(reqwest::header::AUTHORIZATION, viewer.auth_header());
    }

    request.send().await.expect("Failed to list replies")
}

async fn delete_scrap(app: &TestApp, user: &RegisteredUser, scrap_id: &str) -> reqwest::Response {
    app.api_client
        .delete(format!("{}/api/v1/scrap/{}", &app.address, scrap_id))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to delete scrap")
}

/// Publica um recado e devolve o id gerado.
async fn scrap_id(app: &TestApp, author: &RegisteredUser, slug: &str, body: &str) -> String {
    let response = post_scrap(app, Some(author), slug, body).await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let body: serde_json::Value = response.json().await.expect("scrap response not JSON");
    body["results"]["id"]
        .as_str()
        .expect("scrap without id")
        .to_string()
}

#[tokio::test]
async fn posting_a_scrap_puts_it_on_the_wall() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    open_wall(&app, &alice).await;

    // Act
    let created = post_scrap(&app, Some(&bob), &alice.slug, "  oi, sumida!  ").await;
    let status = created.status();
    let created: serde_json::Value = created.json().await.expect("scrap response not JSON");

    let wall: serde_json::Value = list_wall(&app, &alice.slug, None, "")
        .await
        .json()
        .await
        .expect("wall not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["results"]["body"], "oi, sumida!");
    assert_eq!(created["results"]["author"]["slug"], bob.slug);
    assert_eq!(created["results"]["reply_count"], 0);

    assert_eq!(wall["results"].as_array().unwrap().len(), 1);
    assert_eq!(wall["results"][0]["author"]["slug"], bob.slug);
}

#[tokio::test]
async fn anonymous_cannot_post_a_scrap() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;

    // Act
    let response = post_scrap(&app, None, &alice.slug, "oi").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn posting_an_empty_scrap_returns_400() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    // Act
    let response = post_scrap(&app, Some(&bob), &alice.slug, "   ").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn scrap_read_nobody_hides_the_wall_from_everyone_but_the_owner() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    scrap_id(&app, &alice, &alice.slug, "recado meu").await;
    set_privacy(&app, &alice, "scrap_read", "nobody").await;

    // Act
    let anonymous = list_wall(&app, &alice.slug, None, "").await;
    let stranger = list_wall(&app, &alice.slug, Some(&bob), "").await;
    let owner = list_wall(&app, &alice.slug, Some(&alice), "").await;
    let profile: serde_json::Value = app
        .api_client
        .get(format!("{}/api/v1/profile/{}", &app.address, alice.slug))
        .send()
        .await
        .expect("Failed to get profile")
        .json()
        .await
        .expect("profile not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(anonymous.status(), StatusCode::FORBIDDEN);
    assert_eq!(stranger.status(), StatusCode::FORBIDDEN);
    assert_eq!(owner.status(), StatusCode::OK);
    assert_eq!(profile["results"]["scraps_visible"], false);
    assert!(
        profile["results"]["scraps_count"].is_null(),
        "o contador não pode vazar quando o mural é privado"
    );
}

#[tokio::test]
async fn profile_counts_only_wall_scraps() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    open_wall(&app, &alice).await;

    let id = scrap_id(&app, &bob, &alice.slug, "primeiro").await;
    scrap_id(&app, &bob, &alice.slug, "segundo").await;

    // A resposta não entra na contagem do mural
    let response = post_reply(&app, &alice, &id, "obrigada!").await;
    assert_eq!(response.status(), StatusCode::CREATED);

    // Act
    let profile: serde_json::Value = app
        .api_client
        .get(format!("{}/api/v1/profile/{}", &app.address, alice.slug))
        .send()
        .await
        .expect("Failed to get profile")
        .json()
        .await
        .expect("profile not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(profile["results"]["scraps_count"], 2);
}

#[tokio::test]
async fn scrap_read_friends_allows_only_friends_to_see_the_wall() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let carol = register_and_login(&app, "Carol").await;

    befriend(&app, &bob, &alice).await;
    set_privacy(&app, &alice, "scrap_read", "friends").await;

    // Act
    let friend = list_wall(&app, &alice.slug, Some(&bob), "").await;
    let stranger = list_wall(&app, &alice.slug, Some(&carol), "").await;
    let anonymous = list_wall(&app, &alice.slug, None, "").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(friend.status(), StatusCode::OK);
    assert_eq!(stranger.status(), StatusCode::FORBIDDEN);
    assert_eq!(anonymous.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn scrap_write_friends_blocks_strangers_from_posting() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let carol = register_and_login(&app, "Carol").await;

    befriend(&app, &bob, &alice).await;
    set_privacy(&app, &alice, "scrap_write", "friends").await;

    // Act
    let friend = post_scrap(&app, Some(&bob), &alice.slug, "oi amiga").await;
    let stranger = post_scrap(&app, Some(&carol), &alice.slug, "oi estranha").await;

    let seen_by_stranger: serde_json::Value = app
        .api_client
        .get(format!("{}/api/v1/profile/{}", &app.address, alice.slug))
        .header(reqwest::header::AUTHORIZATION, carol.auth_header())
        .send()
        .await
        .expect("Failed to get profile")
        .json()
        .await
        .expect("profile not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(friend.status(), StatusCode::CREATED);
    assert_eq!(stranger.status(), StatusCode::FORBIDDEN);
    assert_eq!(seen_by_stranger["results"]["can_write_scrap"], false);
}

#[tokio::test]
async fn scrap_write_nobody_still_lets_the_owner_post_on_its_own_wall() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;

    set_privacy(&app, &alice, "scrap_write", "nobody").await;

    // Act
    let response = post_scrap(&app, Some(&alice), &alice.slug, "anotação minha").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn blocked_user_cannot_see_or_post_on_the_wall() {
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
    let read = list_wall(&app, &alice.slug, Some(&bob), "").await;
    let write = post_scrap(&app, Some(&bob), &alice.slug, "oi").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(read.status(), StatusCode::NOT_FOUND);
    assert_eq!(write.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn replying_a_scrap_counts_on_the_wall_and_lists_in_the_thread() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    open_wall(&app, &alice).await;

    let id = scrap_id(&app, &bob, &alice.slug, "oi!").await;

    // Act
    let reply = post_reply(&app, &alice, &id, "oi, tudo bem?").await;
    let reply_status = reply.status();
    let reply: serde_json::Value = reply.json().await.expect("reply not JSON");

    let thread: serde_json::Value = list_replies(&app, &id, None)
        .await
        .json()
        .await
        .expect("thread not JSON");

    let wall: serde_json::Value = list_wall(&app, &alice.slug, None, "")
        .await
        .json()
        .await
        .expect("wall not JSON");

    // A resposta não aparece no mural, só dentro da thread
    let nested = post_reply(&app, &bob, reply["results"]["id"].as_str().unwrap(), "eba").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(reply_status, StatusCode::CREATED);
    assert_eq!(thread["results"].as_array().unwrap().len(), 1);
    assert_eq!(thread["results"][0]["author"]["slug"], alice.slug);

    assert_eq!(wall["results"].as_array().unwrap().len(), 1);
    assert_eq!(wall["results"][0]["reply_count"], 1);

    assert_eq!(
        nested.status(),
        StatusCode::NOT_FOUND,
        "a thread deve ter um nível só"
    );
}

#[tokio::test]
async fn thread_respects_the_wall_privacy() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    let id = scrap_id(&app, &alice, &alice.slug, "oi!").await;
    set_privacy(&app, &alice, "scrap_read", "nobody").await;

    // Act
    let stranger = list_replies(&app, &id, Some(&bob)).await;
    let owner = list_replies(&app, &id, Some(&alice)).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(stranger.status(), StatusCode::FORBIDDEN);
    assert_eq!(owner.status(), StatusCode::OK);
}

#[tokio::test]
async fn only_the_author_can_edit_a_scrap() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    open_wall(&app, &alice).await;

    let id = scrap_id(&app, &bob, &alice.slug, "texto original").await;

    // Act
    let by_author = app
        .api_client
        .put(format!("{}/api/v1/scrap/{}", &app.address, id))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .json(&json!({ "body": "texto editado" }))
        .send()
        .await
        .expect("Failed to edit scrap");
    let by_author_status = by_author.status();
    let by_author: serde_json::Value = by_author.json().await.expect("scrap not JSON");

    // O dono do mural não é o autor, então não pode editar
    let by_wall_owner = app
        .api_client
        .put(format!("{}/api/v1/scrap/{}", &app.address, id))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .json(&json!({ "body": "texto de outro" }))
        .send()
        .await
        .expect("Failed to edit scrap");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(by_author_status, StatusCode::OK);
    assert_eq!(by_author["results"]["body"], "texto editado");
    assert_eq!(by_wall_owner.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn author_and_wall_owner_can_delete_a_scrap() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let carol = register_and_login(&app, "Carol").await;

    open_wall(&app, &alice).await;

    let by_bob = scrap_id(&app, &bob, &alice.slug, "recado do bob").await;
    let by_carol = scrap_id(&app, &carol, &alice.slug, "recado da carol").await;

    // Act
    let by_stranger = delete_scrap(&app, &carol, &by_bob).await;
    let by_author = delete_scrap(&app, &bob, &by_bob).await;
    let by_wall_owner = delete_scrap(&app, &alice, &by_carol).await;

    let wall: serde_json::Value = list_wall(&app, &alice.slug, None, "")
        .await
        .json()
        .await
        .expect("wall not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(by_stranger.status(), StatusCode::NOT_FOUND);
    assert_eq!(by_author.status(), StatusCode::NO_CONTENT);
    assert_eq!(by_wall_owner.status(), StatusCode::NO_CONTENT);
    assert!(wall["results"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn deleting_a_scrap_removes_its_replies() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    open_wall(&app, &alice).await;

    let id = scrap_id(&app, &bob, &alice.slug, "oi!").await;
    let response = post_reply(&app, &alice, &id, "oi!").await;
    assert_eq!(response.status(), StatusCode::CREATED);

    // Act
    let deleted = delete_scrap(&app, &bob, &id).await;
    let thread = list_replies(&app, &id, None).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert_eq!(thread.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn wall_walks_every_page_with_the_cursor() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    open_wall(&app, &alice).await;

    let mut expected = Vec::new();
    for i in 0..3 {
        expected.push(scrap_id(&app, &bob, &alice.slug, &format!("recado {i}")).await);
    }

    // Act
    let mut seen = Vec::new();
    let mut query = "limit=2".to_string();
    loop {
        let body: serde_json::Value = list_wall(&app, &alice.slug, None, &query)
            .await
            .json()
            .await
            .expect("wall not JSON");

        for scrap in body["results"].as_array().unwrap() {
            seen.push(scrap["id"].as_str().unwrap().to_string());
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
async fn unknown_scrap_returns_404() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let unknown = Uuid::now_v7().to_string();

    // Act
    let thread = list_replies(&app, &unknown, None).await;
    let reply = post_reply(&app, &alice, &unknown, "oi").await;
    let deleted = delete_scrap(&app, &alice, &unknown).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(thread.status(), StatusCode::NOT_FOUND);
    assert_eq!(reply.status(), StatusCode::NOT_FOUND);
    assert_eq!(deleted.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deleting_a_scrap_keeps_the_row_for_moderation() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    open_wall(&app, &alice).await;

    let id = scrap_id(&app, &bob, &alice.slug, "conteúdo problemático").await;
    let reply = post_reply(&app, &alice, &id, "resposta").await;
    assert_eq!(reply.status(), StatusCode::CREATED);

    // Act
    let deleted = delete_scrap(&app, &bob, &id).await;

    let wall: serde_json::Value = list_wall(&app, &alice.slug, None, "")
        .await
        .json()
        .await
        .expect("wall not JSON");

    // O conteúdo continua no banco, marcado, junto das respostas
    let rows = sqlx::query!(
        r#"
        SELECT body, deleted_at, parent_id
        FROM scrap
        WHERE id = $1 OR parent_id = $1
        ORDER BY id
        "#,
        Uuid::parse_str(&id).unwrap(),
    )
    .fetch_all(&app.db_pool)
    .await
    .expect("Failed to read scraps");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert!(wall["results"].as_array().unwrap().is_empty());

    assert_eq!(rows.len(), 2, "nem o recado nem a resposta saem do banco");
    assert_eq!(rows[0].body, "conteúdo problemático");
    assert!(rows.iter().all(|row| row.deleted_at.is_some()));
}

#[tokio::test]
async fn a_deleted_scrap_leaves_the_counters_and_the_thread() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    open_wall(&app, &alice).await;

    let kept = scrap_id(&app, &bob, &alice.slug, "fica").await;
    let removed = scrap_id(&app, &bob, &alice.slug, "sai").await;

    // Uma resposta apagada não pode continuar contando na thread
    let reply: serde_json::Value = post_reply(&app, &alice, &kept, "resposta")
        .await
        .json()
        .await
        .expect("reply not JSON");
    let reply_id = reply["results"]["id"].as_str().unwrap().to_string();

    // Act
    delete_scrap(&app, &bob, &removed).await;
    delete_scrap(&app, &alice, &reply_id).await;

    let profile: serde_json::Value = app
        .api_client
        .get(format!("{}/api/v1/profile/{}", &app.address, alice.slug))
        .send()
        .await
        .expect("Failed to get profile")
        .json()
        .await
        .expect("profile not JSON");

    let thread: serde_json::Value = list_replies(&app, &kept, None)
        .await
        .json()
        .await
        .expect("thread not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(profile["results"]["scraps_count"], 1);
    assert!(thread["results"].as_array().unwrap().is_empty());
}
