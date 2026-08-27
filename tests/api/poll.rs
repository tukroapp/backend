use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};

async fn json_of(response: reqwest::Response) -> Value {
    response.json().await.expect("response not JSON")
}

async fn create_community(app: &TestApp, user: &RegisteredUser, name: &str) -> String {
    let response = app
        .api_client
        .post(format!("{}/api/v1/community", app.address))
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

async fn create_private_community(app: &TestApp, user: &RegisteredUser, name: &str) -> String {
    let response = app
        .api_client
        .post(format!("{}/api/v1/community", app.address))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .json(&json!({ "name": name, "is_private": true }))
        .send()
        .await
        .expect("Failed to create community");

    json_of(response).await["results"]["slug"]
        .as_str()
        .expect("slug not found")
        .to_string()
}

async fn join(app: &TestApp, user: &RegisteredUser, slug: &str) {
    app.api_client
        .post(format!("{}/api/v1/community/{}/member", app.address, slug))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to join community");
}

async fn set_role(app: &TestApp, slug: &str, user: &RegisteredUser, role: &str) {
    sqlx::query(
        r#"
        UPDATE community_member
        SET role = $1::community_role
        WHERE community_id = (SELECT id FROM community WHERE slug = $2)
          AND user_id = (SELECT user_id FROM user_profile WHERE slug = $3)
        "#,
    )
    .bind(role)
    .bind(slug)
    .bind(&user.slug)
    .execute(&app.db_pool)
    .await
    .expect("Failed to set role");
}

async fn expire_poll(app: &TestApp, id: &str) {
    sqlx::query("UPDATE community_poll SET closes_at = NOW() - INTERVAL '1 hour' WHERE id = $1::uuid")
        .bind(id)
        .execute(&app.db_pool)
        .await
        .expect("Failed to expire poll");
}

async fn create_poll(
    app: &TestApp,
    user: Option<&RegisteredUser>,
    slug: &str,
    body: Value,
) -> reqwest::Response {
    let request = app
        .api_client
        .post(format!("{}/api/v1/community/{}/poll", app.address, slug))
        .json(&body);

    let request = match user {
        Some(user) => request.header(reqwest::header::AUTHORIZATION, user.auth_header()),
        None => request,
    };

    request.send().await.expect("Failed to create poll")
}

fn default_poll() -> Value {
    json!({
        "question": "Qual a melhor?",
        "min_choices": 1,
        "max_choices": 1,
        "options": ["Opção A", "Opção B", "Opção C"],
    })
}

/// Abre uma enquete padrão e devolve a resposta completa (`results`).
async fn open_poll(app: &TestApp, user: &RegisteredUser, slug: &str) -> Value {
    let response = create_poll(app, Some(user), slug, default_poll()).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    json_of(response).await["results"].clone()
}

async fn get_poll(app: &TestApp, id: &str, viewer: Option<&RegisteredUser>) -> reqwest::Response {
    let request = app
        .api_client
        .get(format!("{}/api/v1/poll/{}", app.address, id));

    let request = match viewer {
        Some(viewer) => request.header(reqwest::header::AUTHORIZATION, viewer.auth_header()),
        None => request,
    };

    request.send().await.expect("Failed to get poll")
}

async fn vote(
    app: &TestApp,
    user: &RegisteredUser,
    id: &str,
    option_ids: &[&str],
) -> reqwest::Response {
    app.api_client
        .post(format!("{}/api/v1/poll/{}/vote", app.address, id))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .json(&json!({ "option_ids": option_ids }))
        .send()
        .await
        .expect("Failed to vote")
}

fn option_id(poll: &Value, index: usize) -> String {
    poll["options"][index]["id"]
        .as_str()
        .expect("option id not found")
        .to_string()
}

#[tokio::test]
async fn owner_and_admin_create_polls_but_plain_members_and_outsiders_cannot() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let admin = register_and_login(&app, "Admin").await;
    let member = register_and_login(&app, "Membro").await;
    let outsider = register_and_login(&app, "Estranho").await;

    let slug = create_community(&app, &owner, "Comunidade").await;
    join(&app, &admin, &slug).await;
    set_role(&app, &slug, &admin, "admin").await;
    join(&app, &member, &slug).await;

    assert_eq!(
        create_poll(&app, Some(&owner), &slug, default_poll())
            .await
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        create_poll(&app, Some(&admin), &slug, default_poll())
            .await
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        create_poll(&app, Some(&member), &slug, default_poll())
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        create_poll(&app, Some(&outsider), &slug, default_poll())
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        create_poll(&app, None, &slug, default_poll()).await.status(),
        StatusCode::UNAUTHORIZED
    );

    drop_app(app).await;
}

#[tokio::test]
async fn create_poll_rejects_invalid_choice_bounds_and_option_lists() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let slug = create_community(&app, &owner, "Comunidade").await;

    let cases = [
        json!({ "question": "q", "min_choices": 2, "max_choices": 1, "options": ["a", "b"] }),
        json!({ "question": "q", "min_choices": 1, "max_choices": 3, "options": ["a", "b"] }),
        json!({ "question": "q", "min_choices": 0, "max_choices": 1, "options": ["a", "b"] }),
        json!({ "question": "q", "min_choices": 1, "max_choices": 1, "options": ["só uma"] }),
        json!({ "question": "q", "min_choices": 1, "max_choices": 1, "options": ["a", "  "] }),
        json!({ "question": "q", "min_choices": 1, "max_choices": 1, "options": ["dup", "dup"] }),
        json!({ "question": "   ", "min_choices": 1, "max_choices": 1, "options": ["a", "b"] }),
    ];

    for case in cases {
        assert_eq!(
            create_poll(&app, Some(&owner), &slug, case.clone())
                .await
                .status(),
            StatusCode::BAD_REQUEST,
            "esperava 400 para {case}"
        );
    }

    drop_app(app).await;
}

#[tokio::test]
async fn poll_closes_24h_after_creation() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let slug = create_community(&app, &owner, "Comunidade").await;

    let poll = open_poll(&app, &owner, &slug).await;

    let created = poll["created_at"]
        .as_str()
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .expect("created_at");
    let closes = poll["closes_at"]
        .as_str()
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .expect("closes_at");

    assert_eq!((closes - created).num_hours(), 24);
    assert_eq!(poll["is_closed"], json!(false));

    drop_app(app).await;
}

#[tokio::test]
async fn results_are_hidden_until_the_viewer_votes() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let voter = register_and_login(&app, "Votante").await;
    let slug = create_community(&app, &owner, "Comunidade").await;
    join(&app, &voter, &slug).await;

    let poll = open_poll(&app, &owner, &slug).await;
    let id = poll["id"].as_str().unwrap().to_string();

    let before = json_of(get_poll(&app, &id, Some(&voter)).await).await;
    assert_eq!(before["results"]["results_visible"], json!(false));
    assert_eq!(before["results"]["total_votes"], Value::Null);
    assert_eq!(before["results"]["options"][0]["votes"], Value::Null);

    let response = vote(&app, &voter, &id, &[&option_id(&poll, 0)]).await;
    assert_eq!(response.status(), StatusCode::OK);
    let after = json_of(response).await;
    assert_eq!(after["results"]["results_visible"], json!(true));
    assert_eq!(after["results"]["total_votes"], json!(1));
    assert_eq!(after["results"]["options"][0]["votes"], json!(1));
    assert_eq!(
        after["results"]["my_choices"],
        json!([option_id(&poll, 0)])
    );

    drop_app(app).await;
}

#[tokio::test]
async fn non_member_cannot_vote() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let outsider = register_and_login(&app, "Estranho").await;
    let slug = create_community(&app, &owner, "Comunidade").await;

    let poll = open_poll(&app, &owner, &slug).await;
    let id = poll["id"].as_str().unwrap().to_string();

    let response = vote(&app, &outsider, &id, &[&option_id(&poll, 0)]).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    drop_app(app).await;
}

#[tokio::test]
async fn vote_enforces_choice_count_and_option_ownership() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let other_owner = register_and_login(&app, "Outro").await;
    let voter = register_and_login(&app, "Votante").await;

    let slug = create_community(&app, &owner, "Comunidade").await;
    join(&app, &voter, &slug).await;
    let other_slug = create_community(&app, &other_owner, "Outra").await;

    let poll = open_poll(&app, &owner, &slug).await;
    let id = poll["id"].as_str().unwrap().to_string();
    let foreign_poll = open_poll(&app, &other_owner, &other_slug).await;

    // max_choices é 1, então dois ids passam do limite.
    let too_many = vote(
        &app,
        &voter,
        &id,
        &[&option_id(&poll, 0), &option_id(&poll, 1)],
    )
    .await;
    assert_eq!(too_many.status(), StatusCode::BAD_REQUEST);

    let none = vote(&app, &voter, &id, &[]).await;
    assert_eq!(none.status(), StatusCode::BAD_REQUEST);

    let foreign = vote(&app, &voter, &id, &[&option_id(&foreign_poll, 0)]).await;
    assert_eq!(foreign.status(), StatusCode::BAD_REQUEST);

    drop_app(app).await;
}

#[tokio::test]
async fn multi_choice_poll_accepts_a_range_of_options() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let voter = register_and_login(&app, "Votante").await;
    let slug = create_community(&app, &owner, "Comunidade").await;
    join(&app, &voter, &slug).await;

    let poll = json_of(
        create_poll(
            &app,
            Some(&owner),
            &slug,
            json!({
                "question": "Marque uma ou duas",
                "min_choices": 1,
                "max_choices": 2,
                "options": ["A", "B", "C"],
            }),
        )
        .await,
    )
    .await["results"]
        .clone();
    let id = poll["id"].as_str().unwrap().to_string();

    let response = vote(
        &app,
        &voter,
        &id,
        &[&option_id(&poll, 0), &option_id(&poll, 1)],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_of(response).await;
    assert_eq!(body["results"]["total_votes"], json!(1));
    assert_eq!(body["results"]["my_choices"].as_array().unwrap().len(), 2);

    drop_app(app).await;
}

#[tokio::test]
async fn a_second_vote_is_rejected_and_the_first_stays() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let voter = register_and_login(&app, "Votante").await;
    let slug = create_community(&app, &owner, "Comunidade").await;
    join(&app, &voter, &slug).await;

    let poll = open_poll(&app, &owner, &slug).await;
    let id = poll["id"].as_str().unwrap().to_string();

    let first = json_of(vote(&app, &voter, &id, &[&option_id(&poll, 0)]).await).await;
    assert_eq!(first["results"]["can_vote"], json!(false));

    let second = vote(&app, &voter, &id, &[&option_id(&poll, 1)]).await;
    assert_eq!(second.status(), StatusCode::CONFLICT);

    let after = json_of(get_poll(&app, &id, Some(&voter)).await).await;
    assert_eq!(after["results"]["total_votes"], json!(1));
    assert_eq!(after["results"]["options"][0]["votes"], json!(1));
    assert_eq!(after["results"]["options"][1]["votes"], json!(0));

    drop_app(app).await;
}

#[tokio::test]
async fn editing_the_question_marks_the_poll_as_edited() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let slug = create_community(&app, &owner, "Comunidade").await;

    let poll = open_poll(&app, &owner, &slug).await;
    let id = poll["id"].as_str().unwrap().to_string();
    assert_eq!(poll["edited_at"], Value::Null);

    let edited = app
        .api_client
        .put(format!("{}/api/v1/poll/{}", app.address, id))
        .header(reqwest::header::AUTHORIZATION, owner.auth_header())
        .json(&json!({
            "question": "Pergunta reescrita",
            "min_choices": 1,
            "max_choices": 1,
            "options": ["Opção A", "Opção B", "Opção C"],
        }))
        .send()
        .await
        .expect("edit");
    assert_eq!(edited.status(), StatusCode::OK);
    assert!(json_of(edited).await["results"]["edited_at"].is_string());

    drop_app(app).await;
}

#[tokio::test]
async fn closing_a_poll_makes_results_public_and_blocks_voting() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let voter = register_and_login(&app, "Votante").await;
    let latecomer = register_and_login(&app, "Atrasado").await;
    let slug = create_community(&app, &owner, "Comunidade").await;
    join(&app, &voter, &slug).await;
    join(&app, &latecomer, &slug).await;

    let poll = open_poll(&app, &owner, &slug).await;
    let id = poll["id"].as_str().unwrap().to_string();
    vote(&app, &voter, &id, &[&option_id(&poll, 0)]).await;

    let close = app
        .api_client
        .post(format!("{}/api/v1/poll/{}/close", app.address, id))
        .header(reqwest::header::AUTHORIZATION, owner.auth_header())
        .send()
        .await
        .expect("Failed to close poll");
    assert_eq!(close.status(), StatusCode::OK);
    assert_eq!(json_of(close).await["results"]["is_closed"], json!(true));

    // Não-votante agora vê os resultados anônimos.
    let seen = json_of(get_poll(&app, &id, Some(&latecomer)).await).await;
    assert_eq!(seen["results"]["results_visible"], json!(true));
    assert_eq!(seen["results"]["total_votes"], json!(1));

    let blocked = vote(&app, &latecomer, &id, &[&option_id(&poll, 0)]).await;
    assert_eq!(blocked.status(), StatusCode::FORBIDDEN);

    // Encerrar de novo não é permitido.
    let again = app
        .api_client
        .post(format!("{}/api/v1/poll/{}/close", app.address, id))
        .header(reqwest::header::AUTHORIZATION, owner.auth_header())
        .send()
        .await
        .expect("Failed to close poll");
    assert_eq!(again.status(), StatusCode::FORBIDDEN);

    drop_app(app).await;
}

#[tokio::test]
async fn expired_poll_shows_public_results_and_rejects_votes() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let voter = register_and_login(&app, "Votante").await;
    let slug = create_community(&app, &owner, "Comunidade").await;
    join(&app, &voter, &slug).await;

    let poll = open_poll(&app, &owner, &slug).await;
    let id = poll["id"].as_str().unwrap().to_string();
    expire_poll(&app, &id).await;

    let seen = json_of(get_poll(&app, &id, Some(&voter)).await).await;
    assert_eq!(seen["results"]["is_closed"], json!(true));
    assert_eq!(seen["results"]["results_visible"], json!(true));

    let blocked = vote(&app, &voter, &id, &[&option_id(&poll, 0)]).await;
    assert_eq!(blocked.status(), StatusCode::FORBIDDEN);

    drop_app(app).await;
}

#[tokio::test]
async fn options_are_editable_only_before_the_first_vote() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let voter = register_and_login(&app, "Votante").await;
    let slug = create_community(&app, &owner, "Comunidade").await;
    join(&app, &voter, &slug).await;

    let poll = open_poll(&app, &owner, &slug).await;
    let id = poll["id"].as_str().unwrap().to_string();

    let edit = |body: Value| {
        app.api_client
            .put(format!("{}/api/v1/poll/{}", app.address, id))
            .header(reqwest::header::AUTHORIZATION, owner.auth_header())
            .json(&body)
    };

    let ok = edit(json!({
        "question": "Nova pergunta",
        "min_choices": 1,
        "max_choices": 1,
        "options": ["X", "Y"],
    }))
    .send()
    .await
    .expect("edit");
    assert_eq!(ok.status(), StatusCode::OK);

    let poll = json_of(get_poll(&app, &id, Some(&owner)).await).await["results"].clone();
    vote(&app, &voter, &id, &[&option_id(&poll, 0)]).await;

    let rejected = edit(json!({
        "question": "Outra pergunta",
        "min_choices": 1,
        "max_choices": 1,
        "options": ["Z", "W"],
    }))
    .send()
    .await
    .expect("edit");
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);

    // Só a pergunta, mantendo as opções, ainda passa.
    let question_only = edit(json!({
        "question": "Pergunta ajustada",
        "min_choices": 1,
        "max_choices": 1,
        "options": ["X", "Y"],
    }))
    .send()
    .await
    .expect("edit");
    assert_eq!(question_only.status(), StatusCode::OK);

    drop_app(app).await;
}

#[tokio::test]
async fn author_and_moderator_delete_poll_but_others_cannot() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let admin = register_and_login(&app, "Admin").await;
    let stranger = register_and_login(&app, "Estranho").await;

    let slug = create_community(&app, &owner, "Comunidade").await;
    join(&app, &admin, &slug).await;
    set_role(&app, &slug, &admin, "admin").await;

    let poll = json_of(create_poll(&app, Some(&admin), &slug, default_poll()).await).await
        ["results"]
        .clone();
    let id = poll["id"].as_str().unwrap().to_string();

    let by_stranger = app
        .api_client
        .delete(format!("{}/api/v1/poll/{}", app.address, id))
        .header(reqwest::header::AUTHORIZATION, stranger.auth_header())
        .send()
        .await
        .expect("delete");
    assert_eq!(by_stranger.status(), StatusCode::FORBIDDEN);

    let by_owner = app
        .api_client
        .delete(format!("{}/api/v1/poll/{}", app.address, id))
        .header(reqwest::header::AUTHORIZATION, owner.auth_header())
        .send()
        .await
        .expect("delete");
    assert_eq!(by_owner.status(), StatusCode::NO_CONTENT);

    assert_eq!(
        get_poll(&app, &id, Some(&owner)).await.status(),
        StatusCode::NOT_FOUND
    );

    drop_app(app).await;
}

#[tokio::test]
async fn private_community_hides_polls_from_outsiders() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let outsider = register_and_login(&app, "Estranho").await;
    let slug = create_private_community(&app, &owner, "Fechada").await;

    let poll = open_poll(&app, &owner, &slug).await;
    let id = poll["id"].as_str().unwrap().to_string();

    assert_eq!(
        get_poll(&app, &id, Some(&outsider)).await.status(),
        StatusCode::NOT_FOUND
    );

    let list = app
        .api_client
        .get(format!("{}/api/v1/community/{}/poll", app.address, slug))
        .header(reqwest::header::AUTHORIZATION, outsider.auth_header())
        .send()
        .await
        .expect("list");
    assert_eq!(list.status(), StatusCode::NOT_FOUND);

    drop_app(app).await;
}

#[tokio::test]
async fn poll_listing_returns_community_polls_newest_first() {
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let slug = create_community(&app, &owner, "Comunidade").await;

    let first = open_poll(&app, &owner, &slug).await;
    let second = open_poll(&app, &owner, &slug).await;

    let list = json_of(
        app.api_client
            .get(format!("{}/api/v1/community/{}/poll", app.address, slug))
            .send()
            .await
            .expect("list"),
    )
    .await;

    let results = list["results"].as_array().expect("results array");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["id"], second["id"]);
    assert_eq!(results[1]["id"], first["id"]);

    drop_app(app).await;
}
