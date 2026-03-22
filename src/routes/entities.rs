use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::{AppState, AuthUser};
use crate::error::ApiError;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/entities", post(create_entity).get(list_entities))
        .route("/entities/{id}", get(get_entity))
}

#[derive(Deserialize)]
struct CreateEntity {
    uri: String,
    name: Option<String>,
    kind: Option<String>,
    payload: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct EntityResponse {
    id: Uuid,
    uri: String,
    name: Option<String>,
    kind: Option<String>,
    payload: serde_json::Value,
    created_at: chrono::DateTime<chrono::Utc>,
}

async fn create_entity(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Json(body): Json<CreateEntity>,
) -> Result<Json<EntityResponse>, ApiError> {
    auth.require_admin()?;
    auth.require_scope("entities:write")?;

    if body.uri.trim().is_empty() {
        return Err(ApiError::BadRequest("entity URI is required".into()));
    }
    if body.uri.len() > 2048 {
        return Err(ApiError::BadRequest("entity URI is too long".into()));
    }
    if let Some(name) = body.name.as_deref() {
        if name.len() > 256 {
            return Err(ApiError::BadRequest("entity name is too long".into()));
        }
    }
    if let Some(kind) = body.kind.as_deref() {
        if kind.len() > 64 {
            return Err(ApiError::BadRequest("entity kind is too long".into()));
        }
    }
    if let Some(payload) = body.payload.as_ref() {
        let serialized = serde_json::to_vec(payload)
            .map_err(|e| ApiError::BadRequest(format!("invalid payload JSON: {e}")))?;
        if serialized.len() > 64 * 1024 {
            return Err(ApiError::BadRequest("entity payload is too large".into()));
        }
    }

    let row = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            Option<String>,
            Option<String>,
            serde_json::Value,
            chrono::DateTime<chrono::Utc>,
        ),
    >(
        "INSERT INTO entities (uri, name, kind, payload)
         VALUES ($1, $2, $3, COALESCE($4, '{}'::jsonb))
         ON CONFLICT (uri) DO UPDATE SET
           name = COALESCE(EXCLUDED.name, entities.name),
           kind = COALESCE(EXCLUDED.kind, entities.kind)
         RETURNING id, uri, name, kind, payload, created_at",
    )
    .bind(body.uri.trim())
    .bind(&body.name)
    .bind(&body.kind)
    .bind(&body.payload)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(EntityResponse {
        id: row.0,
        uri: row.1,
        name: row.2,
        kind: row.3,
        payload: row.4,
        created_at: row.5,
    }))
}

#[derive(Deserialize)]
struct ListParams {
    kind: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn list_entities(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListParams>,
) -> Result<Json<Vec<EntityResponse>>, ApiError> {
    let limit = params.limit.unwrap_or(100).clamp(1, 1000);
    let offset = params.offset.unwrap_or(0).max(0);

    let rows = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            Option<String>,
            Option<String>,
            serde_json::Value,
            chrono::DateTime<chrono::Utc>,
        ),
    >(
        "SELECT id, uri, name, kind, payload, created_at FROM entities
         WHERE ($1::text IS NULL OR kind = $1)
         ORDER BY created_at DESC
         LIMIT $2 OFFSET $3",
    )
    .bind(&params.kind)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let entities: Vec<_> = rows
        .into_iter()
        .map(|r| EntityResponse {
            id: r.0,
            uri: r.1,
            name: r.2,
            kind: r.3,
            payload: r.4,
            created_at: r.5,
        })
        .collect();

    Ok(Json(entities))
}

async fn get_entity(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<Json<EntityResponse>, ApiError> {
    let row = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            Option<String>,
            Option<String>,
            serde_json::Value,
            chrono::DateTime<chrono::Utc>,
        ),
    >("SELECT id, uri, name, kind, payload, created_at FROM entities WHERE id = $1")
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::NotFound(format!("entity {id}")))?;

    Ok(Json(EntityResponse {
        id: row.0,
        uri: row.1,
        name: row.2,
        kind: row.3,
        payload: row.4,
        created_at: row.5,
    }))
}
