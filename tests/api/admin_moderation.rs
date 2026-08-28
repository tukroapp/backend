use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};

async fn json_of(response: reqwest::Response) -> Value {
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

async fn post_scrap(app: &TestApp, author: &RegisteredUser, wall: &str, body: &str) -> String {
    let response = app
        .api_client
        .post(format!("{}/api/v1/profile/{}/scrap", &app.address, wall))
        .header(reqwest::header::AUTHORIZATION, author.auth_header())
        .json(&json!({ "body": body }))
        .send()
        .await
        .expect("Failed to post scrap");

    json_of(response).await["results"]["id"]
        .as_str()
        .expect("scrap id not found")
        .to_string()
}

async fn create_community(app: &TestApp, owner: &RegisteredUser, name: &str) -> String {
    let response = app
        .api_client
        .post(format!("{}/api/v1/community", &app.address))
        .header(reqwest::header::AUTHORIZATION, owner.auth_header())
        .json(&json!({ "name": name, "is_private": false }))
        .send()
        .await
        .expect("Failed to create community");

    json_of(response).await["results"]["slug"]
        .as_str()
        .expect("slug not found")
        .to_string()
}

async fn report_target(app: &TestApp, reporter: &RegisteredUser, kind: &str, id: &str) -> String {
    let response = app
        .api_client
        .post(format!("{}/api/v1/report/{}/{}", &app.address, kind, id))
        .header(reqwest::header::AUTHORIZATION, reporter.auth_header())
        .json(&json!({ "reason": "harassment" }))
        .send()
        .await
        .expect("Failed to report");
    assert_eq!(response.status(), StatusCode::CREATED);

    json_of(response).await["results"]["id"]
        .as_str()
        .expect("report id not found")
        .to_string()
}

async fn report_profile(app: &TestApp, reporter: &RegisteredUser, slug: &str) -> String {
    let response = app
        .api_client
        .post(format!("{}/api/v1/report/profile/{}", &app.address, slug))
        .header(reqwest::header::AUTHORIZATION, reporter.auth_header())
        .json(&json!({ "reason": "spam" }))
        .send()
        .await
        .expect("Failed to report profile");
    assert_eq!(response.status(), StatusCode::CREATED);

    json_of(response).await["results"]["id"]
        .as_str()
        .expect("report id not found")
        .to_string()
}

async fn remove(app: &TestApp, admin: &RegisteredUser, report_id: &str) -> reqwest::Response {
    app.api_client
        .post(format!(
            "{}/api/v1/admin/report/{}/remove",
            &app.address, report_id
        ))
        .header(reqwest::header::AUTHORIZATION, admin.auth_header())
        .json(&json!({ "reason": "conteúdo abusivo" }))
        .send()
        .await
        .expect("Failed to remove content")
}

async fn restore(app: &TestApp, admin: &RegisteredUser, kind: &str, id: &str) -> reqwest::Response {
    app.api_client
        .post(format!(
            "{}/api/v1/admin/content/{}/{}/restore",
            &app.address, kind, id
        ))
        .header(reqwest::header::AUTHORIZATION, admin.auth_header())
        .json(&json!({ "reason": "denúncia improcedente" }))
        .send()
        .await
        .expect("Failed to restore content")
}

async fn wall_of(app: &TestApp, slug: &str) -> Value {
    app.api_client
        .get(format!("{}/api/v1/profile/{}/scrap", &app.address, slug))
        .send()
        .await
        .expect("Failed to read wall")
        .json()
        .await
        .expect("wall not JSON")
}

#[tokio::test]
async fn removing_reported_content_takes_it_down_and_logs_the_action() {
    // Arrange
    let app = spawn_app().await;
    let admin = register_and_login(&app, "Admin").await;
    promote(&app, &admin).await;

    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    open_wall(&app, &alice).await;
    let scrap_id = post_scrap(&app, &bob, &alice.slug, "compre seguidores").await;
    let report_id = report_target(&app, &alice, "scrap", &scrap_id).await;

    // Act
    let response = remove(&app, &admin, &report_id).await;
    let again = remove(&app, &admin, &report_id).await;

    let wall = wall_of(&app, &alice.slug).await;

    let report_status = sqlx::query!(
        r#"SELECT status::TEXT AS "status!", reviewed_at FROM report WHERE id = $1"#,
        uuid::Uuid::parse_str(&report_id).unwrap(),
    )
    .fetch_one(&app.db_pool)
    .await
    .expect("report não encontrado");

    let log = sqlx::query!(
        r#"
        SELECT
            l.action::TEXT AS "action!",
            l.reason,
            l.content_snapshot,
            l.target_label,
            l.scrap_id,
            l.report_id,
            l.target_user_id
        FROM moderation_log l
        "#,
    )
    .fetch_all(&app.db_pool)
    .await
    .expect("log não encontrado");

    let bob_id = sqlx::query_scalar!(
        r#"SELECT user_id FROM user_profile WHERE slug = $1"#,
        bob.slug,
    )
    .fetch_one(&app.db_pool)
    .await
    .expect("bob não encontrado");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    // O conteúdo já está fora do ar, então a segunda tentativa não repete a ação
    assert_eq!(again.status(), StatusCode::CONFLICT);

    assert!(wall["results"].as_array().unwrap().is_empty());

    assert_eq!(report_status.status, "resolved");
    assert!(report_status.reviewed_at.is_some());

    assert_eq!(log.len(), 1);
    let entry = &log[0];
    assert_eq!(entry.action, "content_removed");
    assert_eq!(entry.reason.as_deref(), Some("conteúdo abusivo"));
    assert_eq!(entry.content_snapshot.as_deref(), Some("compre seguidores"));
    assert_eq!(entry.target_label.as_deref(), Some(bob.slug.as_str()));
    assert_eq!(entry.scrap_id.map(|id| id.to_string()), Some(scrap_id));
    assert_eq!(entry.report_id.map(|id| id.to_string()), Some(report_id));
    // O responsável é o autor do recado
    assert_eq!(entry.target_user_id, Some(bob_id));
}

#[tokio::test]
async fn a_profile_report_cannot_be_resolved_by_removing_content() {
    // Arrange
    let app = spawn_app().await;
    let admin = register_and_login(&app, "Admin").await;
    promote(&app, &admin).await;

    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let report_id = report_profile(&app, &alice, &bob.slug).await;

    // Act
    let response = remove(&app, &admin, &report_id).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn what_the_moderation_removed_can_be_restored() {
    // Arrange
    let app = spawn_app().await;
    let admin = register_and_login(&app, "Admin").await;
    promote(&app, &admin).await;

    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    open_wall(&app, &alice).await;
    let scrap_id = post_scrap(&app, &bob, &alice.slug, "recado polêmico").await;
    let report_id = report_target(&app, &alice, "scrap", &scrap_id).await;
    assert_eq!(
        remove(&app, &admin, &report_id).await.status(),
        StatusCode::NO_CONTENT
    );

    // Act
    let response = restore(&app, &admin, "scrap", &scrap_id).await;
    let again = restore(&app, &admin, "scrap", &scrap_id).await;

    let wall = wall_of(&app, &alice.slug).await;

    let actions =
        sqlx::query_scalar!(r#"SELECT action::TEXT AS "action!" FROM moderation_log ORDER BY id"#,)
            .fetch_all(&app.db_pool)
            .await
            .expect("log não encontrado");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    // Já está no ar, não há o que restaurar
    assert_eq!(again.status(), StatusCode::NOT_FOUND);

    assert_eq!(wall["results"].as_array().unwrap().len(), 1);
    assert_eq!(wall["results"][0]["body"], "recado polêmico");

    assert_eq!(actions, vec!["content_removed", "content_restored"]);
}

#[tokio::test]
async fn what_the_author_deleted_is_not_restored() {
    // Arrange
    let app = spawn_app().await;
    let admin = register_and_login(&app, "Admin").await;
    promote(&app, &admin).await;

    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    open_wall(&app, &alice).await;
    let scrap_id = post_scrap(&app, &bob, &alice.slug, "arrependimento").await;

    let response = app
        .api_client
        .delete(format!("{}/api/v1/scrap/{}", &app.address, scrap_id))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .send()
        .await
        .expect("Failed to delete scrap");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // Act
    let response = restore(&app, &admin, "scrap", &scrap_id).await;
    let unknown_kind = restore(&app, &admin, "recado", &scrap_id).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(unknown_kind.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_removed_community_disappears_from_the_public_api() {
    // Arrange
    let app = spawn_app().await;
    let admin = register_and_login(&app, "Admin").await;
    promote(&app, &admin).await;

    let owner = register_and_login(&app, "Dono").await;
    let reporter = register_and_login(&app, "Denunciante").await;

    let slug = create_community(&app, &owner, "Comunidade Suspeita").await;
    let report_id = report_target(&app, &reporter, "community", &slug).await;

    // Act
    let response = remove(&app, &admin, &report_id).await;

    let detail = app
        .api_client
        .get(format!("{}/api/v1/community/{}", &app.address, slug))
        .send()
        .await
        .expect("Failed to read community");

    let listing = app
        .api_client
        .get(format!("{}/api/v1/community", &app.address))
        .send()
        .await
        .expect("Failed to list communities");
    let listing = json_of(listing).await;

    let mine = app
        .api_client
        .get(format!(
            "{}/api/v1/profile/{}/community",
            &app.address, owner.slug
        ))
        .send()
        .await
        .expect("Failed to list profile communities");
    let mine = json_of(mine).await;

    // O painel continua enxergando
    let queue = app
        .api_client
        .get(format!("{}/api/v1/admin/report", &app.address))
        .header(reqwest::header::AUTHORIZATION, admin.auth_header())
        .send()
        .await
        .expect("Failed to list queue");
    let queue = json_of(queue).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(detail.status(), StatusCode::NOT_FOUND);
    assert!(listing["results"].as_array().unwrap().is_empty());
    assert!(mine["results"].as_array().unwrap().is_empty());
    assert_eq!(queue["results"][0]["target"]["kind"], "community");
    assert_eq!(queue["results"][0]["target"]["is_deleted"], true);
}

#[tokio::test]
async fn the_history_gathers_every_action_against_an_account() {
    // Arrange
    let app = spawn_app().await;
    let admin = register_and_login(&app, "Admin").await;
    promote(&app, &admin).await;

    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    open_wall(&app, &alice).await;
    let scrap_id = post_scrap(&app, &bob, &alice.slug, "compre seguidores").await;
    let report_id = report_target(&app, &alice, "scrap", &scrap_id).await;
    assert_eq!(
        remove(&app, &admin, &report_id).await.status(),
        StatusCode::NO_CONTENT
    );

    let response = app
        .api_client
        .post(format!(
            "{}/api/v1/admin/user/{}/suspend",
            &app.address, bob.slug
        ))
        .header(reqwest::header::AUTHORIZATION, admin.auth_header())
        .json(&json!({ "reason": "spam reincidente" }))
        .send()
        .await
        .expect("Failed to suspend");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // Act
    let history = app
        .api_client
        .get(format!(
            "{}/api/v1/admin/user/{}/history",
            &app.address, bob.slug
        ))
        .header(reqwest::header::AUTHORIZATION, admin.auth_header())
        .send()
        .await
        .expect("Failed to read history");
    let history = json_of(history).await;

    let unknown = app
        .api_client
        .get(format!(
            "{}/api/v1/admin/user/nao-existe/history",
            &app.address
        ))
        .header(reqwest::header::AUTHORIZATION, admin.auth_header())
        .send()
        .await
        .expect("Failed to read history");

    // Tear down
    drop_app(app).await;

    // Assert
    let results = history["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    // Da mais nova para a mais antiga
    assert_eq!(results[0]["action"], "user_suspended");
    assert_eq!(results[0]["reason"], "spam reincidente");
    assert!(results[0]["content_kind"].is_null());
    assert_eq!(results[1]["action"], "content_removed");
    assert_eq!(results[1]["content_kind"], "scrap");
    assert_eq!(results[1]["content_snapshot"], "compre seguidores");
    assert_eq!(results[1]["moderator"]["slug"], admin.slug);

    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn moderation_actions_require_a_superuser() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    open_wall(&app, &alice).await;
    let scrap_id = post_scrap(&app, &bob, &alice.slug, "recado").await;
    let report_id = report_target(&app, &alice, "scrap", &scrap_id).await;

    // Act
    let removal = remove(&app, &alice, &report_id).await;
    let restoration = restore(&app, &alice, "scrap", &scrap_id).await;
    let history = app
        .api_client
        .get(format!(
            "{}/api/v1/admin/user/{}/history",
            &app.address, bob.slug
        ))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .send()
        .await
        .expect("Failed to read history");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(removal.status(), StatusCode::FORBIDDEN);
    assert_eq!(restoration.status(), StatusCode::FORBIDDEN);
    assert_eq!(history.status(), StatusCode::FORBIDDEN);
}
