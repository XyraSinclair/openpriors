use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::ApiError;

pub fn routes() -> Router<PgPool> {
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
    State(pool): State<PgPool>,
    Json(body): Json<CreateEntity>,
) -> Result<Json<EntityResponse>, ApiError> {
    let row = sqlx::query_as::<_, (Uuid, String, Option<String>, Option<String>, serde_json::Value, chrono::DateTime<chrono::Utc>)>(
        "INSERT INTO entities (uri, name, kind, payload)
         VALUES ($1, $2, $3, COALESCE($4, '{}'::jsonb))
         ON CONFLICT (uri) DO UPDATE SET
           name = COALESCE(EXCLUDED.name, entities.name),
           kind = COALESCE(EXCLUDED.kind, entities.kind)
         RETURNING id, uri, name, kind, payload, created_at",
    )
    .bind(&body.uri)
    .bind(&body.name)
    .bind(&body.kind)
    .bind(&body.payload)
    .fetch_one(&pool)
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
    State(pool): State<PgPool>,
    Query(params): Query<ListParams>,
) -> Result<Json<Vec<EntityResponse>>, ApiError> {
    let limit = params.limit.unwrap_or(100).min(1000);
    let offset = params.offset.unwrap_or(0);

    let rows = sqlx::query_as::<_, (Uuid, String, Option<String>, Option<String>, serde_json::Value, chrono::DateTime<chrono::Utc>)>(
        "SELECT id, uri, name, kind, payload, created_at FROM entities
         WHERE ($1::text IS NULL OR kind = $1)
         ORDER BY created_at DESC
         LIMIT $2 OFFSET $3",
    )
    .bind(&params.kind)
    .bind(limit)
    .bind(offset)
    .fetch_all(&pool)
    .await?;

    let entities: Vec<_> = rows
        .into_iter()
        .map(|r| EntityResponse {
            id: r.0, uri: r.1, name: r.2, kind: r.3, payload: r.4, created_at: r.5,
        })
        .collect();

    Ok(Json(entities))
}

async fn get_entity(
    State(pool): State<PgPool>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<Json<EntityResponse>, ApiError> {
    let row = sqlx::query_as::<_, (Uuid, String, Option<String>, Option<String>, serde_json::Value, chrono::DateTime<chrono::Utc>)>(
        "SELECT id, uri, name, kind, payload, created_at FROM entities WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&pool)
    .await?
    .ok_or_else(|| ApiError::NotFound(format!("entity {id}")))?;

    Ok(Json(EntityResponse {
        id: row.0, uri: row.1, name: row.2, kind: row.3, payload: row.4, created_at: row.5,
    }))
}
