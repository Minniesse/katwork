use std::{process::ExitCode, sync::Arc};

use katwork::error::KatError;
use katwork_server::{
    app::{AppState, build_router},
    config::Config,
};
use sqlx::postgres::PgPoolOptions;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}", error.code());
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), KatError> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "serve".into());
    if args.next().is_some() {
        return Err(KatError::CommandInvalid);
    }
    match command.as_str() {
        "serve" => {
            let config = Arc::new(Config::from_env()?);
            let pool = PgPoolOptions::new()
                .connect(&config.database_url)
                .await
                .map_err(|_| KatError::DatabaseUnavailable)?;
            let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
            axum::serve(listener, build_router(AppState { pool, config })).await?;
            Ok(())
        }
        // Database migrations and demo data are wired in Tasks 2 and 3.
        "migrate" | "seed-demo" => Ok(()),
        _ => Err(KatError::CommandInvalid),
    }
}
