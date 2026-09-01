use reqwest::StatusCode;
use serde_json::{Value, json};

use tukro_backend::services::cache;

use crate::friend::befriend;
use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};

async fn json_of(response: reqwest::Response) -> Value {
    response.json().await.expect("response not JSON")
}

async fn create(
    app: &TestApp,
    owner: &RegisteredUser,
    name: &str,
    category: &str,
    is_private: bool,
) -> String {
    let response = app
        .api_client
        .post(format!("{}/api/v1/community", &app.address))
        .header(reqwest::header::AUTHORIZATION, owner.auth_header())
        .json(&json!({ "name": name, "category": category, "is_private": is_private }))
        .send()
        .await
        .expect("Failed to create community");
    assert_eq!(response.status(), StatusCode::CREATED);

    json_of(response).await["results"]["slug"]
        .as_str()
        .expect("slug not found")
        .to_string()
}

async fn join(app: &TestApp, user: &RegisteredUser, slug: &str) {
    let response = app
        .api_client
        .post(format!("{}/api/v1/community/{}/member", &app.address, slug))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to join community");
    assert_eq!(response.status(), StatusCode::CREATED);
}

async fn list(app: &TestApp, query: &str) -> Value {
    json_of(
        app.api_client
            .get(format!("{}/api/v1/community?{}", &app.address, query))
            .send()
            .await
            .expect("Failed to list communities"),
    )
    .await
}

fn names_of(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("esperava uma lista")
        .iter()
        .map(|item| item["name"].as_str().expect("name").to_string())
        .collect()
}

#[tokio::test]
async fn the_index_filters_by_category() {
    // Arrange
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;

    create(
        &app,
        &owner,
        "Eu odeio acordar cedo",
        "estilo_de_vida",
        false,
    )
    .await;
    create(&app, &owner, "Rock dos anos 80", "musica", false).await;
    create(&app, &owner, "Forró de verdade", "musica", false).await;

    // Act
    let musica = list(&app, "category=musica").await;
    let jogos = list(&app, "category=jogos").await;
    let all = list(&app, "").await;
    let invalid = app
        .api_client
        .get(format!("{}/api/v1/community?category=banana", &app.address))
        .send()
        .await
        .expect("Failed to list communities");

    // Tear down
    drop_app(app).await;

    // Assert
    let mut found = names_of(&musica["results"]);
    found.sort();
    assert_eq!(found, vec!["Forró de verdade", "Rock dos anos 80"]);
    assert_eq!(musica["results"][0]["category"], "musica");

    assert!(jogos["results"].as_array().unwrap().is_empty());
    assert_eq!(all["results"].as_array().unwrap().len(), 3);
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_index_sorts_both_ways_by_date_and_by_members() {
    // Arrange
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let ana = register_and_login(&app, "Ana").await;
    let bruno = register_and_login(&app, "Bruno").await;

    let primeira = create(&app, &owner, "Primeira", "jogos", false).await;
    create(&app, &owner, "Segunda", "jogos", false).await;
    let terceira = create(&app, &owner, "Terceira", "jogos", false).await;

    // A primeira fica com 3 membros, a terceira com 2, a segunda só com o dono
    join(&app, &ana, &primeira).await;
    join(&app, &bruno, &primeira).await;
    join(&app, &ana, &terceira).await;

    // Act
    let newest = list(&app, "sort=created_at_desc").await;
    let oldest = list(&app, "sort=created_at_asc").await;
    let biggest = list(&app, "sort=members_desc").await;
    let smallest = list(&app, "sort=members_asc").await;
    let invalid = app
        .api_client
        .get(format!("{}/api/v1/community?sort=aleatorio", &app.address))
        .send()
        .await
        .expect("Failed to list communities");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(
        names_of(&newest["results"]),
        vec!["Terceira", "Segunda", "Primeira"]
    );
    assert_eq!(
        names_of(&oldest["results"]),
        vec!["Primeira", "Segunda", "Terceira"]
    );
    assert_eq!(
        names_of(&biggest["results"]),
        vec!["Primeira", "Terceira", "Segunda"]
    );
    assert_eq!(
        names_of(&smallest["results"]),
        vec!["Segunda", "Terceira", "Primeira"]
    );
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_cursor_walks_every_page_in_each_order() {
    // Arrange
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let ana = register_and_login(&app, "Ana").await;

    let uma = create(&app, &owner, "Uma", "jogos", false).await;
    create(&app, &owner, "Duas", "jogos", false).await;
    create(&app, &owner, "Tres", "jogos", false).await;
    join(&app, &ana, &uma).await;

    // Act
    let mut walked = Vec::new();
    let mut cursor: Option<String> = None;

    loop {
        let query = match &cursor {
            Some(cursor) => format!("sort=members_desc&limit=1&cursor={cursor}"),
            None => "sort=members_desc&limit=1".to_string(),
        };
        let page = list(&app, &query).await;
        walked.extend(names_of(&page["results"]));

        match page["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_string()),
            None => break,
        }
    }

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(walked.len(), 3, "a paginação repetiu ou pulou linha");
    assert_eq!(walked[0], "Uma");
    let mut rest = walked[1..].to_vec();
    rest.sort();
    assert_eq!(rest, vec!["Duas", "Tres"]);
}

#[tokio::test]
async fn the_category_index_counts_the_communities() {
    // Arrange
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;

    create(&app, &owner, "Rock", "musica", false).await;
    create(&app, &owner, "Samba", "musica", false).await;
    create(&app, &owner, "RPG de mesa", "jogos", false).await;

    // O índice é cacheado no Redis por uma chave global, que não é isolada por
    // banco como o resto do teste: limpa antes de ler para não pegar a contagem
    // de outra execução.
    deadpool_redis::redis::cmd("DEL")
        .arg(cache::COMMUNITY_CATEGORY_COUNTS)
        .query_async::<()>(&mut app.redis_pool.get().await.unwrap())
        .await
        .expect("Failed to clear the category cache");

    // Act
    let index = json_of(
        app.api_client
            .get(format!("{}/api/v1/community/category", &app.address))
            .send()
            .await
            .expect("Failed to read the category index"),
    )
    .await;

    // Tear down
    drop_app(app).await;

    // Assert
    let results = index["results"].as_array().unwrap();
    // Só as categorias com comunidade aparecem, da maior para a menor
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["category"], "musica");
    assert_eq!(results[0]["count"], 2);
    assert_eq!(results[1]["category"], "jogos");
    assert_eq!(results[1]["count"], 1);
}

#[tokio::test]
async fn the_communities_of_my_friends_leave_out_mine_and_the_private_ones() {
    // Arrange
    let app = spawn_app().await;
    let me = register_and_login(&app, "Eu").await;
    let ana = register_and_login(&app, "Ana").await;
    let bruno = register_and_login(&app, "Bruno").await;
    let estranho = register_and_login(&app, "Estranho").await;

    befriend(&app, &me, &ana).await;
    befriend(&app, &me, &bruno).await;

    // Os dois amigos estão nessa, e eu não
    let popular = create(&app, &ana, "Popular", "pessoas", false).await;
    join(&app, &bruno, &popular).await;

    // Só um amigo
    create(&app, &bruno, "Discreta", "pessoas", false).await;

    // Eu já estou nessa, então não é sugestão
    let minha = create(&app, &ana, "Já estou", "pessoas", false).await;
    join(&app, &me, &minha).await;

    // Privada não se descobre por fora
    create(&app, &ana, "Secreta", "pessoas", true).await;

    // De quem não é meu amigo
    create(&app, &estranho, "De estranho", "pessoas", false).await;

    // Act
    let suggestions = json_of(
        app.api_client
            .get(format!("{}/api/v1/me/community/friend", &app.address))
            .header(reqwest::header::AUTHORIZATION, me.auth_header())
            .send()
            .await
            .expect("Failed to read suggestions"),
    )
    .await;

    let anonymous = app
        .api_client
        .get(format!("{}/api/v1/me/community/friend", &app.address))
        .send()
        .await
        .expect("Failed to read suggestions");

    // Tear down
    drop_app(app).await;

    // Assert
    let results = suggestions["results"].as_array().unwrap();
    assert_eq!(
        names_of(&suggestions["results"]),
        vec!["Popular", "Discreta"]
    );
    // A mais frequentada pelos amigos vem primeiro, com a contagem
    assert_eq!(results[0]["friends_count"], 2);
    assert_eq!(results[1]["friends_count"], 1);
    assert_eq!(results[0]["members_count"], 2);

    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_profile_shows_the_communities_in_common() {
    // Arrange
    let app = spawn_app().await;
    let viewer = register_and_login(&app, "Visitante").await;
    let target = register_and_login(&app, "Alvo").await;

    let comum = create(&app, &target, "Comum", "pessoas", false).await;
    let so_do_alvo = create(&app, &target, "Só do alvo", "pessoas", false).await;
    join(&app, &viewer, &comum).await;

    // Act
    let profile = json_of(
        app.api_client
            .get(format!("{}/api/v1/profile/{}", &app.address, target.slug))
            .header(reqwest::header::AUTHORIZATION, viewer.auth_header())
            .send()
            .await
            .expect("Failed to read profile"),
    )
    .await;

    let anonymous = json_of(
        app.api_client
            .get(format!("{}/api/v1/profile/{}", &app.address, target.slug))
            .send()
            .await
            .expect("Failed to read profile"),
    )
    .await;

    let mutual_list = json_of(
        app.api_client
            .get(format!(
                "{}/api/v1/profile/{}/community?mutual=true",
                &app.address, target.slug
            ))
            .header(reqwest::header::AUTHORIZATION, viewer.auth_header())
            .send()
            .await
            .expect("Failed to list communities"),
    )
    .await;

    let full_list = json_of(
        app.api_client
            .get(format!(
                "{}/api/v1/profile/{}/community",
                &app.address, target.slug
            ))
            .header(reqwest::header::AUTHORIZATION, viewer.auth_header())
            .send()
            .await
            .expect("Failed to list communities"),
    )
    .await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(profile["results"]["mutual_communities_count"], 1);
    assert!(anonymous["results"]["mutual_communities_count"].is_null());
    assert_eq!(names_of(&mutual_list["results"]), vec!["Comum"]);
    assert_eq!(full_list["results"].as_array().unwrap().len(), 2);
    assert_ne!(so_do_alvo, comum);
}
