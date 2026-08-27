use actix_web::{
    Error, HttpMessage,
    body::MessageBody,
    dev::{ServiceRequest, ServiceResponse},
    error::InternalError,
    http::header::{HeaderName, HeaderValue},
    middleware::Next,
};
use tracing_actix_web::RequestId;

pub mod auth;

#[tracing::instrument(name = "Middleware comum para rotas de empresa", skip_all)]
pub async fn common(
    mut req: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<impl MessageBody>, Error> {
    // Simulated delay
    let simulate_delay = false;

    if simulate_delay {
        let delay = rand::random_range(600..750);
        tracing::info!("Simulating delay of {} ms", delay);
        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
    }

    let request_id = req
        .extensions()
        .get::<RequestId>()
        .map(|id| id.to_string())
        .expect("RequestId should be set by tracing_actix_web middleware");

    req.headers_mut().insert(
        HeaderName::from_static("x-request-id"),
        HeaderValue::from_str(&request_id).unwrap(),
    );

    let res_id_ok = request_id.clone();
    let res_id_err = request_id;

    next.call(req)
        .await
        .map(move |mut res| {
            res.headers_mut().insert(
                HeaderName::from_static("x-request-id"),
                HeaderValue::from_str(&res_id_ok).unwrap(),
            );
            res
        })
        .map_err(move |err| {
            let mut error_response = err.error_response();

            error_response.headers_mut().insert(
                HeaderName::from_static("x-request-id"),
                HeaderValue::from_str(&res_id_err).unwrap(),
            );

            InternalError::from_response(err, error_response).into()
        })
}
