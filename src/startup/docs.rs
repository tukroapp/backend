use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::routes::{
    admin, auth, block, community, conversation, feed, friend, healthcheck, me, media, notification,
    poll, profile, report, scrap, testimonial, topic,
};

#[derive(OpenApi)]
#[openapi(
    paths(
        healthcheck::get,

        // Auth
        auth::register::post,
        auth::login::post,
        auth::logout::post,
        auth::logout_all::post,
        auth::change_password::post,
        auth::send_verification_email::post,
        auth::verify_email::post,
        auth::forgot_password::post,
        auth::reset_password::post,
        auth::reset_password::get,

        // Google OAuth
        auth::google::get,
        auth::google::callback::get,

        // Profile
        me::get,
        me::put,
        me::post_avatar,
        me::delete_avatar,
        me::visitor,
        me::unread,

        // Feed
        feed::list,
        feed::seen,

        // Mensagens
        profile::slug::messages::post,
        conversation::list,
        conversation::with_slug,
        conversation::detail,
        conversation::messages,
        conversation::read,
        conversation::delete,
        profile::get,
        profile::slug::get,
        profile::slug::friends::get,
        profile::slug::fans::get,
        profile::slug::fans::post,
        profile::slug::fans::delete,

        // Media
        media::post,
        media::delete,
        profile::slug::media::get,

        // Friends
        friend::request::get,
        friend::request::post,
        friend::request::accept,
        friend::request::reject,
        friend::request::cancel,
        friend::delete,

        // Scraps
        profile::slug::scraps::get,
        profile::slug::scraps::post,
        scrap::get_replies,
        scrap::post_reply,
        scrap::put,
        scrap::delete,

        // Testimonials
        profile::slug::testimonials::get,
        profile::slug::testimonials::post,
        profile::slug::rating::put,
        profile::slug::rating::delete,
        testimonial::get,
        testimonial::put,
        testimonial::delete,
        testimonial::accept,
        testimonial::reject,

        // Communities
        community::get,
        community::post,
        community::slug::get,
        community::slug::put,
        community::slug::delete,
        community::slug::post_avatar,
        community::slug::delete_avatar,
        community::slug::members::get,
        community::slug::members::join,
        community::slug::members::leave,
        community::slug::members::kick,
        community::slug::members::put_role,
        community::slug::members::transfer_owner,
        community::slug::join_requests::get,
        community::slug::join_requests::accept,
        community::slug::join_requests::reject,
        community::slug::join_requests::cancel,
        profile::slug::communities::get,

        // Forum
        community::slug::topics::get,
        community::slug::topics::post,
        topic::get,
        topic::put,
        topic::delete,
        topic::pin,
        topic::unpin,
        topic::lock,
        topic::unlock,
        topic::comments::get,
        topic::comments::post,
        topic::comments::put,
        topic::comments::delete,
        topic::comments::context,

        // Polls
        community::slug::polls::get,
        community::slug::polls::post,
        poll::get,
        poll::put,
        poll::delete,
        poll::close,
        poll::vote,

        // Notifications
        notification::get,
        notification::read,
        notification::read_all,
        notification::delete,

        // Reports
        report::post,

        // Admin
        admin::report::get,
        admin::report::detail,
        admin::report::reviewing,
        admin::report::resolve,
        admin::report::dismiss,
        admin::user::suspend,
        admin::user::unsuspend,

        // Blocks
        block::get,
        block::post,
        block::delete,
    ),
    info(
        title = "Tukro Backend API",
        version = "0.0.1-dev",
        description = "API documentation for the Tukro Backend application.",
        contact(name = "Lucas"),
        license(name = "Proprietary"),
    )
)]
struct ApiDoc;

pub fn swagger_ui() -> SwaggerUi {
    SwaggerUi::new("/api/docs/{_:.*}").url("/api/docs/openapi.json", ApiDoc::openapi())
}
