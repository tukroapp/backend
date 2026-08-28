use reqwest::StatusCode;
use serde_json::json;
use tukro_backend::constants::MAX_COMMUNITIES_PER_USER;

use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};

async fn create(
    app: &TestApp,
    user: &RegisteredUser,
    name: &str,
    is_private: bool,
) -> reqwest::Response {
    app.api_client
        .post(format!("{}/api/v1/community", &app.address))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .json(&json!({ "name": name, "category": "outros", "is_private": is_private }))
        .send()
        .await
        .expect("Failed to create community")
}

/// Cria a comunidade e devolve o slug gerado.
async fn create_community(
    app: &TestApp,
    user: &RegisteredUser,
    name: &str,
    is_private: bool,
) -> String {
    let body: serde_json::Value = create(app, user, name, is_private)
        .await
        .json()
        .await
        .expect("create community response not JSON");

    body["results"]["slug"]
        .as_str()
        .expect("slug not found")
        .to_string()
}

async fn get_community(
    app: &TestApp,
    slug: &str,
    viewer: Option<&RegisteredUser>,
) -> reqwest::Response {
    let request = app
        .api_client
        .get(format!("{}/api/v1/community/{}", &app.address, slug));

    let request = match viewer {
        Some(viewer) => request.header(reqwest::header::AUTHORIZATION, viewer.auth_header()),
        None => request,
    };

    request.send().await.expect("Failed to get community")
}

async fn join(app: &TestApp, user: &RegisteredUser, slug: &str) -> reqwest::Response {
    app.api_client
        .post(format!("{}/api/v1/community/{}/member", &app.address, slug))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to join community")
}

async fn leave(app: &TestApp, user: &RegisteredUser, slug: &str) -> reqwest::Response {
    app.api_client
        .delete(format!("{}/api/v1/community/{}/member", &app.address, slug))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to leave community")
}

async fn kick(
    app: &TestApp,
    user: &RegisteredUser,
    slug: &str,
    member_slug: &str,
) -> reqwest::Response {
    app.api_client
        .delete(format!(
            "{}/api/v1/community/{}/member/{}",
            &app.address, slug, member_slug
        ))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to kick member")
}

async fn set_role(
    app: &TestApp,
    user: &RegisteredUser,
    slug: &str,
    member_slug: &str,
    role: &str,
) -> reqwest::Response {
    app.api_client
        .put(format!(
            "{}/api/v1/community/{}/member/{}/role",
            &app.address, slug, member_slug
        ))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .json(&json!({ "role": role }))
        .send()
        .await
        .expect("Failed to change member role")
}

async fn transfer(
    app: &TestApp,
    user: &RegisteredUser,
    slug: &str,
    new_owner_slug: &str,
) -> reqwest::Response {
    app.api_client
        .put(format!("{}/api/v1/community/{}/owner", &app.address, slug))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .json(&json!({ "slug": new_owner_slug }))
        .send()
        .await
        .expect("Failed to transfer community")
}

async fn list_members(
    app: &TestApp,
    slug: &str,
    viewer: Option<&RegisteredUser>,
) -> reqwest::Response {
    let request = app
        .api_client
        .get(format!("{}/api/v1/community/{}/member", &app.address, slug));

    let request = match viewer {
        Some(viewer) => request.header(reqwest::header::AUTHORIZATION, viewer.auth_header()),
        None => request,
    };

    request.send().await.expect("Failed to list members")
}

async fn list_join_requests(app: &TestApp, user: &RegisteredUser, slug: &str) -> reqwest::Response {
    app.api_client
        .get(format!(
            "{}/api/v1/community/{}/join-request",
            &app.address, slug
        ))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to list join requests")
}

async fn respond_request(
    app: &TestApp,
    user: &RegisteredUser,
    slug: &str,
    request_id: &str,
    action: &str,
) -> reqwest::Response {
    app.api_client
        .post(format!(
            "{}/api/v1/community/{}/join-request/{}/{}",
            &app.address, slug, request_id, action
        ))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to respond to join request")
}

async fn notifications(app: &TestApp, user: &RegisteredUser) -> serde_json::Value {
    app.api_client
        .get(format!("{}/api/v1/notification", &app.address))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to list notifications")
        .json()
        .await
        .expect("notification list not JSON")
}

async fn json_of(response: reqwest::Response) -> serde_json::Value {
    response.json().await.expect("response not JSON")
}

#[tokio::test]
async fn creating_a_community_derives_the_slug_and_makes_the_creator_owner() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;

    // Act
    let response = create(&app, &alice, "Eu Odeio Acordar Cedo", false).await;
    let status = response.status();
    let body = json_of(response).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["results"]["slug"], "eu-odeio-acordar-cedo");
    assert_eq!(body["results"]["my_role"], "owner");
    assert_eq!(body["results"]["members_count"], 1);
    assert_eq!(body["results"]["can_edit"], true);
    assert_eq!(body["results"]["can_join"], false);
}

#[tokio::test]
async fn communities_with_the_same_name_get_different_slugs() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    // Act
    let first = create_community(&app, &alice, "Eu odeio acordar cedo", false).await;
    let second = create_community(&app, &bob, "Eu odeio acordar cedo", false).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(first, "eu-odeio-acordar-cedo");
    assert_ne!(first, second);
    assert!(second.starts_with("eu-odeio-acordar-cedo-"));
}

#[tokio::test]
async fn only_the_owner_edits_and_deletes_the_community() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let slug = create_community(&app, &alice, "Clube do Livro", false).await;
    join(&app, &bob, &slug).await;

    // Act
    let member_edit = app
        .api_client
        .put(format!("{}/api/v1/community/{}", &app.address, slug))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .json(&json!({ "name": "Clube do Bob", "category": "outros" }))
        .send()
        .await
        .expect("Failed to edit community");

    let member_delete = app
        .api_client
        .delete(format!("{}/api/v1/community/{}", &app.address, slug))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .send()
        .await
        .expect("Failed to delete community");

    let owner_edit = app
        .api_client
        .put(format!("{}/api/v1/community/{}", &app.address, slug))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .json(&json!({ "name": "Clube da Alice", "category": "outros", "is_private": true }))
        .send()
        .await
        .expect("Failed to edit community");

    let owner_edit_body = json_of(owner_edit).await;

    let owner_delete = app
        .api_client
        .delete(format!("{}/api/v1/community/{}", &app.address, slug))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .send()
        .await
        .expect("Failed to delete community");

    let after_delete = get_community(&app, &slug, Some(&alice)).await.status();

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(member_edit.status(), StatusCode::FORBIDDEN);
    assert_eq!(member_delete.status(), StatusCode::FORBIDDEN);
    assert_eq!(owner_edit_body["results"]["name"], "Clube da Alice");
    assert_eq!(owner_edit_body["results"]["is_private"], true);
    // O slug não acompanha o nome.
    assert_eq!(owner_edit_body["results"]["slug"], slug);
    assert_eq!(owner_delete.status(), StatusCode::NO_CONTENT);
    assert_eq!(after_delete, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn joining_a_public_community_is_immediate() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let slug = create_community(&app, &alice, "Vôlei de Domingo", false).await;

    // Act
    let response = join(&app, &bob, &slug).await;
    let status = response.status();
    let body = json_of(response).await;
    let repeated = join(&app, &bob, &slug).await.status();
    let community = json_of(get_community(&app, &slug, Some(&bob)).await).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["results"]["status"], "joined");
    assert_eq!(repeated, StatusCode::CONFLICT);
    assert_eq!(community["results"]["my_role"], "member");
    assert_eq!(community["results"]["members_count"], 2);
}

#[tokio::test]
async fn joining_a_private_community_creates_a_request_and_notifies_the_moderation() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let slug = create_community(&app, &alice, "Sociedade Secreta", true).await;

    // Act
    let response = join(&app, &bob, &slug).await;
    let status = response.status();
    let body = json_of(response).await;
    let repeated = join(&app, &bob, &slug).await.status();

    let requests = json_of(list_join_requests(&app, &alice, &slug).await).await;
    let owner_notifications = notifications(&app, &alice).await;
    let requester_view = json_of(get_community(&app, &slug, Some(&bob)).await).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["results"]["status"], "pending");
    assert_eq!(repeated, StatusCode::CONFLICT);
    assert_eq!(requests["results"].as_array().unwrap().len(), 1);
    assert_eq!(requests["results"][0]["profile"]["slug"], bob.slug);
    assert_eq!(
        owner_notifications["results"][0]["kind"],
        "community_join_request"
    );
    assert_eq!(owner_notifications["results"][0]["community_slug"], slug);
    assert_eq!(requester_view["results"]["has_pending_request"], true);
    assert_eq!(
        requester_view["results"]["my_role"],
        serde_json::Value::Null
    );
}

#[tokio::test]
async fn accepting_a_request_adds_the_member_and_notifies_the_requester() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let slug = create_community(&app, &alice, "Sociedade Secreta", true).await;
    join(&app, &bob, &slug).await;

    let requests = json_of(list_join_requests(&app, &alice, &slug).await).await;
    let request_id = requests["results"][0]["id"].as_str().unwrap().to_string();

    // Act
    let accepted = respond_request(&app, &alice, &slug, &request_id, "accept").await;
    let community = json_of(get_community(&app, &slug, Some(&bob)).await).await;
    let requester_notifications = notifications(&app, &bob).await;
    let remaining = json_of(list_join_requests(&app, &alice, &slug).await).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(accepted.status(), StatusCode::NO_CONTENT);
    assert_eq!(community["results"]["my_role"], "member");
    assert_eq!(community["results"]["members_count"], 2);
    assert_eq!(
        requester_notifications["results"][0]["kind"],
        "community_join_accepted"
    );
    assert_eq!(
        requester_notifications["results"][0]["community_slug"],
        slug
    );
    assert!(remaining["results"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_rejected_request_can_be_made_again() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let slug = create_community(&app, &alice, "Sociedade Secreta", true).await;
    join(&app, &bob, &slug).await;

    let requests = json_of(list_join_requests(&app, &alice, &slug).await).await;
    let request_id = requests["results"][0]["id"].as_str().unwrap().to_string();

    // Act
    let rejected = respond_request(&app, &alice, &slug, &request_id, "reject").await;
    let after_reject = json_of(get_community(&app, &slug, Some(&bob)).await).await;
    let again = json_of(join(&app, &bob, &slug).await).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(rejected.status(), StatusCode::NO_CONTENT);
    assert_eq!(after_reject["results"]["has_pending_request"], false);
    assert_eq!(again["results"]["status"], "pending");
}

#[tokio::test]
async fn the_requester_cancels_the_own_pending_request() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let slug = create_community(&app, &alice, "Sociedade Secreta", true).await;
    join(&app, &bob, &slug).await;

    // Act
    let cancelled = app
        .api_client
        .delete(format!(
            "{}/api/v1/community/{}/join-request",
            &app.address, slug
        ))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .send()
        .await
        .expect("Failed to cancel join request");

    let repeated = app
        .api_client
        .delete(format!(
            "{}/api/v1/community/{}/join-request",
            &app.address, slug
        ))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .send()
        .await
        .expect("Failed to cancel join request");

    let requests = json_of(list_join_requests(&app, &alice, &slug).await).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(cancelled.status(), StatusCode::NO_CONTENT);
    assert_eq!(repeated.status(), StatusCode::NOT_FOUND);
    assert!(requests["results"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn the_member_list_of_a_private_community_is_hidden_from_outsiders() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let slug = create_community(&app, &alice, "Sociedade Secreta", true).await;

    // Act
    let anonymous = list_members(&app, &slug, None).await.status();
    let outsider = list_members(&app, &slug, Some(&bob)).await.status();
    let owner = list_members(&app, &slug, Some(&alice)).await;
    let owner_status = owner.status();
    let owner_body = json_of(owner).await;
    let outsider_detail = json_of(get_community(&app, &slug, Some(&bob)).await).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(anonymous, StatusCode::FORBIDDEN);
    assert_eq!(outsider, StatusCode::FORBIDDEN);
    assert_eq!(owner_status, StatusCode::OK);
    assert_eq!(owner_body["results"].as_array().unwrap().len(), 1);
    // A comunidade continua visível, só os membros não.
    assert_eq!(outsider_detail["results"]["members_visible"], false);
    assert!(
        outsider_detail["results"]["members"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(outsider_detail["results"]["members_count"], 1);
}

#[tokio::test]
async fn only_the_owner_changes_roles() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let carol = register_and_login(&app, "Carol").await;
    let slug = create_community(&app, &alice, "Clube do Livro", false).await;
    join(&app, &bob, &slug).await;
    join(&app, &carol, &slug).await;

    // Act
    let promoted = set_role(&app, &alice, &slug, &bob.slug, "admin").await;
    let by_moderator = set_role(&app, &bob, &slug, &carol.slug, "admin").await;
    let to_owner = set_role(&app, &alice, &slug, &carol.slug, "owner").await;
    let moderator_view = json_of(get_community(&app, &slug, Some(&bob)).await).await;
    let demoted = set_role(&app, &alice, &slug, &bob.slug, "member").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(promoted.status(), StatusCode::NO_CONTENT);
    assert_eq!(by_moderator.status(), StatusCode::FORBIDDEN);
    assert_eq!(to_owner.status(), StatusCode::BAD_REQUEST);
    assert_eq!(moderator_view["results"]["my_role"], "admin");
    assert_eq!(moderator_view["results"]["can_moderate"], true);
    assert_eq!(moderator_view["results"]["can_edit"], false);
    assert_eq!(demoted.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn a_moderator_kicks_only_common_members() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let carol = register_and_login(&app, "Carol").await;
    let dave = register_and_login(&app, "Dave").await;
    let slug = create_community(&app, &alice, "Clube do Livro", false).await;
    join(&app, &bob, &slug).await;
    join(&app, &carol, &slug).await;
    join(&app, &dave, &slug).await;
    set_role(&app, &alice, &slug, &bob.slug, "admin").await;
    set_role(&app, &alice, &slug, &carol.slug, "admin").await;

    // Act
    let kicked_member = kick(&app, &bob, &slug, &dave.slug).await;
    let kicked_moderator = kick(&app, &bob, &slug, &carol.slug).await;
    let kicked_owner = kick(&app, &bob, &slug, &alice.slug).await;
    let kicked_by_owner = kick(&app, &alice, &slug, &carol.slug).await;
    let kicked_self = kick(&app, &bob, &slug, &bob.slug).await;
    let community = json_of(get_community(&app, &slug, Some(&alice)).await).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(kicked_member.status(), StatusCode::NO_CONTENT);
    assert_eq!(kicked_moderator.status(), StatusCode::FORBIDDEN);
    assert_eq!(kicked_owner.status(), StatusCode::FORBIDDEN);
    assert_eq!(kicked_by_owner.status(), StatusCode::NO_CONTENT);
    assert_eq!(kicked_self.status(), StatusCode::BAD_REQUEST);
    assert_eq!(community["results"]["members_count"], 2);
}

#[tokio::test]
async fn the_owner_leaves_only_after_transferring_the_community() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let carol = register_and_login(&app, "Carol").await;
    let slug = create_community(&app, &alice, "Clube do Livro", false).await;
    join(&app, &bob, &slug).await;

    // Act
    let owner_leaving = leave(&app, &alice, &slug).await;
    let to_outsider = transfer(&app, &alice, &slug, &carol.slug).await;
    let to_self = transfer(&app, &alice, &slug, &alice.slug).await;
    let transferred = transfer(&app, &alice, &slug, &bob.slug).await;

    let old_owner_view = json_of(get_community(&app, &slug, Some(&alice)).await).await;
    let new_owner_view = json_of(get_community(&app, &slug, Some(&bob)).await).await;
    let leaving_after_transfer = leave(&app, &alice, &slug).await;
    let by_old_owner = transfer(&app, &alice, &slug, &bob.slug).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(owner_leaving.status(), StatusCode::CONFLICT);
    assert_eq!(to_outsider.status(), StatusCode::NOT_FOUND);
    assert_eq!(to_self.status(), StatusCode::BAD_REQUEST);
    assert_eq!(transferred.status(), StatusCode::NO_CONTENT);
    assert_eq!(old_owner_view["results"]["my_role"], "admin");
    assert_eq!(new_owner_view["results"]["my_role"], "owner");
    assert_eq!(leaving_after_transfer.status(), StatusCode::NO_CONTENT);
    assert_eq!(by_old_owner.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_search_finds_communities_by_name_and_lists_them_on_the_profile() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let slug = create_community(&app, &alice, "Fotografia Analógica", false).await;
    create_community(&app, &alice, "Culinária Vegetariana", false).await;
    join(&app, &bob, &slug).await;

    // Act
    let search = json_of(
        app.api_client
            .get(format!("{}/api/v1/community?q=fotografia", &app.address))
            .send()
            .await
            .expect("Failed to search communities"),
    )
    .await;

    let short_term = app
        .api_client
        .get(format!("{}/api/v1/community?q=f", &app.address))
        .send()
        .await
        .expect("Failed to search communities")
        .status();

    let listing = json_of(
        app.api_client
            .get(format!("{}/api/v1/community", &app.address))
            .send()
            .await
            .expect("Failed to list communities"),
    )
    .await;

    let profile_communities = json_of(
        app.api_client
            .get(format!(
                "{}/api/v1/profile/{}/community",
                &app.address, bob.slug
            ))
            .send()
            .await
            .expect("Failed to list profile communities"),
    )
    .await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(search["results"].as_array().unwrap().len(), 1);
    assert_eq!(search["results"][0]["slug"], slug);
    assert_eq!(search["results"][0]["members_count"], 2);
    assert_eq!(short_term, StatusCode::BAD_REQUEST);
    assert_eq!(listing["results"].as_array().unwrap().len(), 2);
    assert_eq!(profile_communities["results"].as_array().unwrap().len(), 1);
    assert_eq!(profile_communities["results"][0]["slug"], slug);
}

#[tokio::test]
async fn writing_requires_a_session() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let slug = create_community(&app, &alice, "Clube do Livro", false).await;

    // Act
    let create_without_session = app
        .api_client
        .post(format!("{}/api/v1/community", &app.address))
        .json(&json!({ "name": "Sem sessão", "category": "outros" }))
        .send()
        .await
        .expect("Failed to create community");

    let join_without_session = app
        .api_client
        .post(format!("{}/api/v1/community/{}/member", &app.address, slug))
        .send()
        .await
        .expect("Failed to join community");

    let public_read = get_community(&app, &slug, None).await;
    let public_read_status = public_read.status();
    let public_body = json_of(public_read).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(create_without_session.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(join_without_session.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(public_read_status, StatusCode::OK);
    assert_eq!(public_body["results"]["can_join"], false);
    assert_eq!(public_body["results"]["members_visible"], true);
}

/// Enche a lista de comunidades até o limite, direto no banco: passar pelo
/// endpoint mil vezes deixaria o teste inviável.
async fn fill_communities(app: &TestApp, slug: &str) {
    sqlx::query!(
        r#"
        WITH target AS (
            SELECT user_id FROM user_profile WHERE slug = $1
        ), filler AS (
        INSERT INTO community (slug, name, category)
            SELECT 'filler-' || i, 'Filler ' || i, 'outros'
            FROM generate_series(1, $2::bigint) AS i
        RETURNING
            id
        )
        INSERT INTO community_member (community_id, user_id)
        SELECT
            filler.id,
            target.user_id
        FROM
            filler,
            target
        "#,
        slug,
        MAX_COMMUNITIES_PER_USER,
    )
    .execute(&app.db_pool)
    .await
    .expect("Failed to fill the community list");
}

#[tokio::test]
async fn the_community_limit_blocks_creating_and_joining() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let slug = create_community(&app, &bob, "Clube do Livro", false).await;
    fill_communities(&app, &alice.slug).await;

    // Act
    let created = create(&app, &alice, "Mais Uma", false).await;
    let created_status = created.status();
    let created_body = json_of(created).await;

    let joined = join(&app, &alice, &slug).await;
    let joined_status = joined.status();
    let joined_body = json_of(joined).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(created_status, StatusCode::CONFLICT);
    assert_eq!(
        created_body["error"]["community"][0]["code"],
        "community_limit_reached"
    );
    assert_eq!(joined_status, StatusCode::CONFLICT);
    assert_eq!(
        joined_body["error"]["community"][0]["code"],
        "community_limit_reached"
    );
}

#[tokio::test]
async fn accepting_over_the_community_limit_keeps_the_request_pending() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let slug = create_community(&app, &alice, "Sociedade Secreta", true).await;
    join(&app, &bob, &slug).await;

    let requests = json_of(list_join_requests(&app, &alice, &slug).await).await;
    let request_id = requests["results"][0]["id"].as_str().unwrap().to_string();

    // O limite estoura entre o pedido e a aprovação.
    fill_communities(&app, &bob.slug).await;

    // Act
    let response = respond_request(&app, &alice, &slug, &request_id, "accept").await;
    let status = response.status();
    let body = json_of(response).await;
    let still_pending = json_of(list_join_requests(&app, &alice, &slug).await).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body["error"]["member"][0]["code"],
        "community_limit_reached"
    );
    assert_eq!(still_pending["results"].as_array().unwrap().len(), 1);
}
