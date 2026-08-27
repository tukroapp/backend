use reqwest::StatusCode;
use serde_json::json;

use crate::helpers::{TestApp, drop_app, spawn_app};

#[tokio::test]
async fn me_returns_fields_from_both_tables() {
    // Arrange
    let app = spawn_app().await;
    app.test_user.login(&app).await;
    let email = app.test_user.email.clone();

    // Act
    let response = app
        .api_client
        .get(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .send()
        .await
        .expect("Failed to send request");

    let status = response.status();
    let body: serde_json::Value = response.json().await.expect("me response not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::OK);
    // auth_user
    assert_eq!(body["results"]["email"], email);
    assert_eq!(body["results"]["is_email_verified"], false);
    // conta criada com senha, não pelo Google
    assert_eq!(body["results"]["is_google"], false);
    // user_profile
    assert_eq!(body["results"]["name"], "Test User");
    assert!(body["results"]["slug"].is_string());
    // nunca vaza o hash de senha
    assert!(body["results"].get("password_hash").is_none());
}

#[tokio::test]
async fn me_flags_accounts_linked_to_google() {
    // Arrange
    let app = spawn_app().await;
    app.test_user.login(&app).await;

    sqlx::query!(
        "UPDATE auth_user SET google_id = $1 WHERE id = $2",
        app.test_user.id.to_string(),
        app.test_user.id,
    )
    .execute(&app.db_pool)
    .await
    .expect("Failed to link google account");

    // Act
    let body: serde_json::Value = app
        .api_client
        .get(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .send()
        .await
        .expect("Failed to send request")
        .json()
        .await
        .expect("me response not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(body["results"]["is_google"], true);
}

#[tokio::test]
async fn put_me_updates_profile_and_keeps_slug() {
    // Arrange
    let app = spawn_app().await;
    app.test_user.login(&app).await;

    let slug_before: String = {
        let response = app
            .api_client
            .get(format!("{}/api/v1/me", &app.address))
            .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
            .send()
            .await
            .expect("Failed to send request");
        let body: serde_json::Value = response.json().await.unwrap();
        body["results"]["slug"].as_str().unwrap().to_string()
    };

    // Act
    let response = app
        .api_client
        .put(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .json(&json!({
            "name": "Nome Atualizado",
            "bio": "Nova bio",
            "city": "Curitiba",
            "birthdate": "1990-05-20",
            "slug": "hacker-slug"
        }))
        .send()
        .await
        .expect("Failed to send request");

    let status = response.status();
    let body: serde_json::Value = response.json().await.expect("me response not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["results"]["name"], "Nome Atualizado");
    assert_eq!(body["results"]["bio"], "Nova bio");
    assert_eq!(body["results"]["city"], "Curitiba");
    assert_eq!(body["results"]["birthdate"], "1990-05-20");
    // slug é imutável: o campo enviado no body é ignorado
    assert_eq!(body["results"]["slug"], slug_before);
}

#[tokio::test]
async fn put_me_updates_privacy_and_keeps_it_when_omitted() {
    // Arrange
    let app = spawn_app().await;
    app.test_user.login(&app).await;

    // Act
    let updated: serde_json::Value = app
        .api_client
        .put(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .json(&json!({
            "name": "Test User",
            "profile_visibility": "friends",
            "scrap_read": "nobody",
            "scrap_write": "nobody",
            "testimonial_write": "everyone"
        }))
        .send()
        .await
        .expect("Failed to send request")
        .json()
        .await
        .expect("me response not JSON");

    // Salvar só o bloco de perfil não pode reabrir a privacidade
    let after_profile_save: serde_json::Value = app
        .api_client
        .put(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .json(&json!({ "name": "Outro Nome" }))
        .send()
        .await
        .expect("Failed to send request")
        .json()
        .await
        .expect("me response not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(updated["results"]["profile_visibility"], "friends");
    assert_eq!(updated["results"]["scrap_read"], "nobody");
    assert_eq!(updated["results"]["scrap_write"], "nobody");
    assert_eq!(updated["results"]["testimonial_write"], "everyone");

    assert_eq!(after_profile_save["results"]["name"], "Outro Nome");
    assert_eq!(
        after_profile_save["results"]["profile_visibility"],
        "friends"
    );
    assert_eq!(after_profile_save["results"]["scrap_read"], "nobody");
    assert_eq!(after_profile_save["results"]["scrap_write"], "nobody");
    assert_eq!(
        after_profile_save["results"]["testimonial_write"],
        "everyone"
    );
}

#[tokio::test]
async fn put_me_rejects_unknown_visibility() {
    // Arrange
    let app = spawn_app().await;
    app.test_user.login(&app).await;

    // Act
    let response = app
        .api_client
        .put(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .json(&json!({ "name": "Test User", "profile_visibility": "somente-eu" }))
        .send()
        .await
        .expect("Failed to send request");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn put_me_privacy_takes_effect_on_the_public_profile() {
    // Arrange
    let app = spawn_app().await;
    app.test_user.login(&app).await;

    let slug: String = app
        .api_client
        .get(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .send()
        .await
        .expect("Failed to send request")
        .json::<serde_json::Value>()
        .await
        .expect("me response not JSON")["results"]["slug"]
        .as_str()
        .unwrap()
        .to_string();

    // Act
    app.api_client
        .put(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .json(&json!({ "name": "Test User", "profile_visibility": "nobody" }))
        .send()
        .await
        .expect("Failed to send request");

    let public: serde_json::Value = app
        .api_client
        .get(format!("{}/api/v1/profile/{}", &app.address, slug))
        .send()
        .await
        .expect("Failed to get profile")
        .json()
        .await
        .expect("profile response not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(public["results"]["friends_visible"], false);
}

#[tokio::test]
async fn put_me_rejects_invalid_name() {
    // Arrange
    let app = spawn_app().await;
    app.test_user.login(&app).await;

    // Act
    let response = app
        .api_client
        .put(format!("{}/api/v1/me", &app.address))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .json(&json!({ "name": "" }))
        .send()
        .await
        .expect("Failed to send request");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn me_requires_authentication() {
    // Arrange
    let app = spawn_app().await;

    // Act
    let response = app
        .api_client
        .get(format!("{}/api/v1/me", &app.address))
        .send()
        .await
        .expect("Failed to send request");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// PNG 1x1 válido, para exercitar o sniff de tipo por bytes.
fn png_bytes() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0xDA, 0x63, 0xFC,
        0xCF, 0xC0, 0x50, 0x0F, 0x00, 0x04, 0x85, 0x01, 0x80, 0x84, 0xA9, 0x8C, 0x21, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ]
}

/// Caminho em disco do arquivo servido por `/media` em desenvolvimento.
fn media_path(url: &str) -> std::path::PathBuf {
    let key = url.split("/media/").nth(1).expect("URL fora de /media");
    std::path::Path::new("media").join(key)
}

async fn upload_avatar(app: &TestApp, filename: &str, bytes: Vec<u8>) -> reqwest::Response {
    let part = reqwest::multipart::Part::bytes(bytes).file_name(filename.to_string());
    let form = reqwest::multipart::Form::new().part("file", part);

    app.api_client
        .post(format!("{}/api/v1/me/avatar", &app.address))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .multipart(form)
        .send()
        .await
        .expect("Failed to upload avatar")
}

#[tokio::test]
async fn post_avatar_stores_the_image_and_replaces_the_previous_one() {
    // Arrange
    let app = spawn_app().await;
    app.test_user.login(&app).await;

    // Act
    let first: serde_json::Value = upload_avatar(&app, "foto.png", png_bytes())
        .await
        .json()
        .await
        .expect("avatar response not JSON");
    let first_url = first["results"]["avatar_url"]
        .as_str()
        .expect("avatar_url ausente")
        .to_string();

    let second: serde_json::Value = upload_avatar(&app, "outra.png", png_bytes())
        .await
        .json()
        .await
        .expect("avatar response not JSON");
    let second_url = second["results"]["avatar_url"]
        .as_str()
        .expect("avatar_url ausente")
        .to_string();

    let stored = media_path(&second_url);
    let old_file = media_path(&first_url);
    let stored_exists = stored.exists();
    let old_exists = old_file.exists();

    // Tear down
    drop_app(app).await;

    // Assert
    assert!(
        first_url.starts_with("http://") && first_url.contains("/media/avatar/"),
        "avatar_url deve ser absoluta, veio {first_url}"
    );
    assert_ne!(first_url, second_url);
    assert!(stored_exists, "o novo avatar deveria estar no disco");
    assert!(!old_exists, "o avatar anterior deveria ter sido removido");
}

#[tokio::test]
async fn post_avatar_rejects_files_that_are_not_images() {
    // Arrange
    let app = spawn_app().await;
    app.test_user.login(&app).await;

    // Act: extensão de imagem, conteúdo de HTML
    let response = upload_avatar(&app, "payload.png", b"<html>alert(1)</html>".to_vec()).await;
    let status = response.status();
    let body: serde_json::Value = response.json().await.expect("avatar response not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["file"][0]["code"], "unsupported_image");
}

#[tokio::test]
async fn delete_avatar_clears_the_field_and_removes_the_file() {
    // Arrange
    let app = spawn_app().await;
    app.test_user.login(&app).await;

    let uploaded: serde_json::Value = upload_avatar(&app, "foto.png", png_bytes())
        .await
        .json()
        .await
        .expect("avatar response not JSON");
    let url = uploaded["results"]["avatar_url"]
        .as_str()
        .expect("avatar_url ausente")
        .to_string();

    // Act
    let response = app
        .api_client
        .delete(format!("{}/api/v1/me/avatar", &app.address))
        .header(reqwest::header::AUTHORIZATION, app.test_user.auth_header())
        .send()
        .await
        .expect("Failed to delete avatar");

    let status = response.status();
    let body: serde_json::Value = response.json().await.expect("avatar response not JSON");
    let file_exists = media_path(&url).exists();

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::OK);
    assert!(body["results"]["avatar_url"].is_null());
    assert!(!file_exists, "o arquivo deveria ter sido removido do disco");
}

#[tokio::test]
async fn avatar_requires_authentication() {
    // Arrange
    let app = spawn_app().await;

    // Act
    let response = app
        .api_client
        .delete(format!("{}/api/v1/me/avatar", &app.address))
        .send()
        .await
        .expect("Failed to delete avatar");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
