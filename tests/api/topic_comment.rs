use reqwest::StatusCode;
use serde_json::json;

use crate::{
    helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app},
    topic::{create_community, join, moderate, open_topic},
};

async fn json_of(response: reqwest::Response) -> serde_json::Value {
    response.json().await.expect("response not JSON")
}

async fn comment(
    app: &TestApp,
    user: Option<&RegisteredUser>,
    topic_id: &str,
    body: &str,
    parent_id: Option<&str>,
) -> reqwest::Response {
    let payload = match parent_id {
        Some(parent_id) => json!({ "body": body, "parent_id": parent_id }),
        None => json!({ "body": body }),
    };

    let request = app
        .api_client
        .post(format!("{}/api/v1/topic/{}/comment", app.address, topic_id))
        .json(&payload);

    let request = match user {
        Some(user) => request.header(reqwest::header::AUTHORIZATION, user.auth_header()),
        None => request,
    };

    request.send().await.expect("Failed to comment")
}

/// Comenta e devolve o id, para os testes que só precisam montar a árvore.
async fn post_comment(
    app: &TestApp,
    user: &RegisteredUser,
    topic_id: &str,
    body: &str,
    parent_id: Option<&str>,
) -> String {
    let response = comment(app, Some(user), topic_id, body, parent_id).await;

    json_of(response).await["results"]["id"]
        .as_str()
        .expect("comment id not found")
        .to_string()
}

async fn list_comments(
    app: &TestApp,
    topic_id: &str,
    viewer: Option<&RegisteredUser>,
) -> serde_json::Value {
    let request = app
        .api_client
        .get(format!("{}/api/v1/topic/{}/comment", app.address, topic_id));

    let request = match viewer {
        Some(viewer) => request.header(reqwest::header::AUTHORIZATION, viewer.auth_header()),
        None => request,
    };

    json_of(request.send().await.expect("Failed to list comments")).await
}

async fn edit_comment(
    app: &TestApp,
    user: &RegisteredUser,
    topic_id: &str,
    comment_id: &str,
    body: &str,
) -> reqwest::Response {
    app.api_client
        .put(format!(
            "{}/api/v1/topic/{}/comment/{}",
            app.address, topic_id, comment_id
        ))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .json(&json!({ "body": body }))
        .send()
        .await
        .expect("Failed to edit comment")
}

async fn delete_comment(
    app: &TestApp,
    user: &RegisteredUser,
    topic_id: &str,
    comment_id: &str,
) -> reqwest::Response {
    app.api_client
        .delete(format!(
            "{}/api/v1/topic/{}/comment/{}",
            app.address, topic_id, comment_id
        ))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to delete comment")
}

async fn context(app: &TestApp, topic_id: &str, comment_id: &str) -> reqwest::Response {
    app.api_client
        .get(format!(
            "{}/api/v1/topic/{}/comment/{}/context",
            app.address, topic_id, comment_id
        ))
        .send()
        .await
        .expect("Failed to load comment context")
}

async fn notifications(app: &TestApp, user: &RegisteredUser) -> serde_json::Value {
    json_of(
        app.api_client
            .get(format!("{}/api/v1/notification", app.address))
            .header(reqwest::header::AUTHORIZATION, user.auth_header())
            .send()
            .await
            .expect("Failed to list notifications"),
    )
    .await
}

/// Comunidade pública com Alice de dona e Bob de membro, que é o arranjo da
/// maioria dos testes daqui.
async fn forum(app: &TestApp) -> (RegisteredUser, RegisteredUser, String, String) {
    let alice = register_and_login(app, "Alice").await;
    let bob = register_and_login(app, "Bob").await;
    let slug = create_community(app, &alice, "Nostalgia dos Anos 2000", false).await;
    join(app, &bob, &slug).await;
    let topic_id = open_topic(app, &alice, &slug, "Lembram do Orkut?").await;

    (alice, bob, slug, topic_id)
}

#[tokio::test]
async fn a_reply_comes_back_nested_under_its_parent() {
    // Arrange
    let app = spawn_app().await;
    let (alice, bob, _slug, topic_id) = forum(&app).await;

    // Act
    let root = post_comment(&app, &bob, &topic_id, "lembro sim", None).await;
    let reply = post_comment(&app, &alice, &topic_id, "eu também", Some(&root)).await;
    let deep = post_comment(&app, &bob, &topic_id, "e eu", Some(&reply)).await;

    let listing = list_comments(&app, &topic_id, None).await;
    let results = listing["results"].as_array().expect("results");

    // Assert
    assert_eq!(3, results.len());
    assert_eq!(root, results[0]["id"]);
    assert_eq!(0, results[0]["depth"]);
    assert!(results[0]["parent_id"].is_null());

    assert_eq!(reply, results[1]["id"]);
    assert_eq!(1, results[1]["depth"]);
    assert_eq!(root, results[1]["parent_id"]);

    assert_eq!(deep, results[2]["id"]);
    assert_eq!(2, results[2]["depth"]);
    assert_eq!(reply, results[2]["parent_id"]);

    drop_app(app).await;
}

#[tokio::test]
async fn commenting_requires_membership() {
    // Arrange
    let app = spawn_app().await;
    let (_alice, _bob, _slug, topic_id) = forum(&app).await;
    let carol = register_and_login(&app, "Carol").await;

    // Act
    let stranger = comment(&app, Some(&carol), &topic_id, "oi", None).await;
    let anonymous = comment(&app, None, &topic_id, "oi", None).await;

    // Assert
    assert_eq!(StatusCode::FORBIDDEN, stranger.status());
    assert_eq!(StatusCode::UNAUTHORIZED, anonymous.status());

    drop_app(app).await;
}

#[tokio::test]
async fn a_deleted_comment_becomes_a_marker_and_keeps_its_replies() {
    // Arrange
    let app = spawn_app().await;
    let (alice, bob, _slug, topic_id) = forum(&app).await;

    let root = post_comment(&app, &bob, &topic_id, "texto original do Bob", None).await;
    let reply = post_comment(&app, &alice, &topic_id, "respondendo o Bob", Some(&root)).await;

    // Act
    let deleted = delete_comment(&app, &bob, &topic_id, &root).await;
    let listing = list_comments(&app, &topic_id, None).await;
    let results = listing["results"].as_array().expect("results");

    // Assert
    assert_eq!(StatusCode::NO_CONTENT, deleted.status());
    assert_eq!(2, results.len());

    assert_eq!(root, results[0]["id"]);
    assert_eq!("[EXCLUÍDO]", results[0]["body"]);
    assert!(results[0]["author"].is_null());
    assert!(!results[0]["deleted_at"].is_null());

    // A resposta continua legível e presa no marcador.
    assert_eq!(reply, results[1]["id"]);
    assert_eq!("respondendo o Bob", results[1]["body"]);
    assert_eq!(1, results[1]["depth"]);

    // O texto original continua no banco, que é o ponto do soft delete.
    let stored = sqlx::query_scalar!(
        r#"SELECT body FROM community_topic_comment WHERE id = $1"#,
        uuid::Uuid::parse_str(&root).expect("uuid"),
    )
    .fetch_one(&app.db_pool)
    .await
    .expect("Failed to read comment");

    assert_eq!("texto original do Bob", stored);

    drop_app(app).await;
}

#[tokio::test]
async fn moderators_delete_someone_elses_comment_but_never_rewrite_it() {
    // Arrange
    let app = spawn_app().await;
    let (alice, bob, _slug, topic_id) = forum(&app).await;
    let first = post_comment(&app, &bob, &topic_id, "primeiro", None).await;
    let second = post_comment(&app, &bob, &topic_id, "segundo", None).await;

    // Act
    let edited_by_owner = edit_comment(&app, &alice, &topic_id, &first, "reescrito").await;
    let deleted_by_owner = delete_comment(&app, &alice, &topic_id, &second).await;

    // Assert
    assert_eq!(StatusCode::FORBIDDEN, edited_by_owner.status());
    assert_eq!(StatusCode::NO_CONTENT, deleted_by_owner.status());

    drop_app(app).await;
}

#[tokio::test]
async fn editing_a_comment_marks_it_as_edited() {
    // Arrange
    let app = spawn_app().await;
    let (_alice, bob, _slug, topic_id) = forum(&app).await;
    let comment_id = post_comment(&app, &bob, &topic_id, "erro de digitaçao", None).await;

    // Act
    let response = edit_comment(&app, &bob, &topic_id, &comment_id, "erro de digitação").await;
    let status = response.status();
    let body = json_of(response).await;

    // Assert
    assert_eq!(StatusCode::OK, status);
    assert_eq!("erro de digitação", body["results"]["body"]);
    assert!(!body["results"]["edited_at"].is_null());

    drop_app(app).await;
}

#[tokio::test]
async fn a_locked_topic_takes_no_new_comments_and_no_edits() {
    // Arrange
    let app = spawn_app().await;
    let (alice, bob, _slug, topic_id) = forum(&app).await;
    let comment_id = post_comment(&app, &bob, &topic_id, "antes da tranca", None).await;

    // Act
    moderate(&app, &alice, &topic_id, "lock", true).await;
    let new_comment = comment(&app, Some(&bob), &topic_id, "depois da tranca", None).await;
    let edit = edit_comment(&app, &bob, &topic_id, &comment_id, "tentando editar").await;
    let deleted = delete_comment(&app, &bob, &topic_id, &comment_id).await;

    // Assert
    assert_eq!(StatusCode::FORBIDDEN, new_comment.status());
    assert_eq!(StatusCode::FORBIDDEN, edit.status());
    // Apagar o próprio conteúdo continua valendo em tópico trancado.
    assert_eq!(StatusCode::NO_CONTENT, deleted.status());

    drop_app(app).await;
}

#[tokio::test]
async fn replying_a_deleted_comment_is_refused() {
    // Arrange
    let app = spawn_app().await;
    let (_alice, bob, _slug, topic_id) = forum(&app).await;
    let root = post_comment(&app, &bob, &topic_id, "some daqui a pouco", None).await;
    delete_comment(&app, &bob, &topic_id, &root).await;

    // Act
    let response = comment(&app, Some(&bob), &topic_id, "tem alguém aí?", Some(&root)).await;

    // Assert
    assert_eq!(StatusCode::FORBIDDEN, response.status());

    drop_app(app).await;
}

#[tokio::test]
async fn a_comment_past_the_limit_answers_a_message_without_echoing_the_body() {
    // Arrange
    let app = spawn_app().await;
    let (_alice, bob, _slug, topic_id) = forum(&app).await;
    let long_body = "a".repeat(1001);

    // Act
    let response = comment(&app, Some(&bob), &topic_id, &long_body, None).await;

    // Assert
    assert_eq!(StatusCode::BAD_REQUEST, response.status());

    let body: serde_json::Value = response.json().await.expect("response not JSON");
    let error = &body["error"]["body"][0];

    assert_eq!("length", error["code"]);
    // Sem mensagem a tela cai no erro genérico e ninguém descobre o que corrigir.
    assert!(error["message"].is_string());
    // O corpo enviado não volta nos params: quem mandou já tem ele.
    assert!(error["params"]["value"].is_null());
    assert_eq!(1000, error["params"]["max"]);

    drop_app(app).await;
}

#[tokio::test]
async fn a_comment_with_only_markup_is_refused() {
    // Arrange
    let app = spawn_app().await;
    let (_alice, bob, _slug, topic_id) = forum(&app).await;

    // Act
    // Passa no `trim`, mas chega vazio na tela de quem lê.
    let response = comment(&app, Some(&bob), &topic_id, "[i][/i]", None).await;

    // Assert
    assert_eq!(StatusCode::BAD_REQUEST, response.status());

    let body: serde_json::Value = response.json().await.expect("response not JSON");
    assert_eq!("empty_or_whitespace", body["error"]["body"][0]["code"]);

    drop_app(app).await;
}

#[tokio::test]
async fn replying_a_comment_from_another_topic_is_refused() {
    // Arrange
    let app = spawn_app().await;
    let (alice, bob, slug, topic_id) = forum(&app).await;
    let other_topic = open_topic(&app, &alice, &slug, "Outro assunto").await;
    let elsewhere =
        post_comment(&app, &bob, &other_topic, "comentário de outro tópico", None).await;

    // Act
    let response = comment(
        &app,
        Some(&bob),
        &topic_id,
        "resposta cruzada",
        Some(&elsewhere),
    )
    .await;

    // Assert
    assert_eq!(StatusCode::BAD_REQUEST, response.status());

    drop_app(app).await;
}

#[tokio::test]
async fn the_context_route_returns_the_whole_ancestor_chain() {
    // Arrange
    let app = spawn_app().await;
    let (alice, bob, _slug, topic_id) = forum(&app).await;

    let root = post_comment(&app, &bob, &topic_id, "nível 1", None).await;
    let middle = post_comment(&app, &alice, &topic_id, "nível 2", Some(&root)).await;
    let focused = post_comment(&app, &bob, &topic_id, "nível 3", Some(&middle)).await;
    let child = post_comment(&app, &alice, &topic_id, "nível 4", Some(&focused)).await;

    // Act
    let response = context(&app, &topic_id, &focused).await;
    let status = response.status();
    let body = json_of(response).await["results"].clone();
    let ancestors = body["ancestors"].as_array().expect("ancestors");
    let replies = body["replies"].as_array().expect("replies");

    // Assert
    assert_eq!(StatusCode::OK, status);
    assert_eq!(2, ancestors.len());
    assert_eq!(root, ancestors[0]["id"]);
    assert_eq!(middle, ancestors[1]["id"]);

    // A profundidade do contexto é relativa ao comentário focado.
    assert_eq!(focused, body["comment"]["id"]);
    assert_eq!(0, body["comment"]["depth"]);
    assert_eq!(1, replies.len());
    assert_eq!(child, replies[0]["id"]);
    assert_eq!(0, replies[0]["depth"]);

    drop_app(app).await;
}

#[tokio::test]
async fn the_context_of_a_root_comment_has_no_ancestors() {
    // Arrange
    let app = spawn_app().await;
    let (_alice, bob, _slug, topic_id) = forum(&app).await;
    let root = post_comment(&app, &bob, &topic_id, "sozinho no topo", None).await;

    // Act
    let body = json_of(context(&app, &topic_id, &root).await).await["results"].clone();

    // Assert
    assert!(body["ancestors"].as_array().expect("ancestors").is_empty());
    assert_eq!(root, body["comment"]["id"]);

    drop_app(app).await;
}

#[tokio::test]
async fn a_page_that_ends_exactly_on_the_limit_comes_without_cursor() {
    // Arrange
    let app = spawn_app().await;
    let (alice, bob, _slug, topic_id) = forum(&app).await;

    let first = post_comment(&app, &bob, &topic_id, "raiz 1", None).await;
    // A resposta enche a página de itens sem encher a de raízes, que é o que conta.
    post_comment(&app, &alice, &topic_id, "resposta da raiz 1", Some(&first)).await;
    post_comment(&app, &bob, &topic_id, "raiz 2", None).await;

    // Act
    let page = json_of(
        app.api_client
            .get(format!(
                "{}/api/v1/topic/{}/comment?limit=2",
                app.address, topic_id
            ))
            .send()
            .await
            .expect("Failed to list comments"),
    )
    .await;

    // Assert
    // As duas raízes fecham a página, e não existe uma terceira: mandar cursor aqui
    // daria ao frontend um "carregar mais" que não carrega nada.
    assert_eq!(3, page["results"].as_array().expect("results").len());
    assert!(page["next_cursor"].is_null());

    drop_app(app).await;
}

#[tokio::test]
async fn the_comment_cursor_pages_over_roots_carrying_their_replies() {
    // Arrange
    let app = spawn_app().await;
    let (alice, bob, _slug, topic_id) = forum(&app).await;

    let first = post_comment(&app, &bob, &topic_id, "raiz 1", None).await;
    post_comment(&app, &alice, &topic_id, "resposta da raiz 1", Some(&first)).await;
    let second = post_comment(&app, &bob, &topic_id, "raiz 2", None).await;
    let third = post_comment(&app, &bob, &topic_id, "raiz 3", None).await;

    let url = format!("{}/api/v1/topic/{}/comment", app.address, topic_id);

    // Act
    let page = json_of(
        app.api_client
            .get(format!("{url}?limit=2"))
            .send()
            .await
            .expect("Failed to list comments"),
    )
    .await;

    // O cursor dos comentários é o id da última raiz, então vai cru na URL.
    let cursor = page["next_cursor"].as_str().expect("cursor").to_string();

    let next = json_of(
        app.api_client
            .get(format!("{url}?limit=2&cursor={cursor}"))
            .send()
            .await
            .expect("Failed to list comments"),
    )
    .await;

    // Assert
    // A primeira página traz duas raízes mais a resposta pendurada na primeira,
    // e o cursor anda só sobre as raízes.
    let results = page["results"].as_array().expect("results");
    assert_eq!(3, results.len());
    assert_eq!(second, cursor);

    let next_results = next["results"].as_array().expect("results");
    assert_eq!(1, next_results.len());
    assert_eq!(third, next_results[0]["id"]);
    assert!(next["next_cursor"].is_null());

    drop_app(app).await;
}

#[tokio::test]
async fn a_comment_notifies_the_topic_author_and_a_reply_notifies_the_parent_author() {
    // Arrange
    let app = spawn_app().await;
    let (alice, bob, _slug, topic_id) = forum(&app).await;

    // Act
    let root = post_comment(&app, &bob, &topic_id, "comentando o tópico da Alice", None).await;
    post_comment(&app, &alice, &topic_id, "respondendo o Bob", Some(&root)).await;

    let alice_inbox = notifications(&app, &alice).await;
    let bob_inbox = notifications(&app, &bob).await;

    // Assert
    let to_alice = alice_inbox["results"].as_array().expect("results");
    assert_eq!(1, to_alice.len());
    assert_eq!("topic_comment", to_alice[0]["kind"]);
    assert_eq!(topic_id, to_alice[0]["topic_id"]);
    assert_eq!(bob.slug, to_alice[0]["actor"]["slug"]);

    let to_bob = bob_inbox["results"].as_array().expect("results");
    assert_eq!(1, to_bob.len());
    assert_eq!("topic_reply", to_bob[0]["kind"]);
    assert_eq!(topic_id, to_bob[0]["topic_id"]);
    assert!(!to_bob[0]["comment_id"].is_null());
    assert_eq!(alice.slug, to_bob[0]["actor"]["slug"]);

    drop_app(app).await;
}
