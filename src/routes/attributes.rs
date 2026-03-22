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
        .route("/attributes", post(create_attribute).get(list_attributes))
        .route("/attributes/{slug}", get(get_attribute))
}

#[derive(Deserialize)]
struct CreateAttribute {
    slug: String,
    name: Option<String>,
    description: Option<String>,
    prompt_template: Option<String>,
}

#[derive(Serialize)]
struct AttributeResponse {
    id: Uuid,
    slug: String,
    name: String,
    description: Option<String>,
    prompt_template: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

async fn create_attribute(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Json(body): Json<CreateAttribute>,
) -> Result<Json<AttributeResponse>, ApiError> {
    auth.require_admin()?;
    auth.require_scope("attributes:write")?;

    let slug = body.slug.trim();
    if !is_valid_slug(slug) {
        return Err(ApiError::BadRequest(
            "attribute slug must use lowercase letters, numbers, '-' or '_'".into(),
        ));
    }
    if slug.len() > 128 {
        return Err(ApiError::BadRequest("attribute slug is too long".into()));
    }

    let display_name = body.name.as_deref().unwrap_or(slug);
    if display_name.len() > 256 {
        return Err(ApiError::BadRequest("attribute name is too long".into()));
    }
    if let Some(description) = body.description.as_deref() {
        if description.len() > 8192 {
            return Err(ApiError::BadRequest(
                "attribute description is too long".into(),
            ));
        }
    }
    if let Some(prompt_template) = body.prompt_template.as_deref() {
        if prompt_template.len() > 16 * 1024 {
            return Err(ApiError::BadRequest("prompt template is too long".into()));
        }
    }

    let row = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            String,
            Option<String>,
            Option<String>,
            chrono::DateTime<chrono::Utc>,
        ),
    >(
        "INSERT INTO attributes (slug, name, description, prompt_template)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (slug) DO UPDATE SET
           name = COALESCE(EXCLUDED.name, attributes.name),
           description = COALESCE(EXCLUDED.description, attributes.description),
           prompt_template = COALESCE(EXCLUDED.prompt_template, attributes.prompt_template)
         RETURNING id, slug, name, description, prompt_template, created_at",
    )
    .bind(slug)
    .bind(display_name)
    .bind(&body.description)
    .bind(&body.prompt_template)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(AttributeResponse {
        id: row.0,
        slug: row.1,
        name: row.2,
        description: row.3,
        prompt_template: row.4,
        created_at: row.5,
    }))
}

#[derive(Deserialize)]
struct ListParams {
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn list_attributes(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListParams>,
) -> Result<Json<Vec<AttributeResponse>>, ApiError> {
    let limit = params.limit.unwrap_or(100).clamp(1, 1000);
    let offset = params.offset.unwrap_or(0).max(0);

    let rows = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            String,
            Option<String>,
            Option<String>,
            chrono::DateTime<chrono::Utc>,
        ),
    >(
        "SELECT id, slug, name, description, prompt_template, created_at FROM attributes
         ORDER BY created_at DESC LIMIT $1 OFFSET $2",
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let attrs: Vec<_> = rows
        .into_iter()
        .map(|r| AttributeResponse {
            id: r.0,
            slug: r.1,
            name: r.2,
            description: r.3,
            prompt_template: r.4,
            created_at: r.5,
        })
        .collect();

    Ok(Json(attrs))
}

async fn get_attribute(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(slug): axum::extract::Path<String>,
) -> Result<Json<AttributeResponse>, ApiError> {
    let row = sqlx::query_as::<_, (Uuid, String, String, Option<String>, Option<String>, chrono::DateTime<chrono::Utc>)>(
        "SELECT id, slug, name, description, prompt_template, created_at FROM attributes WHERE slug = $1",
    )
    .bind(&slug)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::NotFound(format!("attribute {slug}")))?;

    Ok(Json(AttributeResponse {
        id: row.0,
        slug: row.1,
        name: row.2,
        description: row.3,
        prompt_template: row.4,
        created_at: row.5,
    }))
}

fn is_valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
}
