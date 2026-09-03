use anyhow::Context;
use sqlx::{
    PgPool,
    migrate::{Migrate, Migrator},
};

use crate::{configuration::Settings, startup::get_connection_pool};

/// O diretório vai embutido no binário em tempo de compilação: o servidor de
/// produção recebe só o executável, sem os arquivos .sql.
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

pub async fn run(configuration: &Settings, dry_run: bool) -> Result<(), anyhow::Error> {
    let pool = get_connection_pool(&configuration.database);
    let applied = applied_versions(&pool).await?;

    let pending: Vec<_> = up_migrations()
        .filter(|migration| !applied.contains(&migration.version))
        .collect();

    if pending.is_empty() {
        println!("Nenhuma migration pendente.");
        return Ok(());
    }

    if dry_run {
        println!("{} migration(s) pendente(s):", pending.len());
        for migration in pending {
            println!("  {} {}", migration.version, migration.description);
        }
        return Ok(());
    }

    MIGRATOR
        .run(&pool)
        .await
        .context("Failed to run the pending migrations")?;

    println!("{} migration(s) aplicada(s):", pending.len());
    for migration in pending {
        println!("  {} {}", migration.version, migration.description);
    }

    Ok(())
}

/// Só o sentido "up": um par up/down é uma migration só, com a mesma versão, e
/// a tabela de controle só registra a ida.
pub fn up_migrations() -> impl Iterator<Item = &'static sqlx::migrate::Migration> {
    MIGRATOR
        .iter()
        .filter(|migration| !migration.migration_type.is_down_migration())
}

/// Versões já registradas na tabela de controle do sqlx, criada aqui quando o
/// banco ainda está vazio.
pub async fn applied_versions(pool: &PgPool) -> Result<Vec<i64>, anyhow::Error> {
    let mut connection = pool
        .acquire()
        .await
        .context("Failed to connect to Postgres")?;

    connection
        .ensure_migrations_table(&MIGRATOR.table_name)
        .await
        .context("Failed to create the migrations table")?;

    let applied = connection
        .list_applied_migrations(&MIGRATOR.table_name)
        .await
        .context("Failed to list the applied migrations")?;

    Ok(applied.into_iter().map(|m| m.version).collect())
}
