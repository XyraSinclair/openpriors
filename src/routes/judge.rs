use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::db;
use crate::error::ApiError;

pub fn routes() -> Router<PgPool> {
    Router::new()
        .route("/judge", post(submit_judgement))
        .route("/judgements", get(list_judgements))
}

// --- Submit a pairwise judgement ---

#[derive(Deserialize)]
struct JudgeRequest {
    /// URI or existing entity for side A.
    entity_a: EntityRef,
    /// URI or existing entity for side B.
    entity_b: EntityRef,
    /// Attribute slug (created on the fly if new).
    attribute: String,
    /// Optional: attribute description for auto-creation.
    attribute_description: Option<String>,
    /// Model name to use as rater. Defaults to server config.
    model: Option<String>,
    /// Pre-computed ln_ratio if the caller already ran the LLM.
    /// When provided, the server caches the result without calling an LLM.
    ln_ratio: Option<f64>,
    /// Confidence [0, 1].
    confidence: Option<f64>,
    /// Full prompt text (for cache).
    prompt_text: Option<String>,
    /// Reasoning trace (for cache).
    reasoning_text: Option<String>,
    /// Raw LLM output (for cache).
    raw_output: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum EntityRef {
    Uri(String),
    Full {
        uri: String,
        name: Option<String>,
        kind: Option<String>,
    },
}

impl EntityRef {
    fn uri(&self) -> &str {
        match self {
            EntityRef::Uri(u) => u,
            EntityRef::Full { uri, .. } => uri,
        }
    }
    fn name(&self) -> Option<&str> {
        match self {
            EntityRef::Uri(_) => None,
            EntityRef::Full { name, .. } => name.as_deref(),
        }
    }
    fn kind(&self) -> Option<&str> {
        match self {
            EntityRef::Uri(_) => None,
            EntityRef::Full { kind, .. } => kind.as_deref(),
        }
    }
}

#[derive(Serialize)]
struct JudgeResponse {
    judgement_id: Uuid,
    comparison_id: Uuid,
    entity_a_id: Uuid,
    entity_b_id: Uuid,
    attribute_id: Uuid,
    rater_id: Uuid,
    ln_ratio: f64,
    confidence: f64,
    cached: bool,
}

async fn submit_judgement(
    State(pool): State<PgPool>,
    Json(body): Json<JudgeRequest>,
) -> Result<Json<JudgeResponse>, ApiError> {
    // Resolve entities (canonical order: a < b by URI)
    let (a_ref, b_ref, flipped) = {
        let a_uri = body.entity_a.uri();
        let b_uri = body.entity_b.uri();
        if a_uri == b_uri {
            return Err(ApiError::BadRequest("entity_a and entity_b must differ".into()));
        }
        if a_uri < b_uri {
            (&body.entity_a, &body.entity_b, false)
        } else {
            (&body.entity_b, &body.entity_a, true)
        }
    };

    let entity_a_id = db::ensure_entity(&pool, a_ref.uri(), a_ref.name(), a_ref.kind()).await?;
    let entity_b_id = db::ensure_entity(&pool, b_ref.uri(), b_ref.name(), b_ref.kind()).await?;

    // Resolve attribute
    let attribute_id = db::ensure_attribute(
        &pool,
        &body.attribute,
        None,
        body.attribute_description.as_deref(),
    )
    .await?;

    // Resolve rater
    let model_name = body.model.as_deref().unwrap_or("manual");
    let rater_kind = if model_name == "manual" { "human" } else { "model" };
    let provider = if model_name.starts_with("claude") {
        Some("anthropic")
    } else if model_name.starts_with("gpt") || model_name.starts_with("o1") || model_name.starts_with("o3") || model_name.starts_with("o4") {
        Some("openai")
    } else {
        None
    };
    let rater_id = db::ensure_rater(&pool, rater_kind, model_name, provider).await?;

    // For now: accept pre-computed judgements.
    // TODO: when ln_ratio is absent, call cardinal-harness to produce the judgement.
    let ln_ratio = body.ln_ratio.ok_or_else(|| {
        ApiError::BadRequest(
            "ln_ratio required (server-side LLM calls not yet wired — submit pre-computed judgements)".into(),
        )
    })?;
    let ln_ratio = if flipped { -ln_ratio } else { ln_ratio };
    let confidence = body.confidence.unwrap_or(0.5);

    // Cache the judgement trace
    let prompt_text = body.prompt_text.as_deref().unwrap_or("");
    let prompt_hash = blake3::hash(prompt_text.as_bytes());
    let raw_output = body.raw_output.as_deref().unwrap_or("");

    let judgement_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO judgements
           (entity_a_id, entity_b_id, attribute_id, rater_id,
            prompt_text, reasoning_text, raw_output,
            ln_ratio, confidence, prompt_hash)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         RETURNING id",
    )
    .bind(entity_a_id)
    .bind(entity_b_id)
    .bind(attribute_id)
    .bind(rater_id)
    .bind(prompt_text)
    .bind(body.reasoning_text.as_deref())
    .bind(raw_output)
    .bind(ln_ratio)
    .bind(confidence)
    .bind(prompt_hash.as_bytes().as_slice())
    .fetch_one(&pool)
    .await?;

    // Aggregate into comparisons
    let comparison_id = db::upsert_comparison(
        &pool,
        entity_a_id,
        entity_b_id,
        attribute_id,
        rater_id,
        ln_ratio,
        confidence,
    )
    .await?;

    Ok(Json(JudgeResponse {
        judgement_id,
        comparison_id,
        entity_a_id,
        entity_b_id,
        attribute_id,
        rater_id,
        ln_ratio,
        confidence,
        cached: false,
    }))
}

// --- List judgements ---

#[derive(Serialize)]
struct JudgementRow {
    id: Uuid,
    entity_a_id: Uuid,
    entity_b_id: Uuid,
    attribute_id: Uuid,
    rater_id: Uuid,
    ln_ratio: f64,
    confidence: f64,
    reasoning_text: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

async fn list_judgements(
    State(pool): State<PgPool>,
    axum::extract::Query(params): axum::extract::Query<ListJudgementsParams>,
) -> Result<Json<Vec<JudgementRow>>, ApiError> {
    let limit = params.limit.unwrap_or(100).min(1000);

    let rows = sqlx::query_as::<_, (Uuid, Uuid, Uuid, Uuid, Uuid, f64, f64, Option<String>, chrono::DateTime<chrono::Utc>)>(
        "SELECT id, entity_a_id, entity_b_id, attribute_id, rater_id,
                ln_ratio, confidence, reasoning_text, created_at
         FROM judgements
         WHERE ($1::uuid IS NULL OR attribute_id = $1)
         ORDER BY created_at DESC
         LIMIT $2",
    )
    .bind(params.attribute_id)
    .bind(limit)
    .fetch_all(&pool)
    .await?;

    let judgements: Vec<_> = rows
        .into_iter()
        .map(|r| JudgementRow {
            id: r.0,
            entity_a_id: r.1,
            entity_b_id: r.2,
            attribute_id: r.3,
            rater_id: r.4,
            ln_ratio: r.5,
            confidence: r.6,
            reasoning_text: r.7,
            created_at: r.8,
        })
        .collect();

    Ok(Json(judgements))
}

#[derive(Deserialize)]
struct ListJudgementsParams {
    attribute_id: Option<Uuid>,
    limit: Option<i64>,
}
