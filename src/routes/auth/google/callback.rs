use actix_web::{
    HttpRequest, HttpResponse, Responder,
    cookie::{Cookie, SameSite, time::Duration},
    error::InternalError,
    web::{Data, Query},
};
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, PkceCodeVerifier, RedirectUrl,
    TokenResponse, TokenUrl, basic::BasicClient,
};
use serde::Deserialize;
use sqlx::PgPool;
use utoipa::ToSchema;

use crate::{
    configuration::Environment,
    services::auth::{
        GoogleUserInfo, LoginError, LoginType, SessionMeta, login, make_token_cookie,
    },
    startup::{AppState, RedisPool},
    utils::{ApiErrorResponse, e500},
};

/// O callback é um redirect, não uma resposta JSON, então a falha só chega ao
/// front pela query string. Suspensão sai com os detalhes que a tela de login
/// precisa mostrar; o resto vira um código genérico, porque a mensagem interna
/// não é para o usuário final.
fn login_error_query(error: &LoginError) -> String {
    let LoginError::Suspended(suspension) = error else {
        tracing::warn!("Google login failed: {:?}", error);
        return "error=login_failed".to_string();
    };

    let params = [
        ("error", Some("account_suspended".to_string())),
        ("until", suspension.until.map(|until| until.to_rfc3339())),
        ("reason", suspension.reason.clone()),
    ];

    serde_urlencoded::to_string(
        params
            .iter()
            .filter_map(|(key, value)| value.as_ref().map(|value| (key, value)))
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| "error=login_failed".to_string())
}

#[utoipa::path(
    get,
    operation_id = "auth_google_callback",
    tag = "Auth",
    summary = "Callback de login do Google",
    path = "/api/v1/auth/google/callback",
    description = "Callback do OAuth2 do Google após o login bem-sucedido",
    params(
        ("code" = String, Query, description = "O código de autorização retornado pelo Google após o login bem-sucedido"),
        ("state" = String, Query, description = "O token de estado OAuth2 para verificação CSRF")
    ),
    responses(
        (status = 302, description = "Redireciona para a página de callback do frontend com o cookie de sessão definido"),
        (status = 400, description = "Requisição inválida", body = ApiErrorResponse),
    ),
)]
#[tracing::instrument(name = "Callback do OAuth2 do Google", skip_all)]
pub async fn get(
    app_state: Data<AppState>,
    query: Query<AuthRequest>,
    request: HttpRequest,
    pool: Data<PgPool>,
    redis: Data<RedisPool>,
) -> Result<impl Responder, actix_web::Error> {
    let code = AuthorizationCode::new(query.code.clone());

    let state_cookie = request.cookie("oauth_state").ok_or_else(|| {
        InternalError::from_response(
            "OAuth state cookie not found",
            HttpResponse::BadRequest().json(ApiErrorResponse::from_single_validation_error(
                "state",
                "not_found",
                Some("O cookie de estado OAuth não foi encontrado."),
                vec![],
            )),
        )
    })?;

    let expected_state = CsrfToken::new(state_cookie.value().to_string());
    if expected_state.secret() != &query.state {
        return Err(InternalError::from_response(
            "OAuth state mismatch",
            HttpResponse::BadRequest().json(ApiErrorResponse::from_single_validation_error(
                "state",
                "invalid",
                Some("O token de estado OAuth é inválido."),
                vec![],
            )),
        )
        .into());
    }

    let cookie = request.cookie("pkce_verifier").ok_or_else(|| {
        InternalError::from_response(
            "PKCE verifier cookie not found",
            HttpResponse::BadRequest().json(ApiErrorResponse::from_single_validation_error(
                "pkce_verifier",
                "not_found",
                Some("O cookie do verificador PKCE não foi encontrado."),
                vec![],
            )),
        )
    })?;

    let pkce_verifier = PkceCodeVerifier::new(cookie.value().to_string());

    let token_result = BasicClient::new(ClientId::new(app_state.google_client_id.clone()))
        .set_client_secret(ClientSecret::new(app_state.google_client_secret.clone()))
        .set_token_uri(TokenUrl::new(app_state.google_token_uri.clone()).map_err(e500)?)
        .set_auth_uri(AuthUrl::new(app_state.google_auth_uri.clone()).map_err(e500)?)
        .set_redirect_uri(RedirectUrl::new(app_state.google_redirect_uri.clone()).map_err(e500)?)
        .exchange_code(code)
        .set_pkce_verifier(pkce_verifier)
        .request_async(&oauth2::reqwest::Client::new())
        .await;

    let token = match token_result {
        Ok(token) => token,
        Err(err) => {
            tracing::error!("Error exchanging code for token: {:?}", err);
            return Ok(HttpResponse::BadRequest().json(
                ApiErrorResponse::from_single_validation_error(
                    "authorization_code",
                    "invalid",
                    Some("O código de autorização é inválido ou expirou."),
                    vec![],
                ),
            ));
        }
    };

    let client = reqwest::Client::new();
    let user_info: GoogleUserInfo = client
        .get(app_state.google_user_info_uri.clone())
        .bearer_auth(token.access_token().secret())
        .send()
        .await
        .map_err(e500)?
        .json()
        .await
        .map_err(e500)?;

    let meta = SessionMeta::from_http_request(&request);

    let token = match login(&pool, &redis, LoginType::Google(user_info), meta).await {
        Err(err) => {
            return Ok(HttpResponse::Found()
                .append_header((
                    "Location",
                    format!(
                        "{}/login/google/callback?{}",
                        app_state.frontend_url,
                        login_error_query(&err)
                    ),
                ))
                .finish());
        }
        Ok(session) => session.token,
    };

    let next = request
        .cookie("next_redirect")
        .map(|c| c.value().to_string());

    let mut cookie_builder_next = Cookie::build("next_redirect", "")
        .path("/")
        .same_site(SameSite::Lax)
        .max_age(Duration::seconds(0));

    let mut cookie_builder_state = Cookie::build("oauth_state", "")
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .max_age(Duration::seconds(0));

    let mut cookie_builder_pkce = Cookie::build("pkce_verifier", "")
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .max_age(Duration::seconds(0));

    if app_state.environment == Environment::Production {
        cookie_builder_next = cookie_builder_next
            .secure(true)
            .domain(app_state.domain.clone());
        cookie_builder_state = cookie_builder_state
            .secure(true)
            .domain(app_state.domain.clone());
        cookie_builder_pkce = cookie_builder_pkce
            .secure(true)
            .domain(app_state.domain.clone());
    }

    let cookie_next = cookie_builder_next.finish();
    let cookie_state = cookie_builder_state.finish();
    let cookie_pkce = cookie_builder_pkce.finish();
    let cookie_token = make_token_cookie(&app_state, &token);

    Ok(HttpResponse::Found()
        .append_header((
            "Location",
            match next {
                Some(n) => format!(
                    "{}/login/google/callback?next={}",
                    app_state.frontend_url, n
                ),
                None => format!("{}/login/google/callback", app_state.frontend_url),
            },
        ))
        .cookie(cookie_next)
        .cookie(cookie_state)
        .cookie(cookie_pkce)
        .cookie(cookie_token)
        .finish())
}

#[derive(Deserialize, ToSchema)]
pub struct AuthRequest {
    #[schema(example = "4/P7q7W9...")]
    code: String,
    #[schema(example = "random_state_token")]
    state: String,
}
