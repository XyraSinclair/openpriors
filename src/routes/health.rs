use std::sync::Arc;

use axum::{extract::State, http::StatusCode, routing::get, Json, Router};
use serde::Serialize;

use crate::auth::AppState;

const SERVICE_NAME: &str = env!("CARGO_PKG_NAME");
const SERVICE_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn routes() -> Router<Arc<AppState>> {
    shared_routes()
}

pub fn api_routes() -> Router<Arc<AppState>> {
    shared_routes()
}

fn shared_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/health", get(legacy_health))
        .route("/healthz", get(liveness))
        .route("/readyz", get(readiness))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct HealthResponse {
    ok: bool,
    service: &'static str,
    version: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct ReadinessChecks {
    database: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct ReadinessResponse {
    ok: bool,
    service: &'static str,
    version: &'static str,
    checks: ReadinessChecks,
}

async fn legacy_health() -> &'static str {
    "ok"
}

async fn liveness() -> Json<HealthResponse> {
    Json(liveness_payload())
}

async fn readiness(State(state): State<Arc<AppState>>) -> (StatusCode, Json<ReadinessResponse>) {
    match crate::db::ping(&state.db).await {
        Ok(()) => (StatusCode::OK, Json(readiness_payload(true, "ok"))),
        Err(error) => {
            tracing::error!("database readiness check failed: {error}");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(readiness_payload(false, "error")),
            )
        }
    }
}

fn liveness_payload() -> HealthResponse {
    HealthResponse {
        ok: true,
        service: SERVICE_NAME,
        version: SERVICE_VERSION,
    }
}

fn readiness_payload(ok: bool, database: &'static str) -> ReadinessResponse {
    ReadinessResponse {
        ok,
        service: SERVICE_NAME,
        version: SERVICE_VERSION,
        checks: ReadinessChecks { database },
    }
}

#[cfg(test)]
mod tests {
    use super::{liveness_payload, readiness_payload};

    #[test]
    fn liveness_payload_reports_service_identity() {
        let payload = liveness_payload();
        assert!(payload.ok);
        assert!(!payload.service.is_empty());
        assert!(!payload.version.is_empty());
    }

    #[test]
    fn readiness_payload_reflects_database_status() {
        let payload = readiness_payload(false, "error");
        assert!(!payload.ok);
        assert_eq!(payload.checks.database, "error");
    }
}
