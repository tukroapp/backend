use reqwest::StatusCode;
use tukro_backend::constants::MAX_FRIENDS;
use uuid::Uuid;

use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};
use crate::profile::get_profile;

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

async fn list_requests(app: &TestApp, user: &RegisteredUser, query: &str) -> serde_json::Value {
    app.api_client
        .get(format!("{}/api/v1/friend/request?{}", &app.address, query))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to list friend requests")
        .json()
        .await
        .expect("friend request list not JSON")
}

async fn cancel_request(app: &TestApp, user: &RegisteredUser, slug: &str) -> reqwest::Response {
    app.api_client
        .delete(format!("{}/api/v1/friend/request/{}", &app.address, slug))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to cancel friend request")
}

async fn respond(
    app: &TestApp,
    user: &RegisteredUser,
    request_id: &str,
    action: &str,
) -> reqwest::Response {
    app.api_client
        .post(format!(
            "{}/api/v1/friend/request/{}/{}",
            &app.address, request_id, action
        ))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to respond to friend request")
}

async fn remove_friend(app: &TestApp, user: &RegisteredUser, slug: &str) -> reqwest::Response {
    app.api_client
        .delete(format!("{}/api/v1/friend/{}", &app.address, slug))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to remove friend")
}

async fn list_friends(
    app: &TestApp,
    slug: &str,
    viewer: Option<&RegisteredUser>,
    query: &str,
) -> reqwest::Response {
    let mut request = app.api_client.get(format!(
        "{}/api/v1/profile/{}/friend?{}",
        &app.address, slug, query
    ));

    if let Some(viewer) = viewer {
        request = request.header(reqwest::header::AUTHORIZATION, viewer.auth_header());
    }

    request.send().await.expect("Failed to list friends")
}

/// Devolve o id do único pedido pendente na caixa de entrada de `user`.
async fn first_pending_id(app: &TestApp, user: &RegisteredUser) -> String {
    let body = list_requests(app, user, "").await;
    body["results"][0]["id"]
        .as_str()
        .expect("no pending friend request")
        .to_string()
}

/// Cria a amizade completa: `a` pede e `b` aceita.
pub async fn befriend(app: &TestApp, a: &RegisteredUser, b: &RegisteredUser) {
    let response = send_request(app, a, &b.slug).await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let request_id = first_pending_id(app, b).await;
    let response = respond(app, b, &request_id, "accept").await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

/// Perfil visto por `viewer` (anônimo quando `None`).
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
        .expect("Failed to get profile")
        .json()
        .await
        .expect("profile response not JSON")
}

async fn relationship(app: &TestApp, slug: &str, viewer: Option<&RegisteredUser>) -> String {
    profile_as(app, slug, viewer).await["results"]["relationship"]
        .as_str()
        .expect("relationship not found")
        .to_string()
}

async fn profile_friend_slugs(app: &TestApp, slug: &str) -> Vec<String> {
    let body: serde_json::Value = get_profile(app, slug)
        .await
        .json()
        .await
        .expect("profile response not JSON");

    body["results"]["friends"]
        .as_array()
        .expect("friends not an array")
        .iter()
        .map(|friend| friend["slug"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn accepting_a_request_creates_friendship_on_both_sides() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    // Act
    befriend(&app, &alice, &bob).await;

    let alice_friends = profile_friend_slugs(&app, &alice.slug).await;
    let bob_friends = profile_friend_slugs(&app, &bob.slug).await;
    let inbox = list_requests(&app, &bob, "").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(alice_friends, vec![bob.slug.clone()]);
    assert_eq!(bob_friends, vec![alice.slug.clone()]);
    assert!(
        inbox["results"].as_array().unwrap().is_empty(),
        "o pedido aceito não deveria continuar pendente"
    );
}

#[tokio::test]
async fn rejecting_a_request_does_not_create_friendship() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    send_request(&app, &alice, &bob.slug).await;
    let request_id = first_pending_id(&app, &bob).await;

    // Act
    let response = respond(&app, &bob, &request_id, "reject").await;
    let status = response.status();
    let bob_friends = profile_friend_slugs(&app, &bob.slug).await;
    let inbox = list_requests(&app, &bob, "").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(bob_friends.is_empty());
    assert!(inbox["results"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn responding_twice_returns_404() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    send_request(&app, &alice, &bob.slug).await;
    let request_id = first_pending_id(&app, &bob).await;
    respond(&app, &bob, &request_id, "accept").await;

    // Act
    let response = respond(&app, &bob, &request_id, "accept").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn only_the_addressee_can_respond_to_a_request() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let carol = register_and_login(&app, "Carol").await;

    send_request(&app, &alice, &bob.slug).await;
    let request_id = first_pending_id(&app, &bob).await;

    // Act
    let by_requester = respond(&app, &alice, &request_id, "accept").await;
    let by_stranger = respond(&app, &carol, &request_id, "accept").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(by_requester.status(), StatusCode::NOT_FOUND);
    assert_eq!(by_stranger.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn duplicated_request_returns_409() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    send_request(&app, &alice, &bob.slug).await;

    // Act
    let response = send_request(&app, &alice, &bob.slug).await;
    let status = response.status();
    let body: serde_json::Value = response.json().await.expect("response not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body["error"]["friend_request"][0]["code"],
        "request_pending"
    );
}

#[tokio::test]
async fn crossed_requests_become_friends_automatically() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    send_request(&app, &alice, &bob.slug).await;

    // Act
    let response = send_request(&app, &bob, &alice.slug).await;
    let status = response.status();
    let body: serde_json::Value = response.json().await.expect("response not JSON");

    let alice_friends = profile_friend_slugs(&app, &alice.slug).await;
    let bob_friends = profile_friend_slugs(&app, &bob.slug).await;
    let inbox = list_requests(&app, &bob, "").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["results"]["status"], "accepted");
    assert_eq!(alice_friends, vec![bob.slug.clone()]);
    assert_eq!(bob_friends, vec![alice.slug.clone()]);
    assert!(
        inbox["results"].as_array().unwrap().is_empty(),
        "o pedido cruzado não deveria continuar pendente"
    );
}

#[tokio::test]
async fn request_to_an_existing_friend_returns_409() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    befriend(&app, &alice, &bob).await;

    // Act
    let response = send_request(&app, &alice, &bob.slug).await;
    let status = response.status();
    let body: serde_json::Value = response.json().await.expect("response not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body["error"]["friend_request"][0]["code"],
        "already_friends"
    );
}

#[tokio::test]
async fn request_to_self_returns_400() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;

    // Act
    let response = send_request(&app, &alice, &alice.slug).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn request_to_unknown_slug_returns_404() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;

    // Act
    let response = send_request(&app, &alice, "nao-existe-esse-slug").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn request_to_a_blocked_user_returns_404() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    sqlx::query!(
        r#"
        INSERT INTO block (blocker_id, blocked_id)
        SELECT
            (SELECT user_id FROM user_profile WHERE slug = $1),
            (SELECT user_id FROM user_profile WHERE slug = $2)
        "#,
        bob.slug,
        alice.slug,
    )
    .execute(&app.db_pool)
    .await
    .expect("Failed to create block");

    // Act
    let response = send_request(&app, &alice, &bob.slug).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn removing_a_friend_clears_both_sides() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    befriend(&app, &alice, &bob).await;

    // Act
    let response = remove_friend(&app, &bob, &alice.slug).await;
    let status = response.status();
    let alice_friends = profile_friend_slugs(&app, &alice.slug).await;
    let bob_friends = profile_friend_slugs(&app, &bob.slug).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(alice_friends.is_empty());
    assert!(bob_friends.is_empty());
}

#[tokio::test]
async fn removing_someone_who_is_not_a_friend_returns_404() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    // Act
    let response = remove_friend(&app, &alice, &bob.slug).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn friend_endpoints_require_authentication() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let request_id = Uuid::now_v7().to_string();

    // Act
    let list = app
        .api_client
        .get(format!("{}/api/v1/friend/request", &app.address))
        .send()
        .await
        .expect("Failed to list friend requests");
    let send = app
        .api_client
        .post(format!(
            "{}/api/v1/friend/request/{}",
            &app.address, alice.slug
        ))
        .send()
        .await
        .expect("Failed to send friend request");
    let accept = app
        .api_client
        .post(format!(
            "{}/api/v1/friend/request/{}/accept",
            &app.address, request_id
        ))
        .send()
        .await
        .expect("Failed to accept friend request");
    let remove = app
        .api_client
        .delete(format!("{}/api/v1/friend/{}", &app.address, alice.slug))
        .send()
        .await
        .expect("Failed to remove friend");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(list.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(send.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(accept.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(remove.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn profile_shows_at_most_nine_friends() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;

    for i in 0..10 {
        let friend = register_and_login(&app, &format!("Amigo {i}")).await;
        befriend(&app, &friend, &alice).await;
    }

    // Act
    let friends = profile_friend_slugs(&app, &alice.slug).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(friends.len(), 9);
}

#[tokio::test]
async fn friend_list_walks_every_page_with_the_cursor() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;

    let mut expected = Vec::new();
    for i in 0..3 {
        let friend = register_and_login(&app, &format!("Amigo {i}")).await;
        expected.push(friend.slug.clone());
        befriend(&app, &friend, &alice).await;
    }

    // Act
    let mut seen = Vec::new();
    let mut query = "limit=2".to_string();
    loop {
        let body: serde_json::Value = list_friends(&app, &alice.slug, None, &query)
            .await
            .json()
            .await
            .expect("friend list not JSON");

        for friend in body["results"].as_array().unwrap() {
            seen.push(friend["slug"].as_str().unwrap().to_string());
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
async fn friend_list_filters_by_the_search_term() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bruno = register_and_login(&app, "Bruno Souza").await;
    let carla = register_and_login(&app, "Carla Lima").await;

    befriend(&app, &bruno, &alice).await;
    befriend(&app, &carla, &alice).await;

    // Act
    let by_name: serde_json::Value = list_friends(&app, &alice.slug, None, "q=souza")
        .await
        .json()
        .await
        .expect("friend list not JSON");
    let by_slug: serde_json::Value =
        list_friends(&app, &alice.slug, None, &format!("q={}", &carla.slug))
            .await
            .json()
            .await
            .expect("friend list not JSON");
    let no_match: serde_json::Value = list_friends(&app, &alice.slug, None, "q=zzz")
        .await
        .json()
        .await
        .expect("friend list not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(by_name["results"].as_array().unwrap().len(), 1);
    assert_eq!(by_name["results"][0]["slug"], bruno.slug);
    assert_eq!(by_slug["results"].as_array().unwrap().len(), 1);
    assert_eq!(by_slug["results"][0]["slug"], carla.slug);
    assert!(no_match["results"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn friend_list_is_hidden_when_visibility_is_nobody() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    befriend(&app, &alice, &bob).await;

    sqlx::query!(
        "UPDATE user_profile SET profile_visibility = 'nobody' WHERE slug = $1",
        alice.slug
    )
    .execute(&app.db_pool)
    .await
    .expect("Failed to update visibility");

    // Act
    let anonymous = list_friends(&app, &alice.slug, None, "").await;
    let friend = list_friends(&app, &alice.slug, Some(&bob), "").await;
    let owner = list_friends(&app, &alice.slug, Some(&alice), "").await;
    let preview = profile_friend_slugs(&app, &alice.slug).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(anonymous.status(), StatusCode::FORBIDDEN);
    assert_eq!(friend.status(), StatusCode::FORBIDDEN);
    assert_eq!(owner.status(), StatusCode::OK);
    assert!(preview.is_empty(), "o preview anônimo deve vir vazio");
}

#[tokio::test]
async fn friend_list_with_friends_visibility_is_restricted_to_friends() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let carol = register_and_login(&app, "Carol").await;
    befriend(&app, &alice, &bob).await;

    sqlx::query!(
        "UPDATE user_profile SET profile_visibility = 'friends' WHERE slug = $1",
        alice.slug
    )
    .execute(&app.db_pool)
    .await
    .expect("Failed to update visibility");

    // Act
    let anonymous = list_friends(&app, &alice.slug, None, "").await;
    let stranger = list_friends(&app, &alice.slug, Some(&carol), "").await;
    let friend = list_friends(&app, &alice.slug, Some(&bob), "").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(anonymous.status(), StatusCode::FORBIDDEN);
    assert_eq!(stranger.status(), StatusCode::FORBIDDEN);
    assert_eq!(friend.status(), StatusCode::OK);
}

#[tokio::test]
async fn cancelling_a_sent_request_frees_both_sides() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    send_request(&app, &alice, &bob.slug).await;

    // Act
    let response = cancel_request(&app, &alice, &bob.slug).await;
    let status = response.status();
    let inbox = list_requests(&app, &bob, "").await;
    let after = relationship(&app, &bob.slug, Some(&alice)).await;
    let again = send_request(&app, &alice, &bob.slug).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(inbox["results"].as_array().unwrap().is_empty());
    assert_eq!(after, "none");
    assert_eq!(
        again.status(),
        StatusCode::CREATED,
        "depois de cancelar deve ser possível pedir de novo"
    );
}

#[tokio::test]
async fn cancelling_without_a_pending_request_returns_404() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    // Act
    let response = cancel_request(&app, &alice, &bob.slug).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn sent_direction_lists_outgoing_requests() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    send_request(&app, &alice, &bob.slug).await;

    // Act
    let sent = list_requests(&app, &alice, "direction=sent&limit=10").await;
    let received = list_requests(&app, &alice, "").await;

    // Tear down
    drop_app(app).await;

    // Assert: em `sent` o perfil listado é o do destinatário
    assert_eq!(sent["results"][0]["slug"], bob.slug);
    assert!(received["results"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn profile_relationship_reflects_each_stage_of_the_flow() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    // Act
    let anonymous = relationship(&app, &alice.slug, None).await;
    let own = relationship(&app, &alice.slug, Some(&alice)).await;
    let stranger = relationship(&app, &bob.slug, Some(&alice)).await;

    send_request(&app, &alice, &bob.slug).await;
    let sent = relationship(&app, &bob.slug, Some(&alice)).await;
    let received = relationship(&app, &alice.slug, Some(&bob)).await;

    let request_id = first_pending_id(&app, &bob).await;
    respond(&app, &bob, &request_id, "accept").await;
    let friends = relationship(&app, &bob.slug, Some(&alice)).await;

    remove_friend(&app, &alice, &bob.slug).await;
    let removed = relationship(&app, &bob.slug, Some(&alice)).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(anonymous, "anonymous");
    assert_eq!(own, "self");
    assert_eq!(stranger, "none");
    assert_eq!(sent, "request_sent");
    assert_eq!(received, "request_received");
    assert_eq!(friends, "friends");
    assert_eq!(removed, "none");
}

#[tokio::test]
async fn profile_flags_when_the_friend_list_is_hidden() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    befriend(&app, &alice, &bob).await;

    sqlx::query!(
        "UPDATE user_profile SET profile_visibility = 'nobody' WHERE slug = $1",
        alice.slug
    )
    .execute(&app.db_pool)
    .await
    .expect("Failed to update visibility");

    // Act
    let hidden = profile_as(&app, &alice.slug, Some(&bob)).await;
    let visible = profile_as(&app, &alice.slug, Some(&alice)).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(hidden["results"]["friends_visible"], false);
    assert!(hidden["results"]["friends"].as_array().unwrap().is_empty());
    assert_eq!(visible["results"]["friends_visible"], true);
    assert_eq!(visible["results"]["friends"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn friend_list_of_unknown_slug_returns_404() {
    // Arrange
    let app = spawn_app().await;

    // Act
    let response = list_friends(&app, "nao-existe-esse-slug", None, "").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// Enche a lista de amigos até o limite, direto no banco: passar pelo endpoint
/// cinco mil vezes deixaria o teste inviável.
async fn fill_friends(app: &TestApp, slug: &str) {
    sqlx::query!(
        r#"
        WITH target AS (
            SELECT user_id FROM user_profile WHERE slug = $1
        ), filler AS (
        INSERT INTO auth_user (email, password_hash)
            SELECT 'filler-' || i || '@test.com', 'x'
            FROM generate_series(1, $2::bigint) AS i
        RETURNING
            id
        )
        INSERT INTO friend (user_id, friend_id)
        SELECT
            target.user_id,
            filler.id
        FROM
            target,
            filler
        "#,
        slug,
        MAX_FRIENDS,
    )
    .execute(&app.db_pool)
    .await
    .expect("Failed to fill the friend list");
}

#[tokio::test]
async fn the_friend_limit_blocks_new_requests_on_both_sides() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    fill_friends(&app, &alice.slug).await;

    // Act
    let to_full_profile = send_request(&app, &bob, &alice.slug).await;
    let to_full_profile_status = to_full_profile.status();
    let to_full_profile_body: serde_json::Value =
        to_full_profile.json().await.expect("response not JSON");

    let from_full_profile = send_request(&app, &alice, &bob.slug).await;
    let from_full_profile_status = from_full_profile.status();
    let from_full_profile_body: serde_json::Value =
        from_full_profile.json().await.expect("response not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(to_full_profile_status, StatusCode::CONFLICT);
    assert_eq!(
        to_full_profile_body["error"]["friend"][0]["code"],
        "target_friend_limit_reached"
    );
    assert_eq!(from_full_profile_status, StatusCode::CONFLICT);
    assert_eq!(
        from_full_profile_body["error"]["friend"][0]["code"],
        "friend_limit_reached"
    );
}

#[tokio::test]
async fn accepting_over_the_friend_limit_keeps_the_request_pending() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    send_request(&app, &bob, &alice.slug).await;
    let inbox = list_requests(&app, &alice, "").await;
    let request_id = inbox["results"][0]["id"].as_str().unwrap().to_string();

    // O limite estoura entre o pedido e a resposta.
    fill_friends(&app, &alice.slug).await;

    // Act
    let response = respond(&app, &alice, &request_id, "accept").await;
    let status = response.status();
    let body: serde_json::Value = response.json().await.expect("response not JSON");
    let still_pending = list_requests(&app, &alice, "").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["friend"][0]["code"], "friend_limit_reached");
    assert_eq!(still_pending["results"].as_array().unwrap().len(), 1);
}
