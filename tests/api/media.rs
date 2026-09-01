use reqwest::StatusCode;
use serde_json::json;
use tukro_backend::constants::{MAX_PHOTOS_PER_USER, MAX_VIDEOS_PER_USER};

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

/// Cabeçalho de um MP4: tamanho do box, `ftyp` e a marca `isom`.
fn mp4_bytes() -> Vec<u8> {
    let mut bytes = vec![
        0x00, 0x00, 0x00, 0x20, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm', 0x00, 0x00, 0x02,
        0x00,
    ];
    bytes.extend_from_slice(b"isomiso2avc1mp41");
    bytes
}

/// Cabeçalho EBML com o doctype `webm`.
fn webm_bytes() -> Vec<u8> {
    let mut bytes = vec![
        0x1A, 0x45, 0xDF, 0xA3, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1F,
    ];
    bytes.extend_from_slice(b"\x42\x82\x84webm");
    bytes.extend_from_slice(&[0x00; 16]);
    bytes
}

/// Caminho em disco do arquivo servido por `/media` em desenvolvimento.
fn media_path(url: &str) -> std::path::PathBuf {
    let key = url.split("/media/").nth(1).expect("URL fora de /media");
    std::path::Path::new("media").join(key)
}

async fn upload(
    app: &TestApp,
    user: &RegisteredUser,
    filename: &str,
    bytes: Vec<u8>,
) -> reqwest::Response {
    let part = reqwest::multipart::Part::bytes(bytes).file_name(filename.to_string());
    let form = reqwest::multipart::Form::new().part("file", part);

    app.api_client
        .post(format!("{}/api/v1/me/media", &app.address))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .multipart(form)
        .send()
        .await
        .expect("Failed to upload media")
}

async fn profile_of(
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
        .expect("Failed to read the profile")
        .json()
        .await
        .expect("profile response not JSON")
}

async fn delete_media(app: &TestApp, user: &RegisteredUser, id: &str) -> reqwest::Response {
    app.api_client
        .delete(format!("{}/api/v1/me/media/{}", &app.address, id))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to delete media")
}

async fn list_album(
    app: &TestApp,
    slug: &str,
    viewer: Option<&RegisteredUser>,
    query: &str,
) -> reqwest::Response {
    let request = app.api_client.get(format!(
        "{}/api/v1/profile/{}/media?{}",
        &app.address, slug, query
    ));

    let request = match viewer {
        Some(viewer) => request.header(reqwest::header::AUTHORIZATION, viewer.auth_header()),
        None => request,
    };

    request.send().await.expect("Failed to list album")
}

async fn json_of(response: reqwest::Response) -> serde_json::Value {
    response.json().await.expect("response not JSON")
}

#[tokio::test]
async fn uploading_a_photo_stores_the_file_and_shows_it_on_the_album() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;

    // Act
    let response = upload(&app, &alice, "foto.png", png_bytes()).await;
    let status = response.status();
    let body = json_of(response).await;
    let url = body["results"]["url"].as_str().unwrap().to_string();
    let stored = media_path(&url).exists();

    let album = json_of(list_album(&app, &alice.slug, None, "").await).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["results"]["kind"], "photo");
    assert_eq!(body["results"]["content_type"], "image/png");
    assert!(stored, "o arquivo deveria estar no disco");
    assert_eq!(album["results"].as_array().unwrap().len(), 1);
    assert_eq!(album["results"][0]["url"], url);
}

#[tokio::test]
async fn uploading_accepts_mp4_and_webm_as_video() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;

    // Act
    let mp4 = json_of(upload(&app, &alice, "video.mp4", mp4_bytes()).await).await;
    let webm = json_of(upload(&app, &alice, "video.webm", webm_bytes()).await).await;

    let photos = json_of(list_album(&app, &alice.slug, None, "kind=photo").await).await;
    let videos = json_of(list_album(&app, &alice.slug, None, "kind=video").await).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(mp4["results"]["kind"], "video");
    assert_eq!(mp4["results"]["content_type"], "video/mp4");
    assert_eq!(webm["results"]["kind"], "video");
    assert_eq!(webm["results"]["content_type"], "video/webm");
    assert!(photos["results"].as_array().unwrap().is_empty());
    assert_eq!(videos["results"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn the_photo_limit_blocks_the_next_upload_until_one_is_deleted() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;

    let mut first_id = String::new();
    for i in 0..MAX_PHOTOS_PER_USER {
        let body = json_of(upload(&app, &alice, "foto.png", png_bytes()).await).await;
        if i == 0 {
            first_id = body["results"]["id"].as_str().unwrap().to_string();
        }
    }

    // Act
    let blocked = upload(&app, &alice, "foto.png", png_bytes()).await;
    let blocked_status = blocked.status();
    let blocked_body = json_of(blocked).await;

    let deleted = delete_media(&app, &alice, &first_id).await;
    let after_delete = upload(&app, &alice, "foto.png", png_bytes()).await;
    let album = json_of(list_album(&app, &alice.slug, None, "&limit=100").await).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(blocked_status, StatusCode::CONFLICT);
    assert_eq!(
        blocked_body["error"]["file"][0]["code"],
        "photo_limit_reached"
    );
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert_eq!(after_delete.status(), StatusCode::CREATED);
    assert_eq!(
        album["results"].as_array().unwrap().len() as i64,
        MAX_PHOTOS_PER_USER
    );
}

#[tokio::test]
async fn the_video_limit_is_counted_apart_from_the_photos() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;

    for _ in 0..MAX_VIDEOS_PER_USER {
        upload(&app, &alice, "video.mp4", mp4_bytes()).await;
    }

    // Act
    let blocked = upload(&app, &alice, "video.mp4", mp4_bytes()).await;
    let blocked_status = blocked.status();
    let blocked_body = json_of(blocked).await;
    let photo = upload(&app, &alice, "foto.png", png_bytes()).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(blocked_status, StatusCode::CONFLICT);
    assert_eq!(
        blocked_body["error"]["file"][0]["code"],
        "video_limit_reached"
    );
    assert_eq!(photo.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn deleting_removes_the_file_from_the_storage() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;

    let body = json_of(upload(&app, &alice, "foto.png", png_bytes()).await).await;
    let id = body["results"]["id"].as_str().unwrap().to_string();
    let path = media_path(body["results"]["url"].as_str().unwrap());

    // Act
    let deleted = delete_media(&app, &alice, &id).await;
    let file_exists = path.exists();
    let repeated = delete_media(&app, &alice, &id).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert!(!file_exists, "o arquivo deveria ter sido removido do disco");
    assert_eq!(repeated.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn only_the_owner_deletes_the_own_media() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;

    let body = json_of(upload(&app, &alice, "foto.png", png_bytes()).await).await;
    let id = body["results"]["id"].as_str().unwrap().to_string();

    // Act
    let by_stranger = delete_media(&app, &bob, &id).await;
    let album = json_of(list_album(&app, &alice.slug, None, "").await).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(by_stranger.status(), StatusCode::NOT_FOUND);
    assert_eq!(album["results"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn files_that_are_not_photo_or_video_are_rejected() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;

    // Act
    let by_name = upload(&app, &alice, "malicioso.html", b"<h1>oi</h1>".to_vec()).await;
    let by_name_status = by_name.status();
    let by_name_body = json_of(by_name).await;

    // Nome de imagem, conteúdo de texto: o sniff dos bytes é quem decide.
    let by_content = upload(&app, &alice, "malicioso.png", b"<h1>oi</h1>".to_vec()).await;
    let by_content_status = by_content.status();
    let by_content_body = json_of(by_content).await;

    let empty = upload(&app, &alice, "vazio.png", Vec::new()).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(by_name_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        by_name_body["error"]["file"][0]["code"],
        "unsupported_media"
    );
    assert_eq!(by_content_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        by_content_body["error"]["file"][0]["code"],
        "unsupported_media"
    );
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_album_follows_the_profile_privacy() {
    // Arrange
    let app = spawn_app().await;
    let alice = register_and_login(&app, "Alice").await;
    let bob = register_and_login(&app, "Bob").await;
    upload(&app, &alice, "foto.png", png_bytes()).await;

    app.api_client
        .put(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, alice.auth_header())
        .json(&json!({ "name": "Alice", "profile_visibility": "nobody" }))
        .send()
        .await
        .expect("Failed to update profile");

    // Act
    let stranger = list_album(&app, &alice.slug, Some(&bob), "").await;
    let anonymous = list_album(&app, &alice.slug, None, "").await;
    let owner = list_album(&app, &alice.slug, Some(&alice), "").await;
    let owner_status = owner.status();
    let owner_body = json_of(owner).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(stranger.status(), StatusCode::FORBIDDEN);
    assert_eq!(anonymous.status(), StatusCode::FORBIDDEN);
    assert_eq!(owner_status, StatusCode::OK);
    assert_eq!(owner_body["results"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn the_profile_counts_media_and_hides_the_counts_from_a_closed_album() {
    // Arrange
    let app = spawn_app().await;
    let owner = register_and_login(&app, "Dono").await;
    let stranger = register_and_login(&app, "Estranho").await;

    upload(&app, &owner, "uma.png", png_bytes()).await;
    upload(&app, &owner, "outra.png", png_bytes()).await;
    upload(&app, &owner, "video.mp4", mp4_bytes()).await;

    let public = profile_of(&app, &owner.slug, Some(&stranger)).await;
    let public_photos = public["results"]["photos_count"].clone();
    let public_videos = public["results"]["videos_count"].clone();

    // O perfil fechado esconde o álbum, e a contagem tem que sumir junto.
    app.api_client
        .put(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, owner.auth_header())
        .json(&json!({ "name": "Dono", "profile_visibility": "nobody" }))
        .send()
        .await
        .expect("Failed to close the profile");

    let hidden = profile_of(&app, &owner.slug, Some(&stranger)).await;
    let own = profile_of(&app, &owner.slug, Some(&owner)).await;

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(public_photos, 2);
    assert_eq!(public_videos, 1);
    assert!(hidden["results"]["photos_count"].is_null());
    assert!(hidden["results"]["videos_count"].is_null());
    assert_eq!(own["results"]["photos_count"], 2);
    assert_eq!(own["results"]["videos_count"], 1);
}
