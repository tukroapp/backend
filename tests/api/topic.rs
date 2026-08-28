use reqwest::StatusCode;
use serde_json::json;

use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};

/// O cursor da listagem carrega data ISO, então o `+` e o `:` precisam ir
/// escapados na query string.
fn escape_cursor(cursor: &str) -> String {
    cursor.replace('+', "%2B").replace(':', "%3A")
}

async fn json_of(response: reqwest::Response) -> serde_json::Value {
    response.json().await.expect("response not JSON")
}

async fn promote(app: &TestApp, user: &RegisteredUser) {
    sqlx::query!(
        r#"
        UPDATE auth_user SET is_superuser = TRUE
        WHERE id = (SELECT user_id FROM user_profile WHERE slug = $1)
        "#,
        user.slug,
    )
    .execute(&app.db_pool)
    .await
    .expect("Failed to promote user");
}

pub async fn create_community(
    app: &TestApp,
    user: &RegisteredUser,
    name: &str,
    is_private: bool,
) -> String {
    let response = app
        .api_client
        .post(format!("{}/api/v1/community", app.address))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .json(&json!({ "name": name, "category": "outros", "is_private": is_private }))
        .send()
        .await
        .expect("Failed to create community");

    json_of(response).await["results"]["slug"]
        .as_str()
        .expect("slug not found")
        .to_string()
}

pub async fn join(app: &TestApp, user: &RegisteredUser, slug: &str) {
    app.api_client
        .post(format!("{}/api/v1/community/{}/member", app.address, slug))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to join community");
}

pub async fn create_topic(
    app: &TestApp,
    user: Option<&RegisteredUser>,
    slug: &str,
    title: &str,
    body: &str,
) -> reqwest::Response {
    let request = app
        .api_client
        .post(format!("{}/api/v1/community/{}/topic", app.address, slug))
        .json(&json!({ "title": title, "body": body }));

    let request = match user {
        Some(user) => request.header(reqwest::header::AUTHORIZATION, user.auth_header()),
        None => request,
    };

    request.send().await.expect("Failed to create topic")
}

/// Abre o tópico e devolve o id, para os testes que só precisam disso.
pub async fn open_topic(app: &TestApp, user: &RegisteredUser, slug: &str, title: &str) -> String {
    let response = create_topic(app, Some(user), slug, title, "corpo do tópico").await;

    json_of(response).await["results"]["id"]
        .as_str()
        .expect("topic id not found")
        .to_string()
}

pub async fn list_topics(
    app: &TestApp,
    slug: &str,
    viewer: Option<&RegisteredUser>,
) -> reqwest::Response {
    let request = app
        .api_client
        .get(format!("{}/api/v1/community/{}/topic", app.address, slug));

    let request = match viewer {
        Some(viewer) => request.header(reqwest::header::AUTHORIZATION, viewer.auth_header()),
        None => request,
    };

    request.send().await.expect("Failed to list topics")
}

pub async fn get_topic(
    app: &TestApp,
    id: &str,
    viewer: Option<&RegisteredUser>,
) -> reqwest::Response {
    let request = app
        .api_client
        .get(format!("{}/api/v1/topic/{}", app.address, id));

    let request = match viewer {
        Some(viewer) => request.header(reqwest::header::AUTHORIZATION, viewer.auth_header()),
        None => request,
    };

    request.send().await.expect("Failed to get topic")
}

async fn edit_topic(
    app: &TestApp,
    user: &RegisteredUser,
    id: &str,
    title: &str,
    body: &str,
) -> reqwest::Response {
    app.api_client
        .put(format!("{}/api/v1/topic/{}", app.address, id))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .json(&json!({ "title": title, "body": body }))
        .send()
        .await
        .expect("Failed to edit topic")
}

async fn delete_topic(app: &TestApp, user: &RegisteredUser, id: &str) -> reqwest::Response {
    app.api_client
        .delete(format!("{}/api/v1/topic/{}", app.address, id))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to delete topic")
}

pub async fn moderate(
    app: &TestApp,
    user: &RegisteredUser,
    id: &str,
    flag: &str,
    on: bool,
) -> reqwest::Response {
    let url = format!("{}/api/v1/topic/{}/{}", app.address, id, flag);
    let request = if on {
        app.api_client.post(url)
    } else {
        app.api_client.delete(url)
    };

    request
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to moderate topic")
}

#[tokio::test]
async fn member_opens_a_topic_and_it_shows_up_in_the_listing() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let slug = create_community(&app, &alice, "Eu Odeio Acordar Cedo", false).await;

    // Act
    let response = create_topic(&app, Some(&alice), &slug, "Bom dia", "alguém acordou?").await;
    let status = response.status();
    let created = json_of(response).await;

    let listing = json_of(list_topics(&app, &slug, None).await).await;

    // Assert
    assert_eq!(StatusCode::CREATED, status);
    assert_eq!("Bom dia", created["results"]["title"]);
    assert_eq!(0, created["results"]["comment_count"]);
    assert!(created["results"]["edited_at"].is_null());
    assert_eq!(alice.slug, created["results"]["author"]["slug"]);
    assert_eq!(1, listing["results"].as_array().expect("results").len());
    assert!(listing["next_cursor"].is_null());

    drop_app(app).await;
}

#[tokio::test]
async fn opening_a_topic_requires_membership() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let slug = create_community(&app, &alice, "Jogos Antigos", false).await;

    // Act
    let stranger = create_topic(&app, Some(&bob), &slug, "Oi", "posso postar?").await;
    let anonymous = create_topic(&app, None, &slug, "Oi", "posso postar?").await;

    // Assert
    assert_eq!(StatusCode::FORBIDDEN, stranger.status());
    assert_eq!(StatusCode::UNAUTHORIZED, anonymous.status());

    drop_app(app).await;
}

#[tokio::test]
async fn the_forum_of_a_private_community_is_invisible_from_the_outside() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let slug = create_community(&app, &alice, "Clube Fechado", true).await;
    let topic_id = open_topic(&app, &alice, &slug, "Só para membros").await;

    // Act
    let listing = list_topics(&app, &slug, Some(&bob)).await;
    let detail = get_topic(&app, &topic_id, Some(&bob)).await;
    let anonymous = get_topic(&app, &topic_id, None).await;
    let member = get_topic(&app, &topic_id, Some(&alice)).await;

    // Assert
    assert_eq!(StatusCode::NOT_FOUND, listing.status());
    assert_eq!(StatusCode::NOT_FOUND, detail.status());
    assert_eq!(StatusCode::NOT_FOUND, anonymous.status());
    assert_eq!(StatusCode::OK, member.status());

    drop_app(app).await;
}

#[tokio::test]
async fn editing_a_topic_marks_it_as_edited_and_only_the_author_can_do_it() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let slug = create_community(&app, &alice, "Culinária", false).await;
    join(&app, &bob, &slug).await;
    let topic_id = open_topic(&app, &bob, &slug, "Receita de bolo").await;

    // Act
    let author = edit_topic(&app, &bob, &topic_id, "Receita de bolo", "agora com fubá").await;
    let author_status = author.status();
    let edited = json_of(author).await;

    // A dona da comunidade modera, mas texto alheio ela não reescreve.
    let owner = edit_topic(&app, &alice, &topic_id, "Outro título", "outro corpo").await;

    // Assert
    assert_eq!(StatusCode::OK, author_status);
    assert_eq!("agora com fubá", edited["results"]["body"]);
    assert!(!edited["results"]["edited_at"].is_null());
    assert_eq!(StatusCode::FORBIDDEN, owner.status());

    drop_app(app).await;
}

#[tokio::test]
async fn a_deleted_topic_disappears_from_the_forum() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let slug = create_community(&app, &alice, "Rock Nacional", false).await;
    let topic_id = open_topic(&app, &alice, &slug, "Melhor disco").await;

    // Act
    let deleted = delete_topic(&app, &alice, &topic_id).await;
    let detail = get_topic(&app, &topic_id, Some(&alice)).await;
    let listing = json_of(list_topics(&app, &slug, Some(&alice)).await).await;

    // Assert
    assert_eq!(StatusCode::NO_CONTENT, deleted.status());
    assert_eq!(StatusCode::NOT_FOUND, detail.status());
    assert!(listing["results"].as_array().expect("results").is_empty());

    drop_app(app).await;
}

#[tokio::test]
async fn moderators_and_superusers_delete_someone_elses_topic() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let carol = register_and_login(&app, "Carol").await;
    promote(&app, &carol).await;

    let slug = create_community(&app, &alice, "Fotografia", false).await;
    join(&app, &bob, &slug).await;
    let first = open_topic(&app, &bob, &slug, "Primeira câmera").await;
    let second = open_topic(&app, &bob, &slug, "Segunda câmera").await;

    // Act
    let by_owner = delete_topic(&app, &alice, &first).await;
    // A Carol nem é membro da comunidade: o alcance vem de ser superusuária.
    let by_superuser = delete_topic(&app, &carol, &second).await;

    // Assert
    assert_eq!(StatusCode::NO_CONTENT, by_owner.status());
    assert_eq!(StatusCode::NO_CONTENT, by_superuser.status());

    drop_app(app).await;
}

#[tokio::test]
async fn a_regular_member_cannot_delete_or_moderate_someone_elses_topic() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let slug = create_community(&app, &alice, "Viagens", false).await;
    join(&app, &bob, &slug).await;
    let topic_id = open_topic(&app, &alice, &slug, "Roteiro pelo Sul").await;

    // Act
    let deleted = delete_topic(&app, &bob, &topic_id).await;
    let pinned = moderate(&app, &bob, &topic_id, "pin", true).await;
    let locked = moderate(&app, &bob, &topic_id, "lock", true).await;

    // Assert
    assert_eq!(StatusCode::FORBIDDEN, deleted.status());
    assert_eq!(StatusCode::FORBIDDEN, pinned.status());
    assert_eq!(StatusCode::FORBIDDEN, locked.status());

    drop_app(app).await;
}

#[tokio::test]
async fn a_pinned_topic_comes_first_even_when_it_is_the_oldest() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let slug = create_community(&app, &alice, "Avisos", false).await;
    let rules = open_topic(&app, &alice, &slug, "Regras da comunidade").await;
    open_topic(&app, &alice, &slug, "Conversa do dia").await;

    // Act
    let pinned = moderate(&app, &alice, &rules, "pin", true).await;
    let listing = json_of(list_topics(&app, &slug, None).await).await;
    let results = listing["results"].as_array().expect("results").clone();

    let unpinned = moderate(&app, &alice, &rules, "pin", false).await;
    let after = json_of(list_topics(&app, &slug, None).await).await;

    // Assert
    assert_eq!(StatusCode::OK, pinned.status());
    assert_eq!(2, results.len());
    assert_eq!(rules, results[0]["id"]);
    assert_eq!(true, results[0]["is_pinned"]);

    assert_eq!(StatusCode::OK, unpinned.status());
    assert_eq!("Conversa do dia", after["results"][0]["title"]);

    drop_app(app).await;
}

#[tokio::test]
async fn a_locked_topic_rejects_edits_from_its_own_author() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let slug = create_community(&app, &alice, "Debates", false).await;
    join(&app, &bob, &slug).await;
    let topic_id = open_topic(&app, &bob, &slug, "Assunto polêmico").await;

    // Act
    let locked = moderate(&app, &alice, &topic_id, "lock", true).await;
    let edit = edit_topic(&app, &bob, &topic_id, "Outro assunto", "mudei de ideia").await;

    // A moderação continua alcançando o tópico trancado.
    let unlocked = moderate(&app, &alice, &topic_id, "lock", false).await;
    let after_unlock = edit_topic(&app, &bob, &topic_id, "Assunto", "agora vai").await;

    // Assert
    assert_eq!(StatusCode::OK, locked.status());
    assert_eq!(StatusCode::FORBIDDEN, edit.status());
    assert_eq!(StatusCode::OK, unlocked.status());
    assert_eq!(StatusCode::OK, after_unlock.status());

    drop_app(app).await;
}

#[tokio::test]
async fn a_topic_with_only_markup_in_the_body_is_refused() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let slug = create_community(&app, &alice, "Livros", false).await;

    // Act
    let response = app
        .api_client
        .post(format!("{}/api/v1/community/{}/topic", app.address, slug))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .json(&json!({ "title": "Título de verdade", "body": "[b][/b]" }))
        .send()
        .await
        .expect("Failed to open topic");

    // Assert
    assert_eq!(StatusCode::BAD_REQUEST, response.status());

    drop_app(app).await;
}

#[tokio::test]
async fn a_topic_page_that_ends_exactly_on_the_limit_comes_without_cursor() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let slug = create_community(&app, &alice, "Livros", false).await;

    for index in 0..2 {
        open_topic(&app, &alice, &slug, &format!("Tópico {index}")).await;
    }

    // Act
    let page = json_of(
        app.api_client
            .get(format!(
                "{}/api/v1/community/{}/topic?limit=2",
                app.address, slug
            ))
            .send()
            .await
            .expect("Failed to list topics"),
    )
    .await;

    // Assert
    // Os dois tópicos fecham a página, e não existe um terceiro: mandar cursor aqui
    // daria ao frontend um "carregar mais" que não carrega nada.
    assert_eq!(2, page["results"].as_array().expect("results").len());
    assert!(page["next_cursor"].is_null());

    drop_app(app).await;
}

#[tokio::test]
async fn topic_listing_walks_the_cursor_without_repeating() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let slug = create_community(&app, &alice, "Livros", false).await;

    for index in 0..3 {
        open_topic(&app, &alice, &slug, &format!("Tópico {index}")).await;
    }

    // Act
    let url = format!("{}/api/v1/community/{}/topic", app.address, slug);

    let first = json_of(
        app.api_client
            .get(format!("{url}?limit=2"))
            .send()
            .await
            .expect("Failed to list topics"),
    )
    .await;

    let cursor = first["next_cursor"].as_str().expect("cursor").to_string();

    let second = json_of(
        app.api_client
            .get(format!("{url}?limit=2&cursor={}", escape_cursor(&cursor)))
            .send()
            .await
            .expect("Failed to list topics"),
    )
    .await;

    // Assert
    let first_ids: Vec<&str> = first["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|topic| topic["id"].as_str().expect("id"))
        .collect();
    let second_ids: Vec<&str> = second["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|topic| topic["id"].as_str().expect("id"))
        .collect();

    assert_eq!(2, first_ids.len());
    assert_eq!(1, second_ids.len());
    assert!(!first_ids.contains(&second_ids[0]));
    assert!(second["next_cursor"].is_null());

    drop_app(app).await;
}
