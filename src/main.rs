use tukro_backend::configuration::get_configuration;
use tukro_backend::startup::Application;
use tukro_backend::telemetry::{get_subscriber, init_subscriber};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let subscriber = get_subscriber(
        "tukro_backend".to_string(),
        "info".to_string(),
        std::io::stdout,
    );
    init_subscriber(subscriber);

    let configuration = get_configuration().expect("Failed to read configuration");
    let app = Application::build(configuration).await?;
    app.run_until_stopped().await?;
    Ok(())
}
