use crate::helpers::{drop_app, spawn_app};

#[tokio::test]
async fn healthcheck_works() {
    // Arrange
    let app = spawn_app().await;

    // Act
    let response = app
        .api_client
        .get(format!("{}/api/v1/healthcheck", &app.address))
        .send()
        .await
        .expect("Failed to send request");

    // Tear down
    drop_app(app).await;

    // Assert
    assert!(response.status().is_success());
}
