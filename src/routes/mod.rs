pub mod attributes;
pub mod entities;
pub mod health;
pub mod judge;
pub mod scores;

use axum::Router;
use sqlx::PgPool;

pub fn router(pool: PgPool) -> Router {
    Router::new()
        .merge(health::routes())
        .nest("/v1", api_routes(pool))
}

fn api_routes(pool: PgPool) -> Router {
    Router::new()
        .merge(entities::routes())
        .merge(attributes::routes())
        .merge(judge::routes())
        .merge(scores::routes())
        .with_state(pool)
}
