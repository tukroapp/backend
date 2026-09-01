use crate::helpers::{TestApp, TestUser, drop_app, spawn_app};
use serde_json::json;
use tukro_backend::constants::{CURRENT_TERMS_VERSION, SESSION_COOKIE_NAME};
use tukro_backend::services::auth::AccountStatus;
use uuid::Uuid;
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{header, method, path},
};

fn extract_state_from_location(location: &str) -> String {
    let url = reqwest::Url::parse(location).expect("Invalid redirect URL");
    url.query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.to_string())
        .expect("state param not found in redirect URL")
}

/// Starts the OAuth flow so the state/PKCE cookies are stored in the client, returning the
/// `state` value the callback expects back.
async fn initiate_flow(app: &TestApp) -> String {
    let response = app
        .api_client
        .get(format!("{}/api/v1/auth/google", app.address))
        .send()
        .await
        .expect("Failed to execute request");

    assert_eq!(response.status().as_u16(), 302);

    extract_state_from_location(
        response
            .headers()
            .get("Location")
            .unwrap()
            .to_str()
            .unwrap(),
    )
}

fn mount_token_success(app: &TestApp) -> impl std::future::Future<Output = ()> + '_ {
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "mock_access_token",
            "expires_in": 3600,
            "id_token": "mock_id_token",
            "token_type": "Bearer",
        })))
        .mount(&app.mock_google_server)
}

async fn mount_userinfo(app: &TestApp, body: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path("/userinfo"))
        .and(header("Authorization", "Bearer mock_access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&app.mock_google_server)
        .await;
}

#[tokio::test]
async fn login_with_google_oauth_works() {
    let app = spawn_app().await;
    let oauth_user = TestUser::generate();
    let google_id = oauth_user.store_google_oauth(&app.db_pool).await;

    let mock_code = "mock_authorization_code";

    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "mock_access_token",
            "expires_in": 3600,
            "id_token": "mock_id_token",
            "token_type": "Bearer",
        })))
        .mount(&app.mock_google_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/userinfo"))
        .and(header("Authorization", "Bearer mock_access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": google_id,
            "email": oauth_user.email,
            "name": "Mock User",
            "picture": "mock_picture_url"
        })))
        .mount(&app.mock_google_server)
        .await;

    let initiate_response = app
        .api_client
        .get(format!("{}/api/v1/auth/google", app.address))
        .send()
        .await
        .expect("Failed to execute request");

    assert_eq!(initiate_response.status().as_u16(), 302);

    let state = extract_state_from_location(
        initiate_response
            .headers()
            .get("Location")
            .unwrap()
            .to_str()
            .unwrap(),
    );

    let callback_response = app
        .api_client
        .get(format!(
            "{}/api/v1/auth/google/callback?code={}&state={}",
            app.address, mock_code, state
        ))
        .send()
        .await
        .expect("Failed to execute request");
    let callback_status = callback_response.status();

    let location = callback_response
        .headers()
        .get("Location")
        .unwrap()
        .to_str()
        .unwrap();

    let cookies = callback_response.cookies().collect::<Vec<_>>();
    let session_cookie = cookies.iter().find(|c| c.name() == SESSION_COOKIE_NAME);

    drop_app(app).await;

    assert_eq!(callback_status.as_u16(), 302);
    assert!(location.contains("/login/google/callback"));
    assert!(session_cookie.is_some());
}

#[tokio::test]
async fn login_with_google_oauth_fails_with_missing_state_cookie() {
    let app = spawn_app().await;
    let mock_code = "mock_authorization_code";

    let callback_response = app
        .api_client
        .get(format!(
            "{}/api/v1/auth/google/callback?code={}&state=some_state",
            app.address, mock_code
        ))
        .send()
        .await
        .expect("Failed to execute request");
    let callback_status = callback_response.status();
    let body = callback_response.text().await.unwrap();

    drop_app(app).await;

    assert_eq!(callback_status.as_u16(), 400);
    assert!(
        body.contains("O cookie de estado OAuth não foi encontrado."),
        "Expected missing state cookie error, got: {body}"
    );
}

#[tokio::test]
async fn login_with_google_oauth_fails_with_invalid_state() {
    let app = spawn_app().await;
    let mock_code = "mock_authorization_code";

    let initiate_response = app
        .api_client
        .get(format!("{}/api/v1/auth/google", app.address))
        .send()
        .await
        .expect("Failed to execute request");

    assert_eq!(initiate_response.status().as_u16(), 302);

    let callback_response = app
        .api_client
        .get(format!(
            "{}/api/v1/auth/google/callback?code={}&state=wrong_state_token",
            app.address, mock_code
        ))
        .send()
        .await
        .expect("Failed to execute request");
    let callback_status = callback_response.status();
    let body = callback_response.text().await.unwrap();

    drop_app(app).await;

    assert_eq!(callback_status.as_u16(), 400);
    assert!(
        body.contains("O token de estado OAuth é inválido."),
        "Expected invalid state error, got: {body}"
    );
}

#[tokio::test]
async fn login_with_google_oauth_fails_with_missing_pkce() {
    let app = spawn_app().await;
    let mock_code = "mock_authorization_code";

    let callback_response = app
        .api_client
        .get(format!(
            "{}/api/v1/auth/google/callback?code={}&state=some_state",
            app.address, mock_code
        ))
        .send()
        .await
        .expect("Failed to execute request");
    let callback_status = callback_response.status();
    let body = callback_response.text().await.unwrap();

    drop_app(app).await;

    assert_eq!(callback_status.as_u16(), 400);
    assert!(
        body.contains("O cookie do verificador PKCE não foi encontrado.")
            || body.contains("O cookie de estado OAuth não foi encontrado."),
        "Expected a 400 error for missing cookies, got: {body}"
    );
}

#[tokio::test]
async fn login_with_google_oauth_fails_when_token_exchange_fails() {
    let app = spawn_app().await;
    let state = initiate_flow(&app).await;

    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant"
        })))
        .mount(&app.mock_google_server)
        .await;

    let response = app
        .api_client
        .get(format!(
            "{}/api/v1/auth/google/callback?code=bad_code&state={}",
            app.address, state
        ))
        .send()
        .await
        .expect("Failed to execute request");
    let status = response.status();
    let body = response.text().await.unwrap();

    drop_app(app).await;

    assert_eq!(status.as_u16(), 400);
    assert!(
        body.contains("O código de autorização é inválido ou expirou."),
        "Expected invalid authorization code error, got: {body}"
    );
}

#[tokio::test]
async fn google_login_conflicts_when_email_already_belongs_to_password_account() {
    let app = spawn_app().await;
    // A regular password user already owns this email (created by spawn_app).
    let email = app.test_user.email.clone();
    let state = initiate_flow(&app).await;

    mount_token_success(&app).await;
    mount_userinfo(
        &app,
        json!({
            "id": Uuid::new_v4(),
            "email": email,
            "name": "Mock User",
            "picture": "mock_picture_url"
        }),
    )
    .await;

    let response = app
        .api_client
        .get(format!(
            "{}/api/v1/auth/google/callback?code=code&state={}",
            app.address, state
        ))
        .send()
        .await
        .expect("Failed to execute request");
    let status = response.status();
    let location = response
        .headers()
        .get("Location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let has_session = response
        .cookies()
        .any(|c| c.name() == SESSION_COOKIE_NAME && !c.value().is_empty());

    drop_app(app).await;

    assert_eq!(status.as_u16(), 302);
    assert!(
        location.contains("error="),
        "Expected error redirect, got: {location}"
    );
    assert!(!has_session, "No session should be set on a conflict");
}

#[tokio::test]
async fn google_login_conflicts_when_google_account_has_password() {
    let app = spawn_app().await;
    let google_id = Uuid::new_v4();
    let email = format!("{}@test.com", Uuid::new_v4());

    // A user linked to this google_id but that also has a password set.
    sqlx::query!(
        r#"
        WITH u AS (
            INSERT INTO auth_user (email, google_id, password_hash)
            VALUES ($1, $2, 'dummy-hash')
            RETURNING id
        )
        INSERT INTO user_profile (user_id, slug, name)
            SELECT id, $3, 'Google User' FROM u
        "#,
        email,
        google_id.to_string(),
        format!("u{}", &Uuid::new_v4().simple().to_string()[..20]),
    )
    .execute(&app.db_pool)
    .await
    .expect("Failed to seed google user with password");

    let state = initiate_flow(&app).await;
    mount_token_success(&app).await;
    mount_userinfo(
        &app,
        json!({
            "id": google_id,
            "email": email,
            "name": "Mock User",
            "picture": "mock_picture_url"
        }),
    )
    .await;

    let response = app
        .api_client
        .get(format!(
            "{}/api/v1/auth/google/callback?code=code&state={}",
            app.address, state
        ))
        .send()
        .await
        .expect("Failed to execute request");
    let status = response.status();
    let location = response
        .headers()
        .get("Location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    drop_app(app).await;

    assert_eq!(status.as_u16(), 302);
    assert!(
        location.contains("error="),
        "Expected error redirect, got: {location}"
    );
}

#[tokio::test]
async fn google_login_reactivates_deactivated_account() {
    let app = spawn_app().await;
    let google_id = Uuid::new_v4();
    let email = format!("{}@test.com", Uuid::new_v4());

    // A deactivated google account that should be reactivated on login.
    let user_id = sqlx::query_scalar!(
        r#"
        WITH u AS (
            INSERT INTO auth_user (email, google_id, status)
            VALUES ($1, $2, 'deactivated')
            RETURNING id
        ), p AS (
            INSERT INTO user_profile (user_id, slug, name)
                SELECT id, $3, 'Google User' FROM u
        )
        SELECT id AS "id!" FROM u
        "#,
        email,
        google_id.to_string(),
        format!("u{}", &Uuid::new_v4().simple().to_string()[..20]),
    )
    .fetch_one(&app.db_pool)
    .await
    .expect("Failed to seed deactivated google user");

    let state = initiate_flow(&app).await;
    mount_token_success(&app).await;
    mount_userinfo(
        &app,
        json!({
            "id": google_id,
            "email": email,
            "name": "Reactivated User",
            "picture": "mock_picture_url"
        }),
    )
    .await;

    let response = app
        .api_client
        .get(format!(
            "{}/api/v1/auth/google/callback?code=code&state={}",
            app.address, state
        ))
        .send()
        .await
        .expect("Failed to execute request");
    let response_status = response.status();
    let has_session = response
        .cookies()
        .any(|c| c.name() == SESSION_COOKIE_NAME && !c.value().is_empty());

    let status = sqlx::query_scalar!(
        r#"SELECT status AS "status: AccountStatus" FROM auth_user WHERE id = $1"#,
        user_id
    )
    .fetch_one(&app.db_pool)
    .await
    .unwrap();

    drop_app(app).await;

    assert_eq!(response_status.as_u16(), 302);
    assert!(has_session, "A session should be set after reactivation");
    assert_eq!(
        status,
        AccountStatus::Active,
        "The account should have been reactivated"
    );
}

#[tokio::test]
async fn a_brand_new_google_account_records_the_terms_acceptance() {
    // Arrange
    let app = spawn_app().await;
    let email = format!("{}@gmail.com", Uuid::new_v4());

    mount_token_success(&app).await;
    mount_userinfo(
        &app,
        json!({
            "id": Uuid::new_v4().to_string(),
            "email": email,
            "name": "Recém-chegado",
            "picture": "mock_picture_url"
        }),
    )
    .await;

    let state = initiate_flow(&app).await;

    // Act
    let callback = app
        .api_client
        .get(format!(
            "{}/api/v1/auth/google/callback?code=mock_code&state={}",
            app.address, state
        ))
        .send()
        .await
        .expect("Failed to execute request");

    let callback_status = callback.status();

    let versions = sqlx::query_scalar!(
        r#"
        SELECT ta.version
        FROM terms_acceptance ta
        INNER JOIN auth_user au ON au.id = ta.user_id
        WHERE au.email = $1
        "#,
        email,
    )
    .fetch_all(&app.db_pool)
    .await
    .expect("Failed to read the terms acceptance");

    // Tear down
    drop_app(app).await;

    // Assert
    assert_eq!(callback_status.as_u16(), 302);
    assert_eq!(versions, vec![CURRENT_TERMS_VERSION]);
}
