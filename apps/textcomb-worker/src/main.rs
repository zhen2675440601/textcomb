use std::sync::Arc;
use textcomb_core::{AppConfig, db, worker::Worker};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let filter = EnvFilter::try_from_env("TEXTCOMB_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info,textcomb=debug"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .with_current_span(false)
        .with_span_list(false)
        .init();

    let config = Arc::new(AppConfig::from_env()?);
    let pool = db::connect(&config.database_url).await?;
    let worker = Worker::new(pool, config).await?;
    info!("TextComb worker started");
    worker.run().await?;
    Ok(())
}
