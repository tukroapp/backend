use actix_web::{
    HttpResponse, Responder,
    cookie::{Cookie, SameSite},
    web::{Data, Query},
};
use oauth2::{
    AuthUrl, ClientId, ClientSecret, CsrfToken, PkceCodeChallenge, RedirectUrl, Scope, TokenUrl,
    basic::BasicClient,
};
use serde::{Deserialize, Serialize};

use crate::{configuration::Environment, startup::AppState, utils::e500};

pub mod callback;

#[utoipa::path(
    get,
    operation_id = "auth_google",
    tag = "Auth",
    summary = "Iniciar fluxo Google OAuth",
    path = "/api/v1/auth/google",
    description = "Inicia o fluxo de login OAuth2 do Google",
    responses(
        (status = 302, description = "Redireciona para a página de autorização do Google com o cookie PKCE definido"),
    ),
)]
#[tracing::instrument(name = "Iniciando o fluxo de login OAuth2 do Google", skip_all)]
pub async fn get(
    app_state: Data<AppState>,
    next_query: Query<NextRedirect>,
) -> Result<impl Responder, actix_web::Error> {
    let client = BasicClient::new(ClientId::new(app_state.google_client_id.clone()))
        .set_client_secret(ClientSecret::new(app_state.google_client_secret.clone()))
        .set_token_uri(TokenUrl::new(app_state.google_token_uri.clone()).map_err(e500)?)
        .set_auth_uri(AuthUrl::new(app_state.google_auth_uri.clone()).map_err(e500)?)
        .set_redirect_uri(RedirectUrl::new(app_state.google_redirect_uri.clone()).map_err(e500)?);

    let (pkce_code_challenge, pkce_code_verifier) = PkceCodeChallenge::new_random_sha256();

    let mut cookie_builder =
        Cookie::build("pkce_verifier", pkce_code_verifier.secret().to_string())
            .path("/")
            .http_only(true)
            .same_site(SameSite::Lax);

    if app_state.environment == Environment::Production {
        cookie_builder = cookie_builder.secure(true).domain(app_state.domain.clone());
    } else {
        cookie_builder = cookie_builder.secure(false);
    }

    let cookie = cookie_builder.finish();

    let (auth_url, csrf_token) = client
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new("email".to_string()))
        .add_scope(Scope::new("profile".to_string()))
        .set_pkce_challenge(pkce_code_challenge)
        .url();

    let mut state_cookie_builder = Cookie::build("oauth_state", csrf_token.secret().to_string())
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax);

    let mut next_cookie = if let Some(next) = &next_query.next {
        Cookie::build("next_redirect", next.clone())
            .path("/")
            .same_site(SameSite::Lax)
    } else {
        Cookie::build("next_redirect", "/".to_string())
            .path("/")
            .same_site(SameSite::Lax)
    };

    if app_state.environment == Environment::Production {
        state_cookie_builder = state_cookie_builder
            .secure(true)
            .domain(app_state.domain.clone());
        next_cookie = next_cookie.secure(true).domain(app_state.domain.clone());
    } else {
        state_cookie_builder = state_cookie_builder.secure(false);
        next_cookie = next_cookie.secure(false);
    }

    let state_cookie = state_cookie_builder.finish();
    let next_cookie = next_cookie.finish();

    Ok(HttpResponse::Found()
        .append_header(("Location", auth_url.to_string()))
        .cookie(cookie)
        .cookie(state_cookie)
        .cookie(next_cookie)
        .finish())
}

#[derive(Serialize, Deserialize)]
pub struct NextRedirect {
    pub next: Option<String>,
}
