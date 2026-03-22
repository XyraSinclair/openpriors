use axum::{routing::get, Router};

pub fn routes() -> Router {
    Router::new().route("/health", get(health))
}

pub fn api_routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new().route("/health", get(health))
}

async fn health() -> &'static str {
    "ok"
}
