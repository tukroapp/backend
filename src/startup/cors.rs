use actix_cors::Cors;

use crate::configuration::Environment;

pub fn mount_cors_middleware(env: &Environment, allowed_origins: Vec<String>) -> Cors {
    let mut cors = Cors::default()
        .supports_credentials()
        .expose_headers(vec!["x-request-id"])
        .allow_any_method()
        .allow_any_header();

    if *env == Environment::Development {
        cors = cors.allowed_origin_fn(|_origin, _req_head| true);
    } else {
        cors = cors.allowed_origin_fn(move |origin, _req_head| {
            if let Ok(origin_str) = origin.to_str() {
                allowed_origins.iter().any(|o| o == origin_str)
            } else {
                false
            }
        });
    }

    cors
}
