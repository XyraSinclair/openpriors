use std::sync::Arc;

use cardinal_harness::gateway::{NoopUsageSink, ProviderGateway};
use openpriors::auth::AppState;
use openpriors::config::Config;
use openpriors::db;
use openpriors::routes;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = Config::from_env();
    let pool = db::connect(&config.database_url).await;

    tracing::info!("connected to database");

    // Build provider gateway (reads OPENROUTER_API_KEY from env)
    let usage_sink = Arc::new(NoopUsageSink);
    let gateway = ProviderGateway::from_env(usage_sink)
        .expect("failed to create provider gateway — is OPENROUTER_API_KEY set?");

    let state = Arc::new(AppState {
        db: pool,
        gateway: Arc::new(gateway),
        config,
    });

    let app = routes::router(state)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    let bind_addr = &std::env::var("BIND_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8080".to_string());

    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .expect("failed to bind");

    tracing::info!("listening on {bind_addr}");

    axum::serve(listener, app).await.expect("server error");
}
