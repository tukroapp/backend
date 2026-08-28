use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::friend::befriend;
use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};

fn png_bytes() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0xDA, 0x63, 0xFC,
        0xCF, 0xC0, 0x50, 0x0F, 0x00, 0x04, 0x85, 0x01, 0x80, 0x84, 0xA9, 0x8C, 0x21, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ]
}

async fn json_of(response: reqwest::Response) -> Value {
    response.json().await.expect("response not JSON")
}

async fn create_album(
    app: &TestApp,
    owner: &RegisteredUser,
    title: &str,
    visibility: &str,
) -> String {
    let response = app
        .api_client
        .post(format!("{}/api/v1/me/album", &app.address))
        .header(reqwest::header::AUTHORIZATION, owner.auth_header())
        .json(&json!({ "title": title, "visibility": visibility }))
        .send()
        .await
        .expect("Failed to create album");
    assert_eq!(response.status(), StatusCode::CREATED);

    json_of(response).await["results"]["id"]
        .as_str()
        .expect("album id not found")
        .to_string()
}

/// Sobe uma foto, opcionalmente já dentro de um álbum e com legenda.
async fn upload(
    app: &TestApp,
    user: &RegisteredUser,
    album_id: Option<&str>,
    caption: Option<&str>,
) -> reqwest::Response {
    let part = reqwest::multipart::Part::bytes(png_bytes()).file_name("foto.png".to_string());
    let mut form = reqwest::multipart::Form::new().part("file", part);

    if let Some(album_id) = album_id {
        form = form.text("album_id", album_id.to_string());
    }
    if let Some(caption) = caption {
        form = form.text("caption", caption.to_string());
    }

    app.api_client
        .post(format!("{}/api/v1/me/media", &app.address))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .multipart(form)
        .send()
        .await
        .expect("Failed to upload media")
}

async fn upload_id(app: &TestApp, user: &RegisteredUser, album_id: &str, caption: &str) -> String {
    let response = upload(app, user, Some(album_id), Some(caption)).await;
    assert_eq!(response.status(), StatusCode::CREATED);

    json_of(response).await["results"]["id"]
        .as_str()
        .expect("media id not found")
        .to_string()
}

async fn album_detail(
    app: &TestApp,
    id: &str,
    viewer: Option<&RegisteredUser>,
) -> reqwest::Response {
    let mut request = app
        .api_client
        .get(format!("{}/api/v1/album/{}", &app.address, id));

    if let Some(viewer) = viewer {
        request = request.header(reqwest::header::AUTHORIZATION, viewer.auth_header());
    }

    request.send().await.expect("Failed to read the album")
}

async fn album_list(app: &TestApp, slug: &str, viewer: Option<&RegisteredUser>) -> Value {
    let mut request = app
        .api_client
        .get(format!("{}/api/v1/profile/{}/album", &app.address, slug));

    if let Some(viewer) = viewer {
        request = request.header(reqwest::header::AUTHORIZATION, viewer.auth_header());
    }

    json_of(request.send().await.expect("Failed to list albums")).await
}

async fn comment(
    app: &TestApp,
    user: Option<&RegisteredUser>,
    media_id: &str,
    body: &str,
) -> reqwest::Response {
    let mut request = app
        .api_client
        .post(format!(
            "{}/api/v1/media/{}/comment",
            &app.address, media_id
        ))
        .json(&json!({ "body": body }));

    if let Some(user) = user {
        request = request.header(reqwest::header::AUTHORIZATION, user.auth_header());
    }

    request.send().await.expect("Failed to comment")
}

async fn comments_of(app: &TestApp, media_id: &str, viewer: Option<&RegisteredUser>) -> Value {
    let mut request = app.api_client.get(format!(
        "{}/api/v1/media/{}/comment",
        &app.address, media_id
    ));

    if let Some(viewer) = viewer {
        request = request.header(reqwest::header::AUTHORIZATION, viewer.auth_header());
    }

    json_of(request.send().await.expect("Failed to read comments")).await
}

#[tokio::test]
async fn an_album_holds_photos_with_caption_cover_and_order() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let album = create_album(&app, &alice, "Viagem", "everyone").await;

    // Act
    let first = upload_id(&app, &alice, &album, "chegando").await;
    let second = upload_id(&app, &alice, &album, "na praia").await;

    // A segunda foto vira a capa e vai para a frente
    let cover = app
        .api_client
        .put(format!("{}/api/v1/me/album/{}", &app.address, album))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .json(&json!({ "title": "Viagem de 2003", "cover_media_id": second }))
        .send()
        .await
        .expect("Failed to edit album");
    assert_eq!(cover.status(), StatusCode::OK);

    let ordered = app
        .api_client
        .put(format!("{}/api/v1/me/media/{}", &app.address, second))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .json(&json!({ "caption": "na praia", "position": -1 }))
        .send()
        .await
        .expect("Failed to edit media");
    assert_eq!(ordered.status(), StatusCode::NO_CONTENT);

    let detail = json_of(album_detail(&app, &album, None).await).await;

    // Tear down
    drop_app(app).await;

    // Assert
    let results = &detail["results"];
    assert_eq!(results["title"], "Viagem de 2003");
    assert_eq!(results["photos_count"], 2);
    assert!(
        results["cover_url"].as_str().unwrap().ends_with(".png"),
        "a capa precisa virar URL pública"
    );
    assert_eq!(results["owner_slug"], alice.slug);

    let photos = results["photos"].as_array().unwrap();
    assert_eq!(photos.len(), 2);
    // A posição -1 põe a segunda foto na frente
    assert_eq!(photos[0]["id"], second);
    assert_eq!(photos[0]["caption"], "na praia");
    assert_eq!(photos[1]["id"], first);
    assert_eq!(photos[1]["caption"], "chegando");
    assert_eq!(photos[0]["album_id"], album);
}

#[tokio::test]
async fn a_friends_only_album_hides_from_the_rest() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let amiga = register_and_login(&app, "Amiga").await;
    let estranho = register_and_login(&app, "Estranho").await;
    befriend(&app, &alice, &amiga).await;

    let publico = create_album(&app, &alice, "Público", "everyone").await;
    let restrito = create_album(&app, &alice, "Só amigos", "friends").await;
    let media_id = upload_id(&app, &alice, &restrito, "segredo").await;

    // Act
    let para_amiga = album_list(&app, &alice.slug, Some(&amiga)).await;
    let para_estranho = album_list(&app, &alice.slug, Some(&estranho)).await;
    let anonimo = album_list(&app, &alice.slug, None).await;

    let detalhe_estranho = album_detail(&app, &restrito, Some(&estranho)).await;
    let detalhe_amiga = album_detail(&app, &restrito, Some(&amiga)).await;

    // Nem comentar, nem ler comentário de foto que não alcança
    let comentario_estranho = comment(&app, Some(&estranho), &media_id, "oi").await;
    let comentario_amiga = comment(&app, Some(&amiga), &media_id, "que legal").await;

    // Tear down
    drop_app(app).await;

    // Assert
    let titulos = |value: &Value| -> Vec<String> {
        value["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["title"].as_str().unwrap().to_string())
            .collect()
    };

    assert_eq!(titulos(&para_amiga).len(), 2);
    assert_eq!(titulos(&para_estranho), vec!["Público"]);
    assert_eq!(titulos(&anonimo), vec!["Público"]);
    assert!(!publico.is_empty());

    // Álbum escondido responde 404, não 403
    assert_eq!(detalhe_estranho.status(), StatusCode::NOT_FOUND);
    assert_eq!(detalhe_amiga.status(), StatusCode::OK);

    assert_eq!(comentario_estranho.status(), StatusCode::NOT_FOUND);
    assert_eq!(comentario_amiga.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn comments_notify_the_owner_and_can_be_removed_by_both_sides() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let album = create_album(&app, &alice, "Fotos", "everyone").await;
    let media_id = upload_id(&app, &alice, &album, "eu").await;

    // Act
    let created = comment(&app, Some(&bob), &media_id, "que foto boa").await;
    let created_status = created.status();
    let comment_id = json_of(created).await["results"]["id"]
        .as_str()
        .expect("comment id")
        .to_string();

    let outro = comment(&app, Some(&bob), &media_id, "sério mesmo").await;
    let outro_id = json_of(outro).await["results"]["id"]
        .as_str()
        .expect("comment id")
        .to_string();

    let anonimo = comment(&app, None, &media_id, "sem sessão").await;

    let notifications = json_of(
        app.api_client
            .get(format!("{}/api/v1/notification", &app.address))
            .header(reqwest::header::AUTHORIZATION, alice.auth_header())
            .send()
            .await
            .expect("Failed to read notifications"),
    )
    .await;

    // O autor apaga o seu; a dona da foto apaga o outro
    let pelo_autor = app
        .api_client
        .delete(format!(
            "{}/api/v1/media/{}/comment/{}",
            &app.address, media_id, comment_id
        ))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .send()
        .await
        .expect("Failed to delete comment");

    let pela_dona = app
        .api_client
        .delete(format!(
            "{}/api/v1/media/{}/comment/{}",
            &app.address, media_id, outro_id
        ))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .send()
        .await
        .expect("Failed to delete comment");

    let sobrando = comments_of(&app, &media_id, None).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(created_status, StatusCode::CREATED);
    assert_eq!(anonimo.status(), StatusCode::UNAUTHORIZED);

    let kinds: Vec<&str> = notifications["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"media_comment"));

    assert_eq!(pelo_autor.status(), StatusCode::NO_CONTENT);
    assert_eq!(pela_dona.status(), StatusCode::NO_CONTENT);
    assert!(sobrando["results"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_stranger_cannot_delete_someone_elses_comment() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let estranho = register_and_login(&app, "Estranho").await;
    let album = create_album(&app, &alice, "Fotos", "everyone").await;
    let media_id = upload_id(&app, &alice, &album, "eu").await;

    let comment_id = json_of(comment(&app, Some(&bob), &media_id, "legal").await).await["results"]
        ["id"]
        .as_str()
        .expect("comment id")
        .to_string();

    // Act
    let response = app
        .api_client
        .delete(format!(
            "{}/api/v1/media/{}/comment/{}",
            &app.address, media_id, comment_id
        ))
        .header(reqwest::header::AUTHORIZATION, estranho.auth_header())
        .send()
        .await
        .expect("Failed to delete comment");

    let sobrando = comments_of(&app, &media_id, None).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(sobrando["results"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn deleting_the_album_takes_the_photos_with_it() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let album = create_album(&app, &alice, "Temporário", "everyone").await;
    let media_id = upload_id(&app, &alice, &album, "vai sumir").await;

    // Act
    let response = app
        .api_client
        .delete(format!("{}/api/v1/me/album/{}", &app.address, album))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .send()
        .await
        .expect("Failed to delete album");

    let gallery = json_of(
        app.api_client
            .get(format!(
                "{}/api/v1/profile/{}/media",
                &app.address, alice.slug
            ))
            .send()
            .await
            .expect("Failed to read the gallery"),
    )
    .await;

    let remaining = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM user_media WHERE id = $1"#,
        Uuid::parse_str(&media_id).unwrap(),
    )
    .fetch_one(&app.db_pool)
    .await
    .expect("Failed to count media");

    let detail = album_detail(&app, &album, Some(&alice)).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(remaining, 0, "a foto tinha que sumir junto do álbum");
    assert!(gallery["results"].as_array().unwrap().is_empty());
    assert_eq!(detail.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_album_limits_hold() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    let mut last = String::new();
    for i in 0..5 {
        last = create_album(&app, &alice, &format!("Álbum {i}"), "everyone").await;
    }

    // Act
    let over_limit = app
        .api_client
        .post(format!("{}/api/v1/me/album", &app.address))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .json(&json!({ "title": "Mais um" }))
        .send()
        .await
        .expect("Failed to create album");

    // Álbum dos outros não recebe foto minha
    let outro_album = upload(&app, &bob, Some(&last), None).await;

    let inexistente = upload(&app, &alice, Some(&Uuid::now_v7().to_string()), None).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(over_limit.status(), StatusCode::CONFLICT);
    assert_eq!(outro_album.status(), StatusCode::NOT_FOUND);
    assert_eq!(inexistente.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_photo_and_its_comment_can_be_reported_and_taken_down() {
    // Arrange
    let app = spawn_app().await;
    let admin = register_and_login(&app, "Admin").await;
    sqlx::query!(
        r#"
        UPDATE auth_user SET is_superuser = TRUE
        WHERE id = (SELECT user_id FROM user_profile WHERE slug = $1)
        "#,
        admin.slug,
    )
    .execute(&app.db_pool)
    .await
    .expect("Failed to promote");

    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    let album = create_album(&app, &alice, "Fotos", "everyone").await;
    let media_id = upload_id(&app, &alice, &album, "legenda da foto").await;

    let comment_id = json_of(comment(&app, Some(&bob), &media_id, "comentário ofensivo").await)
        .await["results"]["id"]
        .as_str()
        .expect("comment id")
        .to_string();

    // Act
    let denuncia_foto = app
        .api_client
        .post(format!("{}/api/v1/report/media/{}", &app.address, media_id))
        .header(reqwest::header::AUTHORIZATION, bob.auth_header())
        .json(&json!({ "reason": "nudity" }))
        .send()
        .await
        .expect("Failed to report media");
    let report_id = json_of(denuncia_foto).await["results"]["id"]
        .as_str()
        .expect("report id")
        .to_string();

    let denuncia_comentario = app
        .api_client
        .post(format!(
            "{}/api/v1/report/media-comment/{}",
            &app.address, comment_id
        ))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .json(&json!({ "reason": "harassment" }))
        .send()
        .await
        .expect("Failed to report comment");
    let comment_report_status = denuncia_comentario.status();

    let queue = json_of(
        app.api_client
            .get(format!("{}/api/v1/admin/report?kind=media", &app.address))
            .header(reqwest::header::AUTHORIZATION, admin.auth_header())
            .send()
            .await
            .expect("Failed to list the queue"),
    )
    .await;

    let detail = json_of(
        app.api_client
            .get(format!(
                "{}/api/v1/admin/report/{}",
                &app.address, report_id
            ))
            .header(reqwest::header::AUTHORIZATION, admin.auth_header())
            .send()
            .await
            .expect("Failed to read the report"),
    )
    .await;

    // A moderação tira a foto do ar
    let removed = app
        .api_client
        .post(format!(
            "{}/api/v1/admin/report/{}/remove",
            &app.address, report_id
        ))
        .header(reqwest::header::AUTHORIZATION, admin.auth_header())
        .json(&json!({ "reason": "conteúdo impróprio" }))
        .send()
        .await
        .expect("Failed to remove");

    let gallery = json_of(
        app.api_client
            .get(format!(
                "{}/api/v1/profile/{}/media",
                &app.address, alice.slug
            ))
            .send()
            .await
            .expect("Failed to read the gallery"),
    )
    .await;

    let restored = app
        .api_client
        .post(format!(
            "{}/api/v1/admin/content/media/{}/restore",
            &app.address, media_id
        ))
        .header(reqwest::header::AUTHORIZATION, admin.auth_header())
        .json(&json!({}))
        .send()
        .await
        .expect("Failed to restore");

    let gallery_depois = json_of(
        app.api_client
            .get(format!(
                "{}/api/v1/profile/{}/media",
                &app.address, alice.slug
            ))
            .send()
            .await
            .expect("Failed to read the gallery"),
    )
    .await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(comment_report_status, StatusCode::CREATED);

    let item = &queue["results"][0];
    assert_eq!(item["target"]["kind"], "media");
    assert_eq!(item["target"]["excerpt"], "legenda da foto");
    assert_eq!(item["accountable"]["profile"]["slug"], alice.slug);

    assert_eq!(detail["results"]["target"]["kind"], "media");
    assert_eq!(detail["results"]["target"]["media_kind"], "photo");
    assert!(
        detail["results"]["target"]["url"]
            .as_str()
            .unwrap()
            .ends_with(".png")
    );

    assert_eq!(removed.status(), StatusCode::NO_CONTENT);
    assert!(gallery["results"].as_array().unwrap().is_empty());

    assert_eq!(restored.status(), StatusCode::NO_CONTENT);
    assert_eq!(gallery_depois["results"].as_array().unwrap().len(), 1);
}
