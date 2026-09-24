//! Entry point of the fixture backend.
//!
//! Listens on `BIND_ADDR` (default `0.0.0.0:8080`). When `DATABASE_URL` is set
//! the bundled migrations run before the server starts; otherwise the process
//! serves without a database.

use actix_web::{App, HttpServer};
use api::{configure, FixtureConfig, DATABASE_URL_ENV};

const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8080";

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    if let Ok(database_url) = std::env::var(DATABASE_URL_ENV) {
        api::migrate(&database_url)
            .await
            .map_err(|e| std::io::Error::other(format!("migration failed: {e}")))?;
        tracing::info!("migrations applied");
    } else {
        tracing::info!("{DATABASE_URL_ENV} unset; running without a database");
    }

    let config = FixtureConfig::from_env();
    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| DEFAULT_BIND_ADDR.to_string());
    tracing::info!(%bind_addr, break_route = config.break_route, "starting fixture api");

    HttpServer::new(move || App::new().configure(|c| configure(c, config.clone())))
        .bind(&bind_addr)?
        .run()
        .await
}
