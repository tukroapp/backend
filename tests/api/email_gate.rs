use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::helpers::{
    RegisteredUser, TestApp, drop_app, register_and_login, register_and_login_unverified, spawn_app,
};

async fn json_of(response: reqwest::Response) -> Value {
    response.json().await.expect("response not JSON")
}

/// Abre o mural e as mensagens do dono para que só a verificação de e-mail
/// possa barrar quem escreve.
async fn open_everything(app: &TestApp, owner: &RegisteredUser) {
    let response = app
        .api_client
        .put(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, owner.auth_header())
        .json(&json!({
            "name": "Dono",
            "scrap_write": "everyone",
            "testimonial_write": "everyone",
            "message_receive": "everyone",
        }))
        .send()
        .await
        .expect("Failed to open profile");
    assert_eq!(response.status(), StatusCode::OK);
}

async fn post(app: &TestApp, user: &RegisteredUser, path: &str, body: Value) -> reqwest::Response {
    app.api_client
        .post(format!("{}/api/v1{}", &app.address, path))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .json(&body)
        .send()
        .await
        .expect("Failed to send request")
}

#[tokio::test]
async fn writing_for_others_requires_a_verified_email() {
    // Arrange
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let newcomer = register_and_login_unverified(&app, "Recém-chegado").await;
    open_everything(&app, &owner).await;

    let community = json_of(
        post(
            &app,
            &owner,
            "/community",
            json!({ "name": "Comunidade", "is_private": false }),
        )
        .await,
    )
    .await["results"]["slug"]
        .as_str()
        .expect("slug not found")
        .to_string();

    let topic = json_of(
        post(
            &app,
            &owner,
            &format!("/community/{community}/topic"),
            json!({ "title": "Tópico", "body": "corpo" }),
        )
        .await,
    )
    .await["results"]["id"]
        .as_str()
        .expect("topic id not found")
        .to_string();

    // Act
    let blocked = [
        (
            "recado",
            post(
                &app,
                &newcomer,
                &format!("/profile/{}/scrap", owner.slug),
                json!({ "body": "oi" }),
            )
            .await,
        ),
        (
            "depoimento",
            post(
                &app,
                &newcomer,
                &format!("/profile/{}/testimonial", owner.slug),
                json!({ "body": "pessoa incrível" }),
            )
            .await,
        ),
        (
            "mensagem",
            post(
                &app,
                &newcomer,
                &format!("/profile/{}/message", owner.slug),
                json!({ "body": "oi" }),
            )
            .await,
        ),
        (
            "pedido de amizade",
            post(
                &app,
                &newcomer,
                &format!("/friend/request/{}", owner.slug),
                json!({}),
            )
            .await,
        ),
        (
            "fã",
            post(
                &app,
                &newcomer,
                &format!("/profile/{}/fan", owner.slug),
                json!({}),
            )
            .await,
        ),
        (
            "comunidade",
            post(
                &app,
                &newcomer,
                "/community",
                json!({ "name": "Outra", "is_private": false }),
            )
            .await,
        ),
        (
            "tópico",
            post(
                &app,
                &newcomer,
                &format!("/community/{community}/topic"),
                json!({ "title": "Spam", "body": "spam" }),
            )
            .await,
        ),
        (
            "comentário",
            post(
                &app,
                &newcomer,
                &format!("/topic/{topic}/comment"),
                json!({ "body": "spam" }),
            )
            .await,
        ),
    ];

    let rating = app
        .api_client
        .put(format!(
            "{}/api/v1/profile/{}/rating",
            &app.address, owner.slug
        ))
        .header(reqwest::header::AUTHORIZATION, newcomer.auth_header())
        .json(&json!({ "trusty": 3, "cool": 3, "sexy": 0 }))
        .send()
        .await
        .expect("Failed to rate");

    let body = json_of(
        post(
            &app,
            &newcomer,
            &format!("/profile/{}/scrap", owner.slug),
            json!({ "body": "oi" }),
        )
        .await,
    )
    .await;

    // Tear down
    drop_app(app).await;

    // Assert
    for (what, response) in blocked {
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{what} passou");
    }
    assert_eq!(rating.status(), StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["account"][0]["code"], "email_not_verified");
}

#[tokio::test]
async fn reading_and_the_own_profile_stay_free_before_verifying() {
    // Arrange
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let newcomer = register_and_login_unverified(&app, "Recém-chegado").await;

    let community = json_of(
        post(
            &app,
            &owner,
            "/community",
            json!({ "name": "Comunidade", "is_private": false }),
        )
        .await,
    )
    .await["results"]["slug"]
        .as_str()
        .expect("slug not found")
        .to_string();

    // Act
    let profile = app
        .api_client
        .get(format!("{}/api/v1/profile/{}", &app.address, owner.slug))
        .header(reqwest::header::AUTHORIZATION, newcomer.auth_header())
        .send()
        .await
        .expect("Failed to read profile");

    let edit = app
        .api_client
        .put(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, newcomer.auth_header())
        .json(&json!({ "name": "Nome novo", "bio": "cheguei agora" }))
        .send()
        .await
        .expect("Failed to edit profile");

    let join = post(
        &app,
        &newcomer,
        &format!("/community/{community}/member"),
        json!({}),
    )
    .await;

    let block = post(
        &app,
        &newcomer,
        &format!("/block/{}", owner.slug),
        json!({}),
    )
    .await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(profile.status(), StatusCode::OK);
    assert_eq!(edit.status(), StatusCode::OK);
    assert_eq!(join.status(), StatusCode::CREATED);
    assert_eq!(block.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn verifying_the_email_unlocks_the_writing() {
    // Arrange
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let newcomer = register_and_login_unverified(&app, "Recém-chegado").await;
    open_everything(&app, &owner).await;

    let before = post(
        &app,
        &newcomer,
        &format!("/profile/{}/scrap", owner.slug),
        json!({ "body": "oi" }),
    )
    .await;

    // Act
    sqlx::query!(
        r#"
        UPDATE auth_user SET is_email_verified = TRUE
        WHERE id = (SELECT user_id FROM user_profile WHERE slug = $1)
        "#,
        newcomer.slug,
    )
    .execute(&app.db_pool)
    .await
    .expect("Failed to verify email");

    let after = post(
        &app,
        &newcomer,
        &format!("/profile/{}/scrap", owner.slug),
        json!({ "body": "oi" }),
    )
    .await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(before.status(), StatusCode::FORBIDDEN);
    assert_eq!(after.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn registering_queues_the_verification_email() {
    // Arrange
    let app = spawn_app().await;
    let email = format!("{}@test.com", uuid::Uuid::new_v4());

    // Act
    let response = app
        .api_client
        .post(format!("{}/api/v1/auth/register", &app.address))
        .json(&json!({ "email": email, "password": "supersecret123", "name": "Novato" }))
        .send()
        .await
        .expect("Failed to register");

    let queued = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM email_queue WHERE $1 = ANY(to_address)"#,
        email,
    )
    .fetch_one(&app.db_pool)
    .await
    .expect("Failed to count queued emails");

    let token = sqlx::query_scalar!(
        r#"
        SELECT COUNT(*) AS "count!"
        FROM verification_token vt
        INNER JOIN auth_user au ON au.id = vt.user_id
        WHERE au.email = $1 AND vt.kind = 'email_verification'
        "#,
        email,
    )
    .fetch_one(&app.db_pool)
    .await
    .expect("Failed to count tokens");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(queued, 1, "o e-mail de confirmação não foi enfileirado");
    assert_eq!(token, 1);
}
