pub mod attributes;
pub mod auth;
pub mod entities;
pub mod health;
pub mod judge;
pub mod pages;
pub mod rate;
pub mod scores;

use axum::Router;
use std::sync::Arc;

use crate::auth::AppState;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .merge(health::routes().with_state(state.clone()))
        // HTML pages at root level (not under /v1)
        .merge(pages::routes().with_state(state.clone()))
        // API routes under /v1
        .nest("/v1", api_routes(state))
}

fn api_routes(state: Arc<AppState>) -> Router {
    Router::new()
        .merge(health::api_routes())
        .merge(entities::routes())
        .merge(attributes::routes())
        .merge(judge::routes())
        .merge(scores::routes())
        .merge(rate::routes())
        .merge(auth::routes())
        .with_state(state)
}
