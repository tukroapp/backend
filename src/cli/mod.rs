mod createsuperuser;
mod health;
mod migrate;
mod migrate_info;
mod serve;

use clap::{Parser, Subcommand};

use crate::configuration::get_configuration;

#[derive(Parser)]
#[command(name = "tukro", version, about = "Backend do Tukro")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Sobe o servidor HTTP.
    Serve {
        /// Sobrescreve o APP_HOST do ambiente.
        #[arg(long)]
        host: Option<String>,

        /// Sobrescreve o APP_PORT do ambiente.
        #[arg(long)]
        port: Option<u16>,
    },

    /// Aplica as migrations pendentes.
    Migrate {
        /// Só lista o que seria aplicado.
        #[arg(long)]
        dry_run: bool,
    },

    /// Lista as migrations aplicadas e as pendentes.
    MigrateInfo,

    /// Faz um ping no Postgres e no Redis e sai com 1 se algum falhar.
    Health,

    /// Cria um usuário administrador.
    Createsuperuser {
        /// E-mail do administrador. Ausente, é perguntado no terminal.
        #[arg(long)]
        email: Option<String>,

        /// Lê a senha da entrada padrão, sem prompt e sem confirmação.
        #[arg(long)]
        password_stdin: bool,

        /// Aceita a senha como ela veio, sem a regra de tamanho mínimo.
        #[arg(long)]
        skip_password_validation: bool,
    },
}

impl Cli {
    /// Lê a configuração uma única vez e entrega ao subcomando: nenhum deles
    /// toca no ambiente por conta própria.
    pub async fn run(self) -> Result<(), anyhow::Error> {
        let configuration = get_configuration()?;

        match self.command {
            Commands::Serve { host, port } => serve::run(configuration, host, port).await,
            Commands::Migrate { dry_run } => migrate::run(&configuration, dry_run).await,
            Commands::MigrateInfo => migrate_info::run(&configuration).await,
            Commands::Health => health::run(&configuration).await,
            Commands::Createsuperuser {
                email,
                password_stdin,
                skip_password_validation,
            } => {
                createsuperuser::run(
                    &configuration,
                    email,
                    password_stdin,
                    skip_password_validation,
                )
                .await
            }
        }
    }
}
