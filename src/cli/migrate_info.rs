use crate::{
    cli::migrate::{applied_versions, up_migrations},
    configuration::Settings,
    startup::get_connection_pool,
};

pub async fn run(configuration: &Settings) -> Result<(), anyhow::Error> {
    let pool = get_connection_pool(&configuration.database);
    let applied = applied_versions(&pool).await?;

    for migration in up_migrations() {
        let status = if applied.contains(&migration.version) {
            "aplicada"
        } else {
            "pendente"
        };

        println!(
            "{:<8} {} {}",
            status, migration.version, migration.description
        );
    }

    Ok(())
}
