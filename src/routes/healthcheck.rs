use actix_web::{HttpResponse, Responder};
use serde::Serialize;
use utoipa::ToSchema;

use crate::utils::ApiResponse;

#[utoipa::path(
    get,
    description = "Endpoint de verificação de saúde da API",
    path = "/api/v1/healthcheck",
    responses(
        (status = 200, description = "Healthcheck OK", body = ApiResponse<HealthCheckResponse>),
    ),
)]
pub async fn get() -> impl Responder {
    HttpResponse::Ok()
        .content_type("application/json")
        .body(r#"{"results":{"status":"ok"}}"#)
}

#[allow(dead_code)]
#[derive(Serialize, Debug, ToSchema)]
struct HealthCheckResponse {
    #[schema(example = "ok")]
    status: String,
}
