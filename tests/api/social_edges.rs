use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::friend::befriend;
use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};

async fn json_of(response: reqwest::Response) -> Value {
    response.json().await.expect("response not JSON")
}

async fn create_community(
    app: &TestApp,
    owner: &RegisteredUser,
    name: &str,
    is_private: bool,
) -> String {
    let response = app
        .api_client
        .post(format!("{}/api/v1/community", &app.address))
        .header(reqwest::header::AUTHORIZATION, owner.auth_header())
        .json(&json!({ "name": name, "category": "jogos", "is_private": is_private }))
        .send()
        .await
        .expect("Failed to create community");
    assert_eq!(response.status(), StatusCode::CREATED);

    json_of(response).await["results"]["slug"]
        .as_str()
        .expect("slug not found")
        .to_string()
}

async fn open_topic(app: &TestApp, user: &RegisteredUser, community: &str, title: &str) -> String {
    let response = app
        .api_client
        .post(format!(
            "{}/api/v1/community/{}/topic",
            &app.address, community
        ))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .json(&json!({ "title": title, "body": "corpo do tópico" }))
        .send()
        .await
        .expect("Failed to open topic");
    assert_eq!(response.status(), StatusCode::CREATED);

    json_of(response).await["results"]["id"]
        .as_str()
        .expect("topic id not found")
        .to_string()
}

async fn open_poll(
    app: &TestApp,
    user: &RegisteredUser,
    community: &str,
    question: &str,
) -> String {
    let response = app
        .api_client
        .post(format!(
            "{}/api/v1/community/{}/poll",
            &app.address, community
        ))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .json(&json!({
            "question": question,
            "min_choices": 1,
            "max_choices": 1,
            "options": ["A", "B"],
        }))
        .send()
        .await
        .expect("Failed to open poll");
    assert_eq!(response.status(), StatusCode::CREATED);

    json_of(response).await["results"]["id"]
        .as_str()
        .expect("poll id not found")
        .to_string()
}

async fn become_fan(app: &TestApp, fan: &RegisteredUser, idol: &str) {
    let response = app
        .api_client
        .post(format!("{}/api/v1/profile/{}/fan", &app.address, idol))
        .header(reqwest::header::AUTHORIZATION, fan.auth_header())
        .send()
        .await
        .expect("Failed to become fan");
    assert_eq!(response.status(), StatusCode::OK);
}

async fn feed_of(app: &TestApp, user: &RegisteredUser) -> Value {
    json_of(
        app.api_client
            .get(format!("{}/api/v1/me/feed", &app.address))
            .header(reqwest::header::AUTHORIZATION, user.auth_header())
            .send()
            .await
            .expect("Failed to read the feed"),
    )
    .await
}

async fn notifications_of(app: &TestApp, user: &RegisteredUser) -> Value {
    json_of(
        app.api_client
            .get(format!("{}/api/v1/notification", &app.address))
            .header(reqwest::header::AUTHORIZATION, user.auth_header())
            .send()
            .await
            .expect("Failed to read notifications"),
    )
    .await
}

#[tokio::test]
async fn topics_and_polls_show_up_in_the_feed_of_the_friends() {
    // Arrange
    let app = spawn_app().await;
    let autor = register_and_login(&app, "Autor").await;
    let amigo = register_and_login(&app, "Amigo").await;
    befriend(&app, &amigo, &autor).await;

    let community = create_community(&app, &autor, "Comunidade", false).await;

    // Act
    let topic_id = open_topic(&app, &autor, &community, "Melhor jogo de 2003").await;
    let poll_id = open_poll(&app, &autor, &community, "Qual é o melhor?").await;

    let feed = feed_of(&app, &amigo).await;
    let notifications = notifications_of(&app, &amigo).await;

    // Tear down
    drop_app(app).await;

    // Assert
    let items = feed["results"].as_array().unwrap();
    let poll_item = &items[0];
    let topic_item = &items[1];

    assert_eq!(poll_item["kind"], "poll_created");
    assert_eq!(poll_item["poll_id"], poll_id);
    assert_eq!(poll_item["text"], "Qual é o melhor?");
    assert_eq!(poll_item["community"]["slug"], community);

    assert_eq!(topic_item["kind"], "topic_created");
    assert_eq!(topic_item["topic_id"], topic_id);
    assert_eq!(topic_item["text"], "Melhor jogo de 2003");

    // Nada disso vira notificação: seria um insert por membro da comunidade.
    // A amizade aceita, essa sim, notifica.
    let kinds: Vec<&str> = notifications["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["kind"].as_str().unwrap())
        .collect();
    assert!(!kinds.contains(&"topic_created"));
    assert!(!kinds.contains(&"poll_created"));
}

#[tokio::test]
async fn a_private_community_does_not_leak_into_the_feed() {
    // Arrange
    let app = spawn_app().await;
    let autor = register_and_login(&app, "Autor").await;
    let amigo = register_and_login(&app, "Amigo").await;
    let membro = register_and_login(&app, "Membro").await;
    befriend(&app, &amigo, &autor).await;
    befriend(&app, &membro, &autor).await;

    let secreta = create_community(&app, &autor, "Comunidade Secreta", true).await;

    // O membro entra pela aprovação do dono
    let response = app
        .api_client
        .post(format!(
            "{}/api/v1/community/{}/member",
            &app.address, secreta
        ))
        .header(reqwest::header::AUTHORIZATION, membro.auth_header())
        .send()
        .await
        .expect("Failed to request to join");
    assert_eq!(response.status(), StatusCode::CREATED);

    let requests = json_of(
        app.api_client
            .get(format!(
                "{}/api/v1/community/{}/join-request",
                &app.address, secreta
            ))
            .header(reqwest::header::AUTHORIZATION, autor.auth_header())
            .send()
            .await
            .expect("Failed to list join requests"),
    )
    .await;
    let request_id = requests["results"][0]["id"].as_str().expect("request id");

    let response = app
        .api_client
        .post(format!(
            "{}/api/v1/community/{}/join-request/{}/accept",
            &app.address, secreta, request_id
        ))
        .header(reqwest::header::AUTHORIZATION, autor.auth_header())
        .send()
        .await
        .expect("Failed to accept");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // Act
    open_topic(&app, &autor, &secreta, "Assunto interno").await;

    let de_fora = feed_of(&app, &amigo).await;
    let de_dentro = feed_of(&app, &membro).await;

    // Tear down
    drop_app(app).await;

    // Assert
    let kinds_de_fora: Vec<&str> = de_fora["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["kind"].as_str().unwrap())
        .collect();
    assert!(
        !kinds_de_fora.contains(&"topic_created"),
        "o tópico da comunidade privada vazou no feed de quem não é membro"
    );

    let de_dentro_kinds: Vec<&str> = de_dentro["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["kind"].as_str().unwrap())
        .collect();
    assert!(de_dentro_kinds.contains(&"topic_created"));
}

#[tokio::test]
async fn many_fans_leave_a_single_unread_notification() {
    // Arrange
    let app = spawn_app().await;
    let idolo = register_and_login(&app, "Ídolo").await;
    let fa1 = register_and_login(&app, "Fã Um").await;
    let fa2 = register_and_login(&app, "Fã Dois").await;
    let fa3 = register_and_login(&app, "Fã Três").await;

    // Act
    become_fan(&app, &fa1, &idolo.slug).await;
    become_fan(&app, &fa2, &idolo.slug).await;
    become_fan(&app, &fa3, &idolo.slug).await;

    let notifications = notifications_of(&app, &idolo).await;

    // Depois de ler, um fã novo avisa de novo
    let response = app
        .api_client
        .post(format!("{}/api/v1/notification/read-all", &app.address))
        .header(reqwest::header::AUTHORIZATION, idolo.auth_header())
        .send()
        .await
        .expect("Failed to read all");
    assert_eq!(response.status(), StatusCode::OK);

    let fa4 = register_and_login(&app, "Fã Quatro").await;
    become_fan(&app, &fa4, &idolo.slug).await;

    let total = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM notification WHERE kind = 'new_fan'"#
    )
    .fetch_one(&app.db_pool)
    .await
    .expect("Failed to count notifications");

    // Tear down
    drop_app(app).await;

    // Assert
    let results = notifications["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "três fãs geraram mais de uma notificação");
    assert_eq!(results[0]["kind"], "new_fan");
    assert_eq!(total, 2, "a segunda leva só chega depois de ler a primeira");
}

#[tokio::test]
async fn the_unread_counters_stop_at_the_cap() {
    // Arrange
    let app = spawn_app().await;
    let user = register_and_login(&app, "Usuário").await;

    // 150 notificações direto no banco: pelo endpoint seriam 150 requisições
    sqlx::query!(
        r#"
        INSERT INTO notification (user_id, kind)
        SELECT (SELECT user_id FROM user_profile WHERE slug = $1), 'friend_accepted'
        FROM generate_series(1, 150)
        "#,
        user.slug,
    )
    .execute(&app.db_pool)
    .await
    .expect("Failed to insert notifications");

    // Act
    let unread = json_of(
        app.api_client
            .get(format!("{}/api/v1/me/unread", &app.address))
            .header(reqwest::header::AUTHORIZATION, user.auth_header())
            .send()
            .await
            .expect("Failed to read the counters"),
    )
    .await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(unread["results"]["notifications"], 100);
}

#[tokio::test]
async fn the_profile_lists_who_it_is_a_fan_of() {
    // Arrange
    let app = spawn_app().await;
    let fa = register_and_login(&app, "Fã").await;
    let idolo1 = register_and_login(&app, "Ídolo Um").await;
    let idolo2 = register_and_login(&app, "Ídolo Dois").await;
    let outro = register_and_login(&app, "Outro").await;

    become_fan(&app, &fa, &idolo1.slug).await;
    become_fan(&app, &fa, &idolo2.slug).await;
    become_fan(&app, &outro, &idolo1.slug).await;

    // Act
    let idols = json_of(
        app.api_client
            .get(format!("{}/api/v1/profile/{}/idol", &app.address, fa.slug))
            .send()
            .await
            .expect("Failed to list idols"),
    )
    .await;

    let fans = json_of(
        app.api_client
            .get(format!(
                "{}/api/v1/profile/{}/fan",
                &app.address, idolo1.slug
            ))
            .send()
            .await
            .expect("Failed to list fans"),
    )
    .await;

    let unknown = app
        .api_client
        .get(format!("{}/api/v1/profile/nao-existe/idol", &app.address))
        .send()
        .await
        .expect("Failed to list idols");

    // Tear down
    drop_app(app).await;

    // Assert
    let mut listed: Vec<String> = idols["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["slug"].as_str().unwrap().to_string())
        .collect();
    listed.sort();
    let mut expected = vec![idolo1.slug.clone(), idolo2.slug.clone()];
    expected.sort();
    assert_eq!(listed, expected);

    // O outro lado continua sendo quem é fã do perfil
    assert_eq!(fans["results"].as_array().unwrap().len(), 2);
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
}
