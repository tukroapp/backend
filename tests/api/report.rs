use reqwest::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};

async fn report(
    app: &TestApp,
    reporter: Option<&RegisteredUser>,
    slug: &str,
    payload: serde_json::Value,
) -> reqwest::Response {
    let mut request = app
        .api_client
        .post(format!("{}/api/v1/report/profile/{}", &app.address, slug))
        .json(&payload);

    if let Some(reporter) = reporter {
        request = request.header(reqwest::header::AUTHORIZATION, reporter.auth_header());
    }

    request.send().await.expect("Failed to report profile")
}

/// Quantas denúncias abertas existem contra `slug`, lido direto do banco porque
/// não há endpoint de leitura: a fila é do painel de admin, que ainda não existe.
async fn open_reports_against(app: &TestApp, slug: &str) -> i64 {
    sqlx::query_scalar!(
        r#"
        SELECT COUNT(*) AS "count!"
        FROM report r
        INNER JOIN user_profile up ON up.user_id = r.reported_user_id
        WHERE up.slug = $1 AND r.status = 'open'
        "#,
        slug,
    )
    .fetch_one(&app.db_pool)
    .await
    .expect("Failed to count reports")
}

#[tokio::test]
async fn reporting_a_profile_registers_the_report() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    // Act
    let response = report(
        &app,
        Some(&bob),
        &alice.slug,
        json!({ "reason": "harassment", "details": "  perseguindo no mural  " }),
    )
    .await;

    let status = response.status();
    let body: serde_json::Value = response.json().await.expect("report not JSON");

    let stored = sqlx::query!(
        r#"
        SELECT r.reason::TEXT AS "reason!", r.details, r.status::TEXT AS "status!"
        FROM report r
        INNER JOIN user_profile up ON up.user_id = r.reported_user_id
        WHERE up.slug = $1
        "#,
        alice.slug,
    )
    .fetch_one(&app.db_pool)
    .await
    .expect("report não foi gravado");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::CREATED);
    assert!(body["results"]["id"].is_string());
    assert!(body["results"]["created_at"].is_string());

    assert_eq!(stored.reason, "harassment");
    assert_eq!(stored.details.as_deref(), Some("perseguindo no mural"));
    // Nasce na fila do moderador
    assert_eq!(stored.status, "open");
}

#[tokio::test]
async fn the_same_profile_can_only_be_reported_once_by_the_same_user() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let carol = register_and_login(&app, "Carol").await;

    let first = report(&app, Some(&bob), &alice.slug, json!({ "reason": "spam" })).await;

    // Act
    let again = report(
        &app,
        Some(&bob),
        &alice.slug,
        json!({ "reason": "nudity", "details": "outro motivo" }),
    )
    .await;

    // Outra pessoa denunciando o mesmo perfil continua valendo
    let by_carol = report(&app, Some(&carol), &alice.slug, json!({ "reason": "spam" })).await;

    // E o bob pode denunciar outro perfil
    let other_target = report(&app, Some(&bob), &carol.slug, json!({ "reason": "spam" })).await;

    let against_alice = open_reports_against(&app, &alice.slug).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(first.status(), StatusCode::CREATED);
    assert_eq!(again.status(), StatusCode::CONFLICT);
    assert_eq!(by_carol.status(), StatusCode::CREATED);
    assert_eq!(other_target.status(), StatusCode::CREATED);
    assert_eq!(
        against_alice, 2,
        "a segunda denúncia do bob não pode entrar"
    );
}

#[tokio::test]
async fn reporting_yourself_returns_400() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;

    // Act
    let response = report(&app, Some(&alice), &alice.slug, json!({ "reason": "spam" })).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_unknown_reason_returns_400() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    // Act
    let unknown = report(
        &app,
        Some(&bob),
        &alice.slug,
        json!({ "reason": "porque sim" }),
    )
    .await;
    let missing = report(&app, Some(&bob), &alice.slug, json!({})).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
    assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn details_longer_than_the_limit_return_400() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    // Act
    let response = report(
        &app,
        Some(&bob),
        &alice.slug,
        json!({ "reason": "other", "details": "a".repeat(2001) }),
    )
    .await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_blocked_profile_can_still_be_reported() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    // Bob bloqueia a Alice e mesmo assim precisa poder denunciar
    let response = app
        .api_client
        .post(format!("{}/api/v1/block/{}", &app.address, alice.slug))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .send()
        .await
        .expect("Failed to block");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // Act
    let response = report(
        &app,
        Some(&bob),
        &alice.slug,
        json!({ "reason": "harassment" }),
    )
    .await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn reporting_an_unknown_profile_returns_404() {
    // Arrange
    let app = spawn_app().await;
    let bob = register_and_login(&app, "Bob").await;

    // Act
    let response = report(&app, Some(&bob), "nao-existe", json!({ "reason": "spam" })).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn reporting_requires_authentication() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;

    // Act
    let response = report(&app, None, &alice.slug, json!({ "reason": "spam" })).await;
    let stored = open_reports_against(&app, &alice.slug).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(stored, 0);
}

/// Denuncia um alvo que não é perfil: `/report/{kind}/{id}`.
async fn report_target(
    app: &TestApp,
    reporter: Option<&RegisteredUser>,
    kind: &str,
    id: &str,
    payload: serde_json::Value,
) -> reqwest::Response {
    let mut request = app
        .api_client
        .post(format!("{}/api/v1/report/{}/{}", &app.address, kind, id))
        .json(&payload);

    if let Some(reporter) = reporter {
        request = request.header(reqwest::header::AUTHORIZATION, reporter.auth_header());
    }

    request.send().await.expect("Failed to report target")
}

async fn json_of(response: reqwest::Response) -> serde_json::Value {
    response.json().await.expect("response not JSON")
}

async fn id_of(response: reqwest::Response) -> String {
    json_of(response).await["results"]["id"]
        .as_str()
        .expect("id not found")
        .to_string()
}

async fn create_community(app: &TestApp, user: &RegisteredUser, name: &str) -> String {
    let response = app
        .api_client
        .post(format!("{}/api/v1/community", &app.address))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .json(&json!({ "name": name, "is_private": false }))
        .send()
        .await
        .expect("Failed to create community");

    json_of(response).await["results"]["slug"]
        .as_str()
        .expect("slug not found")
        .to_string()
}

/// O mural nasce restrito a amigos, então o dono abre a escrita antes.
async fn write_scrap(
    app: &TestApp,
    author: &RegisteredUser,
    owner: &RegisteredUser,
    body: &str,
) -> String {
    let response = app
        .api_client
        .put(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, owner.auth_header())
        .json(&json!({ "name": "Dono do mural", "scrap_write": "everyone" }))
        .send()
        .await
        .expect("Failed to open wall");
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .api_client
        .post(format!(
            "{}/api/v1/profile/{}/scrap",
            &app.address, owner.slug
        ))
        .header(reqwest::header::AUTHORIZATION, author.auth_header())
        .json(&json!({ "body": body }))
        .send()
        .await
        .expect("Failed to write scrap");

    id_of(response).await
}

/// Depoimento só entra entre amigos por padrão, então o teste abre a escrita
/// para todo mundo antes de escrever.
async fn write_testimonial(
    app: &TestApp,
    author: &RegisteredUser,
    owner: &RegisteredUser,
) -> String {
    let response = app
        .api_client
        .put(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, owner.auth_header())
        .json(&json!({ "name": "Dono do perfil", "testimonial_write": "everyone" }))
        .send()
        .await
        .expect("Failed to open testimonials");
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .api_client
        .post(format!(
            "{}/api/v1/profile/{}/testimonial",
            &app.address, owner.slug
        ))
        .header(reqwest::header::AUTHORIZATION, author.auth_header())
        .json(&json!({ "body": "depoimento ofensivo" }))
        .send()
        .await
        .expect("Failed to write testimonial");

    id_of(response).await
}

async fn open_topic(app: &TestApp, user: &RegisteredUser, community: &str) -> String {
    let response = app
        .api_client
        .post(format!(
            "{}/api/v1/community/{}/topic",
            &app.address, community
        ))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .json(&json!({ "title": "Título do tópico", "body": "corpo do tópico" }))
        .send()
        .await
        .expect("Failed to open topic");

    id_of(response).await
}

async fn comment_on(app: &TestApp, user: &RegisteredUser, topic_id: &str) -> String {
    let response = app
        .api_client
        .post(format!(
            "{}/api/v1/topic/{}/comment",
            &app.address, topic_id
        ))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .json(&json!({ "body": "comentário ofensivo" }))
        .send()
        .await
        .expect("Failed to comment");

    id_of(response).await
}

async fn open_poll(app: &TestApp, user: &RegisteredUser, community: &str) -> String {
    let response = app
        .api_client
        .post(format!(
            "{}/api/v1/community/{}/poll",
            &app.address, community
        ))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .json(&json!({
            "question": "Pergunta da enquete",
            "min_choices": 1,
            "max_choices": 1,
            "options": ["A", "B"],
        }))
        .send()
        .await
        .expect("Failed to open poll");

    id_of(response).await
}

async fn stored_target(app: &TestApp, report_id: &str) -> (Option<Uuid>, Option<Uuid>) {
    let id = Uuid::parse_str(report_id).expect("report id inválido");

    let row = sqlx::query!(
        r#"
        SELECT scrap_id, testimonial_id, community_id, topic_id, comment_id, poll_id
        FROM report
        WHERE id = $1
        "#,
        id,
    )
    .fetch_one(&app.db_pool)
    .await
    .expect("report não foi gravado");

    (
        row.scrap_id
            .or(row.testimonial_id)
            .or(row.community_id)
            .or(row.topic_id)
            .or(row.comment_id)
            .or(row.poll_id),
        row.scrap_id,
    )
}

#[tokio::test]
async fn every_target_kind_can_be_reported() {
    // Arrange
    let app = spawn_app().await;
    let author = register_and_login(&app, "Autor").await;
    let reporter = register_and_login(&app, "Denunciante").await;

    let community = create_community(&app, &author, "Comunidade do Autor").await;
    let scrap_id = write_scrap(&app, &author, &reporter, "recado ofensivo").await;
    let testimonial_id = write_testimonial(&app, &author, &reporter).await;
    let topic_id = open_topic(&app, &author, &community).await;
    let comment_id = comment_on(&app, &author, &topic_id).await;
    let poll_id = open_poll(&app, &author, &community).await;

    // Act
    let targets = [
        ("scrap", scrap_id.clone()),
        ("testimonial", testimonial_id.clone()),
        ("community", community.clone()),
        ("topic", topic_id.clone()),
        ("comment", comment_id.clone()),
        ("poll", poll_id.clone()),
    ];

    let mut created = Vec::new();
    for (kind, id) in &targets {
        let response = report_target(
            &app,
            Some(&reporter),
            kind,
            id,
            json!({ "reason": "harassment", "details": "conteúdo abusivo" }),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "denúncia de {kind} falhou"
        );
        created.push((*kind, id_of(response).await));
    }

    let scrap_report = created[0].1.clone();
    let (target_id, stored_scrap_id) = stored_target(&app, &scrap_report).await;

    let total = sqlx::query_scalar!(r#"SELECT COUNT(*) AS "count!" FROM report"#)
        .fetch_one(&app.db_pool)
        .await
        .expect("Failed to count reports");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(total, 6);
    assert_eq!(stored_scrap_id, target_id);
    assert_eq!(target_id, Some(Uuid::parse_str(&scrap_id).unwrap()));
}

#[tokio::test]
async fn reporting_your_own_content_returns_400() {
    // Arrange
    let app = spawn_app().await;
    let author = register_and_login(&app, "Autor").await;
    let other = register_and_login(&app, "Outro").await;

    let community = create_community(&app, &author, "Comunidade do Autor").await;
    let scrap_id = write_scrap(&app, &author, &other, "recado próprio").await;
    let topic_id = open_topic(&app, &author, &community).await;
    let comment_id = comment_on(&app, &author, &topic_id).await;
    let poll_id = open_poll(&app, &author, &community).await;

    // Act
    let mut statuses = Vec::new();
    for (kind, id) in [
        ("scrap", &scrap_id),
        ("topic", &topic_id),
        ("comment", &comment_id),
        ("poll", &poll_id),
    ] {
        let response =
            report_target(&app, Some(&author), kind, id, json!({ "reason": "spam" })).await;
        statuses.push((kind, response.status()));
    }

    let total = sqlx::query_scalar!(r#"SELECT COUNT(*) AS "count!" FROM report"#)
        .fetch_one(&app.db_pool)
        .await
        .expect("Failed to count reports");

    // Tear down
    drop_app(app).await;

    // Assert
    for (kind, status) in statuses {
        assert_eq!(status, StatusCode::BAD_REQUEST, "{kind} deixou passar");
    }
    assert_eq!(total, 0);
}

#[tokio::test]
async fn an_unknown_target_returns_404() {
    // Arrange
    let app = spawn_app().await;
    let reporter = register_and_login(&app, "Denunciante").await;
    let missing = Uuid::now_v7().to_string();

    // Act
    let mut statuses = Vec::new();
    for kind in ["scrap", "testimonial", "topic", "comment", "poll"] {
        let response = report_target(
            &app,
            Some(&reporter),
            kind,
            &missing,
            json!({ "reason": "spam" }),
        )
        .await;
        statuses.push((kind, response.status()));
    }

    let community = report_target(
        &app,
        Some(&reporter),
        "community",
        "nao-existe",
        json!({ "reason": "spam" }),
    )
    .await;

    // Tear down
    drop_app(app).await;

    // Assert
    for (kind, status) in statuses {
        assert_eq!(status, StatusCode::NOT_FOUND, "{kind} não devolveu 404");
    }
    assert_eq!(community.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deleted_content_can_no_longer_be_reported() {
    // Arrange
    let app = spawn_app().await;
    let author = register_and_login(&app, "Autor").await;
    let reporter = register_and_login(&app, "Denunciante").await;

    let scrap_id = write_scrap(&app, &author, &reporter, "recado ofensivo").await;

    let response = app
        .api_client
        .delete(format!("{}/api/v1/scrap/{}", &app.address, scrap_id))
        .header(reqwest::header::AUTHORIZATION, author.auth_header())
        .send()
        .await
        .expect("Failed to delete scrap");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // Act
    let response = report_target(
        &app,
        Some(&reporter),
        "scrap",
        &scrap_id,
        json!({ "reason": "harassment" }),
    )
    .await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_same_content_can_be_reported_more_than_once() {
    // Arrange
    let app = spawn_app().await;
    let author = register_and_login(&app, "Autor").await;
    let reporter = register_and_login(&app, "Denunciante").await;

    let scrap_id = write_scrap(&app, &author, &reporter, "recado ofensivo").await;

    // Act
    let first = report_target(
        &app,
        Some(&reporter),
        "scrap",
        &scrap_id,
        json!({ "reason": "spam" }),
    )
    .await;
    let again = report_target(
        &app,
        Some(&reporter),
        "scrap",
        &scrap_id,
        json!({ "reason": "harassment" }),
    )
    .await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(first.status(), StatusCode::CREATED);
    // Só a denúncia de perfil tem trava por denunciante
    assert_eq!(again.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn reporting_content_requires_authentication() {
    // Arrange
    let app = spawn_app().await;
    let author = register_and_login(&app, "Autor").await;
    let other = register_and_login(&app, "Outro").await;
    let scrap_id = write_scrap(&app, &author, &other, "recado ofensivo").await;

    // Act
    let response = report_target(&app, None, "scrap", &scrap_id, json!({ "reason": "spam" })).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
