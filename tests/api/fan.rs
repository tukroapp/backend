use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};

async fn json_of(response: reqwest::Response) -> Value {
    response.json().await.expect("response not JSON")
}

async fn become_fan(app: &TestApp, fan: &RegisteredUser, idol_slug: &str) -> reqwest::Response {
    app.api_client
        .post(format!("{}/api/v1/profile/{}/fan", app.address, idol_slug))
        .header(reqwest::header::AUTHORIZATION, fan.auth_header())
        .send()
        .await
        .expect("Failed to become fan")
}

async fn stop_fan(app: &TestApp, fan: &RegisteredUser, idol_slug: &str) -> reqwest::Response {
    app.api_client
        .delete(format!("{}/api/v1/profile/{}/fan", app.address, idol_slug))
        .header(reqwest::header::AUTHORIZATION, fan.auth_header())
        .send()
        .await
        .expect("Failed to stop being fan")
}

async fn profile(app: &TestApp, slug: &str, viewer: &RegisteredUser) -> Value {
    let response = app
        .api_client
        .get(format!("{}/api/v1/profile/{}", app.address, slug))
        .header(reqwest::header::AUTHORIZATION, viewer.auth_header())
        .send()
        .await
        .expect("Failed to get profile");
    assert_eq!(response.status(), StatusCode::OK);
    json_of(response).await
}

#[tokio::test]
async fn becoming_a_fan_is_unilateral_and_needs_no_friendship() {
    let app = spawn_app().await;
    let idol = register_and_login(&app, "Idolo").await;
    let fan = register_and_login(&app, "Fa").await;

    let response = become_fan(&app, &fan, &idol.slug).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_of(response).await;
    assert_eq!(body["results"]["is_fan"], json!(true));
    assert_eq!(body["results"]["fans_count"], json!(1));

    // Idempotente.
    let again = json_of(become_fan(&app, &fan, &idol.slug).await).await;
    assert_eq!(again["results"]["fans_count"], json!(1));

    let idol_profile = profile(&app, &idol.slug, &fan).await;
    assert_eq!(idol_profile["results"]["fans_count"], json!(1));
    assert_eq!(idol_profile["results"]["is_fan"], json!(true));
    assert_eq!(idol_profile["results"]["can_be_fan"], json!(true));

    drop_app(app).await;
}

#[tokio::test]
async fn stopping_being_a_fan_is_idempotent() {
    let app = spawn_app().await;
    let idol = register_and_login(&app, "Idolo").await;
    let fan = register_and_login(&app, "Fa").await;

    become_fan(&app, &fan, &idol.slug).await;

    let response = stop_fan(&app, &fan, &idol.slug).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_of(response).await;
    assert_eq!(body["results"]["is_fan"], json!(false));
    assert_eq!(body["results"]["fans_count"], json!(0));

    // De novo, sem erro.
    assert_eq!(
        stop_fan(&app, &fan, &idol.slug).await.status(),
        StatusCode::OK
    );

    assert_eq!(
        profile(&app, &idol.slug, &fan).await["results"]["is_fan"],
        json!(false)
    );

    drop_app(app).await;
}

#[tokio::test]
async fn cannot_be_a_fan_of_yourself_or_while_logged_out() {
    let app = spawn_app().await;
    let user = register_and_login(&app, "Fulano").await;

    assert_eq!(
        become_fan(&app, &user, &user.slug).await.status(),
        StatusCode::BAD_REQUEST
    );

    let anon = app
        .api_client
        .post(format!("{}/api/v1/profile/{}/fan", app.address, user.slug))
        .send()
        .await
        .expect("Failed to send");
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);

    let own = profile(&app, &user.slug, &user).await;
    assert_eq!(own["results"]["can_be_fan"], json!(false));

    drop_app(app).await;
}

#[tokio::test]
async fn a_blocked_user_cannot_become_a_fan_and_blocking_drops_the_fan() {
    let app = spawn_app().await;
    let idol = register_and_login(&app, "Idolo").await;
    let fan = register_and_login(&app, "Fa").await;

    become_fan(&app, &fan, &idol.slug).await;
    assert_eq!(
        profile(&app, &idol.slug, &idol).await["results"]["fans_count"],
        json!(1)
    );

    // Idol bloqueia o fã: a linha de fã some.
    app.api_client
        .post(format!("{}/api/v1/block/{}", app.address, fan.slug))
        .header(reqwest::header::AUTHORIZATION, idol.auth_header())
        .send()
        .await
        .expect("Failed to block");

    assert_eq!(
        profile(&app, &idol.slug, &idol).await["results"]["fans_count"],
        json!(0)
    );

    // E não consegue virar fã de novo enquanto bloqueado.
    assert_eq!(
        become_fan(&app, &fan, &idol.slug).await.status(),
        StatusCode::NOT_FOUND
    );

    drop_app(app).await;
}

#[tokio::test]
async fn the_fan_list_is_public_and_paginated() {
    let app = spawn_app().await;
    let idol = register_and_login(&app, "Idolo").await;
    let a = register_and_login(&app, "Ana").await;
    let b = register_and_login(&app, "Bia").await;

    become_fan(&app, &a, &idol.slug).await;
    become_fan(&app, &b, &idol.slug).await;

    let response = app
        .api_client
        .get(format!("{}/api/v1/profile/{}/fan", app.address, idol.slug))
        .send()
        .await
        .expect("Failed to list fans");
    assert_eq!(response.status(), StatusCode::OK);

    let body = json_of(response).await;
    let fans = body["results"].as_array().unwrap();
    assert_eq!(fans.len(), 2);
    let slugs: Vec<&str> = fans.iter().map(|f| f["slug"].as_str().unwrap()).collect();
    assert!(slugs.contains(&a.slug.as_str()));
    assert!(slugs.contains(&b.slug.as_str()));

    drop_app(app).await;
}

async fn list_fans(app: &TestApp, idol_slug: &str, query: &str) -> Value {
    let response = app
        .api_client
        .get(format!(
            "{}/api/v1/profile/{}/fan?{}",
            app.address, idol_slug, query
        ))
        .send()
        .await
        .expect("Failed to list fans");
    assert_eq!(response.status(), StatusCode::OK);
    json_of(response).await
}

/// Os slugs da listagem, na ordem em que vieram.
fn slugs_of(body: &Value) -> Vec<String> {
    body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|fan| fan["slug"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn the_fan_list_carries_the_fan_date_and_the_fan_own_count() {
    let app = spawn_app().await;
    let idol = register_and_login(&app, "Idolo").await;
    let fan = register_and_login(&app, "Ana").await;
    let other = register_and_login(&app, "Bia").await;

    become_fan(&app, &fan, &idol.slug).await;
    // A Ana também tem fã, e é esse número que a lista do ídolo mostra.
    become_fan(&app, &other, &fan.slug).await;

    let body = list_fans(&app, &idol.slug, "").await;

    drop_app(app).await;

    let listed = &body["results"][0];
    assert_eq!(listed["slug"], json!(fan.slug));
    assert_eq!(listed["fans_count"], json!(1));
    assert!(listed["fan_since"].is_string());
}

#[tokio::test]
async fn the_fan_list_filters_by_name_and_sorts_by_date_and_popularity() {
    let app = spawn_app().await;
    let idol = register_and_login(&app, "Idolo").await;
    let first = register_and_login(&app, "Ana Popular").await;
    let second = register_and_login(&app, "Bia Quieta").await;

    become_fan(&app, &first, &idol.slug).await;
    become_fan(&app, &second, &idol.slug).await;
    // Só a Ana tem fã, então ela lidera a ordenação por popularidade.
    become_fan(&app, &idol, &first.slug).await;

    let newest = list_fans(&app, &idol.slug, "sort=created_at_desc").await;
    let oldest = list_fans(&app, &idol.slug, "sort=created_at_asc").await;
    let popular = list_fans(&app, &idol.slug, "sort=fans_desc").await;
    let unpopular = list_fans(&app, &idol.slug, "sort=fans_asc").await;
    let filtered = list_fans(&app, &idol.slug, "q=quieta").await;

    let invalid = app
        .api_client
        .get(format!(
            "{}/api/v1/profile/{}/fan?sort=whatever",
            app.address, idol.slug
        ))
        .send()
        .await
        .expect("Failed to list fans");
    let invalid_status = invalid.status();
    let invalid_body = json_of(invalid).await;

    drop_app(app).await;

    assert_eq!(
        slugs_of(&newest),
        vec![second.slug.clone(), first.slug.clone()]
    );
    assert_eq!(
        slugs_of(&oldest),
        vec![first.slug.clone(), second.slug.clone()]
    );
    assert_eq!(
        slugs_of(&popular),
        vec![first.slug.clone(), second.slug.clone()]
    );
    assert_eq!(
        slugs_of(&unpopular),
        vec![second.slug.clone(), first.slug.clone()]
    );
    assert_eq!(slugs_of(&filtered), vec![second.slug]);
    assert_eq!(invalid_status, StatusCode::BAD_REQUEST);
    assert_eq!(invalid_body["error"]["sort"][0]["code"], "invalid_sort");
}

#[tokio::test]
async fn the_fan_list_cursor_walks_every_sort() {
    let app = spawn_app().await;
    let idol = register_and_login(&app, "Idolo").await;
    let a = register_and_login(&app, "Ana").await;
    let b = register_and_login(&app, "Bia").await;
    let c = register_and_login(&app, "Cida").await;

    become_fan(&app, &a, &idol.slug).await;
    become_fan(&app, &b, &idol.slug).await;
    become_fan(&app, &c, &idol.slug).await;
    // Popularidades distintas para a ordenação por fãs não empatar.
    become_fan(&app, &idol, &a.slug).await;
    become_fan(&app, &b, &a.slug).await;
    become_fan(&app, &idol, &b.slug).await;

    let mut walked = Vec::new();
    for sort in ["created_at_desc", "created_at_asc", "fans_desc", "fans_asc"] {
        let first = list_fans(&app, &idol.slug, &format!("sort={sort}&limit=2")).await;
        let cursor = first["next_cursor"].as_str().unwrap().to_string();
        let second = list_fans(
            &app,
            &idol.slug,
            &format!("sort={sort}&limit=2&cursor={cursor}"),
        )
        .await;

        let mut page = slugs_of(&first);
        page.extend(slugs_of(&second));
        walked.push((page, second["next_cursor"].clone()));
    }

    drop_app(app).await;

    for (page, next_cursor) in walked {
        // As duas páginas somam a lista inteira, sem repetir nem pular ninguém.
        assert_eq!(page.len(), 3);
        let mut sorted = page.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 3);
        assert!(next_cursor.is_null());
    }
}

#[tokio::test]
async fn suspending_a_fan_drops_it_from_the_denormalized_count() {
    let app = spawn_app().await;
    let idol = register_and_login(&app, "Idolo").await;
    let fan = register_and_login(&app, "Ana").await;

    become_fan(&app, &fan, &idol.slug).await;
    let before = json_of(become_fan(&app, &fan, &idol.slug).await).await;

    sqlx::query!(
        "UPDATE auth_user SET status = 'suspended' WHERE email = $1",
        fan.email
    )
    .execute(&app.db_pool)
    .await
    .expect("Failed to suspend the fan");

    let suspended = profile(&app, &idol.slug, &idol).await;

    sqlx::query!(
        "UPDATE auth_user SET status = 'active' WHERE email = $1",
        fan.email
    )
    .execute(&app.db_pool)
    .await
    .expect("Failed to reactivate the fan");

    let reactivated = profile(&app, &idol.slug, &idol).await;

    drop_app(app).await;

    assert_eq!(before["results"]["fans_count"], json!(1));
    assert_eq!(suspended["results"]["fans_count"], json!(0));
    assert_eq!(reactivated["results"]["fans_count"], json!(1));
}
