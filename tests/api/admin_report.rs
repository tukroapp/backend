use reqwest::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};

/// Promove a conta a superusuário direto no banco: não existe (nem deve existir)
/// endpoint para isso.
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

async fn report(app: &TestApp, reporter: &RegisteredUser, slug: &str, reason: &str) -> String {
    let response = app
        .api_client
        .post(format!("{}/api/v1/report/profile/{}", &app.address, slug))
        .header(reqwest::header::AUTHORIZATION, reporter.auth_header())
        .json(&json!({ "reason": reason, "details": "detalhes da denúncia" }))
        .send()
        .await
        .expect("Failed to report");
    assert_eq!(response.status(), StatusCode::CREATED);

    let body: serde_json::Value = response.json().await.expect("report not JSON");
    body["results"]["id"].as_str().unwrap().to_string()
}

async fn queue(app: &TestApp, admin: &RegisteredUser, query: &str) -> serde_json::Value {
    app.api_client
        .get(format!("{}/api/v1/admin/report?{}", &app.address, query))
        .header(reqwest::header::AUTHORIZATION, admin.auth_header())
        .send()
        .await
        .expect("Failed to list queue")
        .json()
        .await
        .expect("queue not JSON")
}

async fn detail(app: &TestApp, admin: &RegisteredUser, id: &str) -> reqwest::Response {
    app.api_client
        .get(format!("{}/api/v1/admin/report/{}", &app.address, id))
        .header(reqwest::header::AUTHORIZATION, admin.auth_header())
        .send()
        .await
        .expect("Failed to get detail")
}

async fn review(
    app: &TestApp,
    admin: &RegisteredUser,
    id: &str,
    action: &str,
) -> reqwest::Response {
    app.api_client
        .post(format!(
            "{}/api/v1/admin/report/{}/{}",
            &app.address, id, action
        ))
        .header(reqwest::header::AUTHORIZATION, admin.auth_header())
        .send()
        .await
        .expect("Failed to review report")
}

async fn post_scrap(app: &TestApp, author: &RegisteredUser, slug: &str, body: &str) {
    let response = app
        .api_client
        .post(format!("{}/api/v1/profile/{}/scrap", &app.address, slug))
        .header(reqwest::header::AUTHORIZATION, author.auth_header())
        .json(&json!({ "body": body }))
        .send()
        .await
        .expect("Failed to post scrap");
    assert_eq!(response.status(), StatusCode::CREATED);
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
async fn the_queue_lists_reports_and_filters_by_status() {
    // Arrange
    let app = spawn_app().await;
    let admin = register_and_login(&app, "Admin").await;
    promote(&app, &admin).await;

    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let carol = register_and_login(&app, "Carol").await;

    let first = report(&app, &bob, &alice.slug, "spam").await;
    report(&app, &carol, &alice.slug, "harassment").await;

    assert_eq!(
        review(&app, &admin, &first, "resolve").await.status(),
        StatusCode::NO_CONTENT
    );

    // Act
    let all = queue(&app, &admin, "").await;
    let open = queue(&app, &admin, "status=open").await;
    let resolved = queue(&app, &admin, "status=resolved").await;
    let invalid = app
        .api_client
        .get(format!("{}/api/v1/admin/report?status=nope", &app.address))
        .header(reqwest::header::AUTHORIZATION, admin.auth_header())
        .send()
        .await
        .expect("Failed to list queue");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(all["results"].as_array().unwrap().len(), 2);
    assert_eq!(all["results"][0]["reporter"]["slug"], carol.slug);
    assert_eq!(
        all["results"][0]["accountable"]["profile"]["slug"],
        alice.slug
    );
    assert_eq!(all["results"][0]["target"]["kind"], "profile");
    assert_eq!(all["results"][0]["target"]["slug"], alice.slug);

    assert_eq!(open["results"].as_array().unwrap().len(), 1);
    assert_eq!(open["results"][0]["reporter"]["slug"], carol.slug);

    assert_eq!(resolved["results"].as_array().unwrap().len(), 1);
    assert_eq!(resolved["results"][0]["id"], first);

    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_detail_brings_the_reported_user_history() {
    // Arrange
    let app = spawn_app().await;
    let admin = register_and_login(&app, "Admin").await;
    promote(&app, &admin).await;

    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let carol = register_and_login(&app, "Carol").await;

    // A conta denunciada é a do bob, que espalhou recado por aí
    open_wall(&app, &alice).await;
    open_wall(&app, &carol).await;
    post_scrap(&app, &bob, &alice.slug, "compre seguidores").await;
    post_scrap(&app, &bob, &carol.slug, "compre seguidores").await;

    // E foi bloqueado por uma delas
    let response = app
        .api_client
        .post(format!("{}/api/v1/block/{}", &app.address, bob.slug))
        .header(reqwest::header::AUTHORIZATION, carol.auth_header())
        .send()
        .await
        .expect("Failed to block");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let id = report(&app, &alice, &bob.slug, "spam").await;
    report(&app, &carol, &bob.slug, "spam").await;

    // Act
    let response = detail(&app, &admin, &id).await;
    let status = response.status();
    let body: serde_json::Value = response.json().await.expect("detail not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::OK);
    let results = &body["results"];

    assert_eq!(results["reason"], "spam");
    assert_eq!(results["status"], "open");
    assert_eq!(results["reporter"]["slug"], alice.slug);
    assert!(results["reviewed_by"].is_null());
    assert!(results["reviewed_at"].is_null());

    // Dados da conta denunciada que o moderador precisa ver
    let reported = &results["reported"];
    assert_eq!(reported["slug"], bob.slug);
    assert_eq!(reported["email"], bob.email);
    assert_eq!(reported["account_status"], "active");
    assert!(reported["suspended_until"].is_null());
    assert_eq!(reported["scraps_written_count"], 2);
    assert_eq!(reported["blocked_by_count"], 1);
    assert_eq!(reported["reports_count"], 2);

    // Os recados que ele escreveu, com o mural de cada um
    let scraps = results["recent_scraps"].as_array().unwrap();
    assert_eq!(scraps.len(), 2);
    assert_eq!(scraps[0]["body"], "compre seguidores");
    assert_eq!(scraps[0]["is_reply"], false);
    let walls: Vec<&str> = scraps
        .iter()
        .map(|s| s["wall_slug"].as_str().unwrap())
        .collect();
    assert!(walls.contains(&carol.slug.as_str()));
    assert!(walls.contains(&alice.slug.as_str()));

    // A outra denúncia contra a mesma conta, sem repetir a que está aberta
    let others = results["other_reports"].as_array().unwrap();
    assert_eq!(others.len(), 1);
    assert_eq!(others[0]["reporter_slug"], carol.slug);
    assert_ne!(others[0]["id"], id.as_str());
}

#[tokio::test]
async fn reviewing_records_the_moderator_without_closing_the_report() {
    // Arrange
    let app = spawn_app().await;
    let admin = register_and_login(&app, "Admin").await;
    promote(&app, &admin).await;

    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let id = report(&app, &bob, &alice.slug, "spam").await;

    // Act
    let response = review(&app, &admin, &id, "reviewing").await;
    let body: serde_json::Value = detail(&app, &admin, &id)
        .await
        .json()
        .await
        .expect("detail not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(body["results"]["status"], "reviewing");
    assert_eq!(body["results"]["reviewed_by"]["slug"], admin.slug);
    assert!(
        body["results"]["reviewed_at"].is_null(),
        "reviewed_at marca a conclusão, não a análise"
    );
}

#[tokio::test]
async fn resolving_and_dismissing_close_the_report() {
    // Arrange
    let app = spawn_app().await;
    let admin = register_and_login(&app, "Admin").await;
    promote(&app, &admin).await;

    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let carol = register_and_login(&app, "Carol").await;

    let resolved = report(&app, &bob, &alice.slug, "spam").await;
    let dismissed = report(&app, &carol, &alice.slug, "spam").await;

    // Act
    review(&app, &admin, &resolved, "resolve").await;
    review(&app, &admin, &dismissed, "dismiss").await;

    let resolved: serde_json::Value = detail(&app, &admin, &resolved)
        .await
        .json()
        .await
        .expect("detail not JSON");
    let dismissed: serde_json::Value = detail(&app, &admin, &dismissed)
        .await
        .json()
        .await
        .expect("detail not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(resolved["results"]["status"], "resolved");
    assert_eq!(resolved["results"]["reviewed_by"]["slug"], admin.slug);
    assert!(!resolved["results"]["reviewed_at"].is_null());

    assert_eq!(dismissed["results"]["status"], "dismissed");
    assert!(!dismissed["results"]["reviewed_at"].is_null());
}

#[tokio::test]
async fn a_regular_user_cannot_reach_the_admin_area() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let id = report(&app, &bob, &alice.slug, "spam").await;

    // Act
    let listed = app
        .api_client
        .get(format!("{}/api/v1/admin/report", &app.address))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .send()
        .await
        .expect("Failed to list queue");
    let detailed = detail(&app, &bob, &id).await;
    let reviewed = review(&app, &bob, &id, "resolve").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(listed.status(), StatusCode::FORBIDDEN);
    assert_eq!(detailed.status(), StatusCode::FORBIDDEN);
    assert_eq!(reviewed.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_admin_area_requires_authentication() {
    // Arrange
    let app = spawn_app().await;

    // Act
    let response = app
        .api_client
        .get(format!("{}/api/v1/admin/report", &app.address))
        .send()
        .await
        .expect("Failed to list queue");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn unknown_report_returns_404() {
    // Arrange
    let app = spawn_app().await;
    let admin = register_and_login(&app, "Admin").await;
    promote(&app, &admin).await;
    let unknown = Uuid::now_v7().to_string();

    // Act
    let detailed = detail(&app, &admin, &unknown).await;
    let reviewed = review(&app, &admin, &unknown, "resolve").await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(detailed.status(), StatusCode::NOT_FOUND);
    assert_eq!(reviewed.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_queue_walks_every_page_with_the_cursor() {
    // Arrange
    let app = spawn_app().await;
    let admin = register_and_login(&app, "Admin").await;
    promote(&app, &admin).await;

    let alice = register_and_login(&app, "Alice").await;

    let mut expected = Vec::new();
    for i in 0..3 {
        let reporter = register_and_login(&app, &format!("Denunciante {i}")).await;
        expected.push(report(&app, &reporter, &alice.slug, "spam").await);
    }

    // Act
    let mut seen = Vec::new();
    let mut query = "limit=2".to_string();
    loop {
        let body = queue(&app, &admin, &query).await;

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
async fn the_panel_sees_what_the_user_deleted() {
    // Arrange
    let app = spawn_app().await;
    let admin = register_and_login(&app, "Admin").await;
    promote(&app, &admin).await;

    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    open_wall(&app, &alice).await;

    // O bob posta algo grave e apaga em seguida
    let response = app
        .api_client
        .post(format!(
            "{}/api/v1/profile/{}/scrap",
            &app.address, alice.slug
        ))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .json(&json!({ "body": "conteúdo grave" }))
        .send()
        .await
        .expect("Failed to post scrap");
    assert_eq!(response.status(), StatusCode::CREATED);

    let created: serde_json::Value = response.json().await.expect("scrap not JSON");
    let scrap = created["results"]["id"].as_str().unwrap().to_string();

    post_scrap(&app, &bob, &alice.slug, "conteúdo inocente").await;

    let response = app
        .api_client
        .delete(format!("{}/api/v1/scrap/{}", &app.address, scrap))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .send()
        .await
        .expect("Failed to delete scrap");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let id = report(&app, &alice, &bob.slug, "harassment").await;

    // Act
    let body: serde_json::Value = detail(&app, &admin, &id)
        .await
        .json()
        .await
        .expect("detail not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    let scraps = body["results"]["recent_scraps"].as_array().unwrap();
    assert_eq!(
        scraps.len(),
        2,
        "o apagado continua visível para a moderação"
    );

    let deleted = scraps
        .iter()
        .find(|item| item["id"] == scrap.as_str())
        .expect("o recado apagado sumiu do painel");
    assert_eq!(deleted["body"], "conteúdo grave");
    assert_eq!(deleted["is_deleted"], true);

    let kept = scraps
        .iter()
        .find(|item| item["body"] == "conteúdo inocente")
        .expect("o recado normal sumiu do painel");
    assert_eq!(kept["is_deleted"], false);
}

async fn json_of(response: reqwest::Response) -> serde_json::Value {
    response.json().await.expect("response not JSON")
}

/// Escreve um recado e devolve o id, para as denúncias de conteúdo.
async fn post_scrap_returning_id(
    app: &TestApp,
    author: &RegisteredUser,
    slug: &str,
    body: &str,
) -> String {
    let response = app
        .api_client
        .post(format!("{}/api/v1/profile/{}/scrap", &app.address, slug))
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

async fn report_target(app: &TestApp, reporter: &RegisteredUser, kind: &str, id: &str) -> String {
    let response = app
        .api_client
        .post(format!("{}/api/v1/report/{}/{}", &app.address, kind, id))
        .header(reqwest::header::AUTHORIZATION, reporter.auth_header())
        .json(&json!({ "reason": "harassment", "details": "conteúdo abusivo" }))
        .send()
        .await
        .expect("Failed to report target");

    assert_eq!(response.status(), StatusCode::CREATED);

    json_of(response).await["results"]["id"]
        .as_str()
        .expect("report id not found")
        .to_string()
}

#[tokio::test]
async fn a_content_report_brings_the_target_and_its_author() {
    // Arrange
    let app = spawn_app().await;
    let admin = register_and_login(&app, "Admin").await;
    promote(&app, &admin).await;

    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    open_wall(&app, &alice).await;
    let scrap_id = post_scrap_returning_id(&app, &bob, &alice.slug, "compre seguidores").await;
    let id = report_target(&app, &alice, "scrap", &scrap_id).await;

    // Act
    let scraps = queue(&app, &admin, "kind=scrap").await;
    let empty = queue(&app, &admin, "kind=profile").await;
    let body = json_of(detail(&app, &admin, &id).await).await;

    // Tear down
    drop_app(app).await;

    // Assert
    let item = &scraps["results"][0];
    assert_eq!(item["target"]["kind"], "scrap");
    assert_eq!(item["target"]["id"], scrap_id);
    // O mural é o link do conteúdo
    assert_eq!(item["target"]["slug"], alice.slug);
    assert_eq!(item["target"]["excerpt"], "compre seguidores");
    assert_eq!(item["target"]["is_deleted"], false);
    // O responsável é o autor do recado, não o dono do mural
    assert_eq!(item["accountable"]["profile"]["slug"], bob.slug);
    assert_eq!(item["accountable"]["account_status"], "active");

    assert!(empty["results"].as_array().unwrap().is_empty());

    let results = &body["results"];
    assert_eq!(results["target"]["kind"], "scrap");
    assert_eq!(results["target"]["body"], "compre seguidores");
    assert_eq!(results["target"]["wall_slug"], alice.slug);
    assert_eq!(results["reported"]["slug"], bob.slug);
    assert_eq!(results["reported"]["email"], bob.email);
}

#[tokio::test]
async fn a_community_report_has_no_accountable_user() {
    // Arrange
    let app = spawn_app().await;
    let admin = register_and_login(&app, "Admin").await;
    promote(&app, &admin).await;

    let owner = register_and_login(&app, "Dono").await;
    let reporter = register_and_login(&app, "Denunciante").await;

    let response = app
        .api_client
        .post(format!("{}/api/v1/community", &app.address))
        .header(reqwest::header::AUTHORIZATION, owner.auth_header())
        .json(&json!({ "name": "Comunidade Suspeita", "category": "outros", "is_private": false }))
        .send()
        .await
        .expect("Failed to create community");
    let slug = json_of(response).await["results"]["slug"]
        .as_str()
        .expect("slug not found")
        .to_string();

    let id = report_target(&app, &reporter, "community", &slug).await;

    // Act
    let communities = queue(&app, &admin, "").await;
    let body = json_of(detail(&app, &admin, &id).await).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(communities["results"][0]["target"]["kind"], "community");
    assert_eq!(
        communities["results"][0]["target"]["title"],
        "Comunidade Suspeita"
    );
    assert!(communities["results"][0]["accountable"].is_null());

    let results = &body["results"];
    assert_eq!(results["target"]["kind"], "community");
    assert_eq!(results["target"]["slug"], slug);
    assert_eq!(results["target"]["members_count"], 1);
    assert_eq!(results["target"]["owner_slug"], owner.slug);
    assert!(results["reported"].is_null());
    assert!(results["recent_scraps"].as_array().unwrap().is_empty());
}
