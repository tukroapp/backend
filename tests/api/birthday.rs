use chrono::{Datelike, FixedOffset, NaiveDate, Utc};
use reqwest::StatusCode;
use serde_json::Value;

use crate::helpers::{RegisteredUser, TestApp, drop_app, register_and_login, spawn_app};

async fn befriend(app: &TestApp, a: &RegisteredUser, b: &RegisteredUser) {
    sqlx::query(
        r#"
        INSERT INTO friend (user_id, friend_id)
        SELECT p1.user_id, p2.user_id FROM user_profile p1, user_profile p2
        WHERE p1.slug = $1 AND p2.slug = $2
        UNION ALL
        SELECT p2.user_id, p1.user_id FROM user_profile p1, user_profile p2
        WHERE p1.slug = $1 AND p2.slug = $2
        "#,
    )
    .bind(&a.slug)
    .bind(&b.slug)
    .execute(&app.db_pool)
    .await
    .expect("Failed to befriend");
}

async fn set_birthdate(app: &TestApp, user: &RegisteredUser, birthdate: NaiveDate) {
    sqlx::query("UPDATE user_profile SET birthdate = $1 WHERE slug = $2")
        .bind(birthdate)
        .bind(&user.slug)
        .execute(&app.db_pool)
        .await
        .expect("Failed to set birthdate");
}

async fn birthdays(app: &TestApp, user: &RegisteredUser, query: &str) -> reqwest::Response {
    app.api_client
        .get(format!("{}/api/v1/me/birthday?{}", app.address, query))
        .header(reqwest::header::AUTHORIZATION, user.auth_header())
        .send()
        .await
        .expect("Failed to list birthdays")
}

/// Ano bissexto, para o dia 29 de fevereiro continuar sendo uma data válida.
const BIRTH_YEAR: i32 = 1992;

fn same_day_of(date: NaiveDate) -> NaiveDate {
    NaiveDate::from_ymd_opt(BIRTH_YEAR, date.month(), date.day()).expect("invalid birthdate")
}

#[tokio::test]
async fn the_birthday_list_brings_the_friends_of_the_current_month() {
    // Arrange
    let app = spawn_app().await;
    let today = Utc::now().date_naive();
    // Outro dia do mesmo mês, que nunca cai no dia de hoje.
    let other_day = if today.day() == 1 { 2 } else { 1 };
    let next_month = today.month() % 12 + 1;

    let me = register_and_login(&app, "Dono do Perfil").await;
    let today_friend = register_and_login(&app, "Amigo de Hoje").await;
    let month_friend = register_and_login(&app, "Amigo do Mes").await;
    let other_month_friend = register_and_login(&app, "Amigo de Outro Mes").await;
    let no_birthdate_friend = register_and_login(&app, "Amigo Sem Data").await;
    let stranger = register_and_login(&app, "Estranho de Hoje").await;

    for friend in [
        &today_friend,
        &month_friend,
        &other_month_friend,
        &no_birthdate_friend,
    ] {
        befriend(&app, &me, friend).await;
    }

    set_birthdate(&app, &today_friend, same_day_of(today)).await;
    set_birthdate(
        &app,
        &month_friend,
        NaiveDate::from_ymd_opt(BIRTH_YEAR, today.month(), other_day).unwrap(),
    )
    .await;
    set_birthdate(
        &app,
        &other_month_friend,
        NaiveDate::from_ymd_opt(BIRTH_YEAR, next_month, 1).unwrap(),
    )
    .await;
    set_birthdate(&app, &stranger, same_day_of(today)).await;

    // Act
    let response = birthdays(&app, &me, "tz=UTC").await;
    let status = response.status();
    let body: Value = response.json().await.expect("birthday response not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["results"]["today"], today.to_string());
    assert_eq!(body["results"]["time_zone"], "UTC");

    let listed = body["results"]["birthdays"].as_array().unwrap();
    let slugs: Vec<&str> = listed
        .iter()
        .map(|entry| entry["profile"]["slug"].as_str().unwrap())
        .collect();

    // Ordenado por dia do mês, então a ordem depende de onde hoje cai.
    let expected = if other_day < today.day() {
        vec![month_friend.slug.as_str(), today_friend.slug.as_str()]
    } else {
        vec![today_friend.slug.as_str(), month_friend.slug.as_str()]
    };
    assert_eq!(slugs, expected);

    let today_entry = listed
        .iter()
        .find(|entry| entry["profile"]["slug"] == today_friend.slug.as_str())
        .unwrap();
    assert_eq!(today_entry["is_today"], true);
    assert_eq!(today_entry["day"], today.day());
    assert_eq!(today_entry["age"], today.year() - BIRTH_YEAR);

    let month_entry = listed
        .iter()
        .find(|entry| entry["profile"]["slug"] == month_friend.slug.as_str())
        .unwrap();
    assert_eq!(month_entry["is_today"], false);
}

#[tokio::test]
async fn the_birthday_list_follows_the_time_zone_from_the_request() {
    // Arrange
    let app = spawn_app().await;
    // Kiritimati é UTC+14 e não tem horário de verão. A diferença de 26 horas
    // para o UTC-12 garante que as duas pontas nunca estão no mesmo dia.
    let far_east = FixedOffset::east_opt(14 * 3600).unwrap();
    let today_far_east = Utc::now().with_timezone(&far_east).date_naive();

    let me = register_and_login(&app, "Dono do Perfil").await;
    let friend = register_and_login(&app, "Amigo do Pacifico").await;
    befriend(&app, &me, &friend).await;
    set_birthdate(&app, &friend, same_day_of(today_far_east)).await;

    // Act
    let east: Value = birthdays(&app, &me, "tz=Pacific/Kiritimati")
        .await
        .json()
        .await
        .expect("birthday response not JSON");
    let west: Value = birthdays(&app, &me, "tz=Etc/GMT%2B12")
        .await
        .json()
        .await
        .expect("birthday response not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(east["results"]["today"], today_far_east.to_string());
    assert_eq!(east["results"]["birthdays"][0]["is_today"], true);
    // O mesmo instante, do outro lado da linha do tempo, não é mais o aniversário.
    assert_ne!(west["results"]["today"], east["results"]["today"]);
    assert!(
        west["results"]["birthdays"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["is_today"] == false)
    );
}

#[tokio::test]
async fn the_birthday_list_rejects_an_unknown_time_zone() {
    // Arrange
    let app = spawn_app().await;
    let me = register_and_login(&app, "Dono do Perfil").await;

    // Act
    let response = birthdays(&app, &me, "tz=Marte/Olympus_Mons").await;
    let status = response.status();
    let body: Value = response.json().await.expect("birthday response not JSON");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["tz"][0]["code"], "unknown_time_zone");
}

#[tokio::test]
async fn the_birthday_list_requires_authentication() {
    // Arrange
    let app = spawn_app().await;

    // Act
    let response = app
        .api_client
        .get(format!("{}/api/v1/me/birthday", app.address))
        .send()
        .await
        .expect("Failed to list birthdays");

    let status = response.status();

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
