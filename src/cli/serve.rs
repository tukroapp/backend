use crate::{
    configuration::Settings,
    startup::Application,
    telemetry::{get_subscriber, init_subscriber},
};

/// Sobe o servidor. `host` e `port` vêm da linha de comando e ganham do
/// ambiente quando presentes.
pub async fn run(
    mut configuration: Settings,
    host: Option<String>,
    port: Option<u16>,
) -> Result<(), anyhow::Error> {
    if let Some(host) = host {
        configuration.application.host = host;
    }

    if let Some(port) = port {
        configuration.application.port = port;
    }

    let subscriber = get_subscriber(
        "tukro_backend".to_string(),
        "info".to_string(),
        std::io::stdout,
    );
    init_subscriber(subscriber);

    let application = Application::build(configuration).await?;
    application.run_until_stopped().await?;

    Ok(())
}
