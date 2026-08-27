use actix_governor::{Governor, GovernorConfig, governor::middleware::StateInformationMiddleware};
use actix_web::{
    dev::HttpServiceFactory,
    middleware::from_fn,
    web::{delete, get, post, put, resource, scope},
};

use crate::{
    middlewares::auth::{needs_authentication, needs_superuser, optional_authentication},
    routes::{
        admin, auth, block, community, conversation, feed, friend, healthcheck, me, media,
        notification, poll, profile, report, scrap, testimonial, topic,
    },
    utils::{CustomIpExtractor, UserSessionExtractor},
};

type IpGovernor = GovernorConfig<CustomIpExtractor, StateInformationMiddleware>;
type SessionGovernor = GovernorConfig<UserSessionExtractor, StateInformationMiddleware>;

pub struct RateLimits<'a> {
    pub strict: &'a IpGovernor,
    pub permissive: &'a IpGovernor,
    pub auth: &'a SessionGovernor,
}

pub fn api_urls(limits: &RateLimits<'_>) -> impl HttpServiceFactory + use<> {
    scope("/api/v1")
        .service(resource("/healthcheck").route(get().to(healthcheck::get)))
        .service(auth_urls(limits))
        .service(me_urls(limits))
        .service(profile_urls(limits))
        .service(community_urls(limits))
        .service(topic_urls(limits))
        .service(poll_urls(limits))
        .service(friend_urls(limits))
        .service(block_urls(limits))
        .service(scrap_urls(limits))
        .service(testimonial_urls(limits))
        .service(notification_urls(limits))
        .service(report_urls(limits))
        .service(admin_urls(limits))
}

fn auth_urls(limits: &RateLimits<'_>) -> impl HttpServiceFactory + use<> {
    let strict = limits.strict;
    let auth_limit = limits.auth;

    scope("/auth")
        .service(
            resource("/register")
                .wrap(Governor::new(strict))
                .route(post().to(auth::register::post)),
        )
        .service(
            resource("/login")
                .wrap(Governor::new(strict))
                .route(post().to(auth::login::post)),
        )
        .service(
            resource("/google")
                .wrap(Governor::new(strict))
                .route(get().to(auth::google::get)),
        )
        .service(
            resource("/google/callback")
                .wrap(Governor::new(strict))
                .route(get().to(auth::google::callback::get)),
        )
        .service(
            resource("/logout")
                .wrap(Governor::new(auth_limit))
                .wrap(from_fn(needs_authentication))
                .route(post().to(auth::logout::post)),
        )
        .service(
            resource("/logout-all")
                .wrap(Governor::new(auth_limit))
                .wrap(from_fn(needs_authentication))
                .route(post().to(auth::logout_all::post)),
        )
        .service(
            resource("/change-password")
                .wrap(Governor::new(auth_limit))
                .wrap(from_fn(needs_authentication))
                .route(post().to(auth::change_password::post)),
        )
        .service(
            resource("/forgot-password")
                .wrap(Governor::new(strict))
                .route(post().to(auth::forgot_password::post)),
        )
        .service(
            resource("/reset-password")
                .wrap(Governor::new(strict))
                .route(get().to(auth::reset_password::get))
                .route(post().to(auth::reset_password::post)),
        )
        .service(
            resource("/send-verification-email")
                .wrap(Governor::new(auth_limit))
                .wrap(from_fn(needs_authentication))
                .route(post().to(auth::send_verification_email::post)),
        )
        .service(
            resource("/verify-email")
                .wrap(Governor::new(auth_limit))
                .wrap(from_fn(needs_authentication))
                .route(post().to(auth::verify_email::post)),
        )
}

fn me_urls(limits: &RateLimits<'_>) -> impl HttpServiceFactory + use<> {
    scope("/me")
        .wrap(Governor::new(limits.auth))
        .wrap(from_fn(needs_authentication))
        .service(
            resource("")
                .route(get().to(me::get))
                .route(put().to(me::put)),
        )
        .service(
            resource("/avatar")
                .route(post().to(me::post_avatar))
                .route(delete().to(me::delete_avatar)),
        )
        .service(resource("/media").route(post().to(media::post)))
        .service(resource("/media/{id}").route(delete().to(media::delete)))
        .service(resource("/visitor").route(get().to(me::visitor)))
        .service(resource("/unread").route(get().to(me::unread)))
        .service(resource("/feed").route(get().to(feed::list)))
        .service(resource("/feed/seen").route(post().to(feed::seen)))
        .service(
            scope("/conversation")
                .service(resource("").route(get().to(conversation::list)))
                .service(resource("/with/{slug}").route(get().to(conversation::with_slug)))
                .service(
                    resource("/{id}")
                        .route(get().to(conversation::detail))
                        .route(delete().to(conversation::delete)),
                )
                .service(resource("/{id}/message").route(get().to(conversation::messages)))
                .service(resource("/{id}/read").route(post().to(conversation::read))),
        )
}

fn profile_urls(limits: &RateLimits<'_>) -> impl HttpServiceFactory + use<> {
    scope("/profile")
        .wrap(Governor::new(limits.permissive))
        .wrap(from_fn(optional_authentication))
        .service(resource("").route(get().to(profile::get)))
        .service(resource("/{slug}").route(get().to(profile::slug::get)))
        .service(resource("/{slug}/friend").route(get().to(profile::slug::friends::get)))
        .service(
            resource("/{slug}/scrap")
                .route(get().to(profile::slug::scraps::get))
                .route(post().to(profile::slug::scraps::post)),
        )
        .service(
            resource("/{slug}/testimonial")
                .route(get().to(profile::slug::testimonials::get))
                .route(post().to(profile::slug::testimonials::post)),
        )
        .service(
            resource("/{slug}/rating")
                .route(put().to(profile::slug::rating::put))
                .route(delete().to(profile::slug::rating::delete)),
        )
        .service(resource("/{slug}/community").route(get().to(profile::slug::communities::get)))
        .service(resource("/{slug}/media").route(get().to(profile::slug::media::get)))
        .service(resource("/{slug}/message").route(post().to(profile::slug::messages::post)))
        .service(
            resource("/{slug}/fan")
                .route(get().to(profile::slug::fans::get))
                .route(post().to(profile::slug::fans::post))
                .route(delete().to(profile::slug::fans::delete)),
        )
}

/// Mesmo arranjo de `/profile`: a leitura é pública, então o escopo usa
/// autenticação opcional e cada handler de escrita exige a sessão por conta
/// própria.
fn community_urls(limits: &RateLimits<'_>) -> impl HttpServiceFactory + use<> {
    scope("/community")
        .wrap(Governor::new(limits.permissive))
        .wrap(from_fn(optional_authentication))
        .service(
            resource("")
                .route(get().to(community::get))
                .route(post().to(community::post)),
        )
        .service(
            resource("/{slug}")
                .route(get().to(community::slug::get))
                .route(put().to(community::slug::put))
                .route(delete().to(community::slug::delete)),
        )
        .service(
            resource("/{slug}/avatar")
                .route(post().to(community::slug::post_avatar))
                .route(delete().to(community::slug::delete_avatar)),
        )
        .service(
            resource("/{slug}/member")
                .route(get().to(community::slug::members::get))
                .route(post().to(community::slug::members::join))
                .route(delete().to(community::slug::members::leave)),
        )
        .service(
            resource("/{slug}/member/{user_slug}")
                .route(delete().to(community::slug::members::kick)),
        )
        .service(
            resource("/{slug}/member/{user_slug}/role")
                .route(put().to(community::slug::members::put_role)),
        )
        .service(
            resource("/{slug}/owner").route(put().to(community::slug::members::transfer_owner)),
        )
        .service(
            resource("/{slug}/join-request")
                .route(get().to(community::slug::join_requests::get))
                .route(delete().to(community::slug::join_requests::cancel)),
        )
        .service(
            resource("/{slug}/join-request/{id}/accept")
                .route(post().to(community::slug::join_requests::accept)),
        )
        .service(
            resource("/{slug}/join-request/{id}/reject")
                .route(post().to(community::slug::join_requests::reject)),
        )
        .service(
            resource("/{slug}/topic")
                .route(get().to(community::slug::topics::get))
                .route(post().to(community::slug::topics::post)),
        )
        .service(
            resource("/{slug}/poll")
                .route(get().to(community::slug::polls::get))
                .route(post().to(community::slug::polls::post)),
        )
}

/// Mesmo arranjo de `/topic`: a leitura da enquete é pública e cada handler de
/// escrita exige a sessão por conta própria.
fn poll_urls(limits: &RateLimits<'_>) -> impl HttpServiceFactory + use<> {
    scope("/poll")
        .wrap(Governor::new(limits.permissive))
        .wrap(from_fn(optional_authentication))
        .service(
            resource("/{id}")
                .route(get().to(poll::get))
                .route(put().to(poll::put))
                .route(delete().to(poll::delete)),
        )
        .service(resource("/{id}/close").route(post().to(poll::close)))
        .service(resource("/{id}/vote").route(post().to(poll::vote)))
}

/// Mesmo arranjo de `/community`: o fórum de comunidade pública é lido por
/// qualquer um, e cada handler de escrita exige a sessão por conta própria.
fn topic_urls(limits: &RateLimits<'_>) -> impl HttpServiceFactory + use<> {
    scope("/topic")
        .wrap(Governor::new(limits.permissive))
        .wrap(from_fn(optional_authentication))
        .service(
            resource("/{id}")
                .route(get().to(topic::get))
                .route(put().to(topic::put))
                .route(delete().to(topic::delete)),
        )
        .service(
            resource("/{id}/pin")
                .route(post().to(topic::pin))
                .route(delete().to(topic::unpin)),
        )
        .service(
            resource("/{id}/lock")
                .route(post().to(topic::lock))
                .route(delete().to(topic::unlock)),
        )
        .service(
            resource("/{id}/comment")
                .route(get().to(topic::comments::get))
                .route(post().to(topic::comments::post)),
        )
        .service(
            resource("/{id}/comment/{comment_id}/context")
                .route(get().to(topic::comments::context)),
        )
        .service(
            resource("/{id}/comment/{comment_id}")
                .route(put().to(topic::comments::put))
                .route(delete().to(topic::comments::delete)),
        )
}

/// `needs_superuser` roda por dentro de `needs_authentication`, que é quem
/// popula a sessão que ele lê.
fn admin_urls(limits: &RateLimits<'_>) -> impl HttpServiceFactory + use<> {
    scope("/admin")
        .wrap(Governor::new(limits.auth))
        .wrap(from_fn(needs_superuser))
        .wrap(from_fn(needs_authentication))
        .service(
            scope("/report")
                .service(resource("").route(get().to(admin::report::get)))
                .service(resource("/{id}").route(get().to(admin::report::detail)))
                .service(resource("/{id}/reviewing").route(post().to(admin::report::reviewing)))
                .service(resource("/{id}/resolve").route(post().to(admin::report::resolve)))
                .service(resource("/{id}/dismiss").route(post().to(admin::report::dismiss))),
        )
        .service(
            scope("/user").service(
                resource("/{slug}/suspend")
                    .route(post().to(admin::user::suspend))
                    .route(delete().to(admin::user::unsuspend)),
            ),
        )
}

/// Denúncia usa o limite estrito por IP: é uma ação rara e cara de moderar.
fn report_urls(limits: &RateLimits<'_>) -> impl HttpServiceFactory + use<> {
    scope("/report")
        .wrap(Governor::new(limits.strict))
        .wrap(from_fn(needs_authentication))
        .service(resource("/{slug}").route(post().to(report::post)))
}

/// `/read-all` precisa vir antes de `/{id}`, senão o resource do id casa com a
/// rota e a requisição morre em 405.
fn notification_urls(limits: &RateLimits<'_>) -> impl HttpServiceFactory + use<> {
    scope("/notification")
        .wrap(Governor::new(limits.auth))
        .wrap(from_fn(needs_authentication))
        .service(resource("").route(get().to(notification::get)))
        .service(resource("/read-all").route(post().to(notification::read_all)))
        .service(resource("/{id}/read").route(post().to(notification::read)))
        .service(resource("/{id}").route(delete().to(notification::delete)))
}

/// Só o dono e o autor mexem em depoimento, então o escopo inteiro exige sessão.
/// A lista pública de depoimentos aceitos fica em /profile/{slug}/testimonial.
fn testimonial_urls(limits: &RateLimits<'_>) -> impl HttpServiceFactory + use<> {
    scope("/testimonial")
        .wrap(Governor::new(limits.auth))
        .wrap(from_fn(needs_authentication))
        .service(resource("").route(get().to(testimonial::get)))
        .service(resource("/{id}/accept").route(post().to(testimonial::accept)))
        .service(resource("/{id}/reject").route(post().to(testimonial::reject)))
        .service(
            resource("/{id}")
                .route(put().to(testimonial::put))
                .route(delete().to(testimonial::delete)),
        )
}

/// Leitura do mural é pública, então o escopo usa autenticação opcional e cada
/// handler de escrita exige a sessão por conta própria.
fn scrap_urls(limits: &RateLimits<'_>) -> impl HttpServiceFactory + use<> {
    scope("/scrap")
        .wrap(Governor::new(limits.permissive))
        .wrap(from_fn(optional_authentication))
        .service(
            resource("/{id}")
                .route(put().to(scrap::put))
                .route(delete().to(scrap::delete)),
        )
        .service(
            resource("/{id}/reply")
                .route(get().to(scrap::get_replies))
                .route(post().to(scrap::post_reply)),
        )
}

fn block_urls(limits: &RateLimits<'_>) -> impl HttpServiceFactory + use<> {
    scope("/block")
        .wrap(Governor::new(limits.auth))
        .wrap(from_fn(needs_authentication))
        .service(resource("").route(get().to(block::get)))
        .service(
            resource("/{slug}")
                .route(post().to(block::post))
                .route(delete().to(block::delete)),
        )
}

fn friend_urls(limits: &RateLimits<'_>) -> impl HttpServiceFactory + use<> {
    scope("/friend")
        .wrap(Governor::new(limits.auth))
        .wrap(from_fn(needs_authentication))
        .service(
            scope("/request")
                .service(resource("").route(get().to(friend::request::get)))
                .service(resource("/{id}/accept").route(post().to(friend::request::accept)))
                .service(resource("/{id}/reject").route(post().to(friend::request::reject)))
                .service(
                    resource("/{slug}")
                        .route(post().to(friend::request::post))
                        .route(delete().to(friend::request::cancel)),
                ),
        )
        .service(resource("/{slug}").route(delete().to(friend::delete)))
}
