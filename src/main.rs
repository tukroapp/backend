use std::process::ExitCode;

use clap::Parser;
use tukro_backend::cli::Cli;

#[tokio::main]
async fn main() -> ExitCode {
    if let Err(error) = Cli::parse().run().await {
        eprintln!("erro: {error:#}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
