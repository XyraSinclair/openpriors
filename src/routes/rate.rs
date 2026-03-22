use axum::{extract::State, routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;

use cardinal_harness::gateway::Attribution;
use cardinal_harness::rerank::{
    estimate_max_rerank_charge, multi_rerank_with_trace, MultiRerankAttributeSpec,
    MultiRerankEntity, MultiRerankRequest, MultiRerankTopKSpec,
};

use crate::auth::{AppState, AuthUser};
use crate::error::ApiError;
use crate::pg_cache::PgPairwiseCache;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/rate", post(rate))
}

#[derive(Deserialize)]
struct RateRequest {
    /// Entity URIs or IDs to rate.
    entities: Vec<EntityInput>,
    /// Attribute slug.
    attribute: String,
    /// Model to use (default: server config).
    model: Option<String>,
    /// Maximum comparisons to run.
    comparison_budget: Option<usize>,
    /// Top-K to focus on (default: all entities).
    top_k: Option<usize>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum EntityInput {
    Uri(String),
    Full {
        uri: String,
        name: Option<String>,
        kind: Option<String>,
        text: Option<String>,
    },
}

impl EntityInput {
    fn uri(&self) -> &str {
        match self {
            EntityInput::Uri(u) => u,
            EntityInput::Full { uri, .. } => uri,
        }
    }
}

#[derive(Serialize)]
struct RateResponse {
    attribute: String,
    entities: Vec<RatedEntity>,
    meta: RateMeta,
}

#[derive(Serialize)]
struct RatedEntity {
    rank: usize,
    entity_id: Uuid,
    entity_uri: String,
    entity_name: Option<String>,
    score: f64,
    uncertainty: f64,
    comparison_count: i32,
}

#[derive(Serialize)]
struct RateMeta {
    comparisons_used: usize,
    comparisons_refused: usize,
    provider_cost_nanodollars: i64,
    user_charge_nanodollars: i64,
    model_used: String,
    stop_reason: String,
    latency_ms: u128,
}

async fn rate(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Json(body): Json<RateRequest>,
) -> Result<Json<RateResponse>, ApiError> {
    auth.require_scope("rate:run")?;

    let pool = &state.db;

    if body.entities.len() < 2 {
        return Err(ApiError::BadRequest("need at least 2 entities".into()));
    }
    if body.entities.len() > 5000 {
        return Err(ApiError::BadRequest("max 5000 entities".into()));
    }
    if matches!(body.top_k, Some(0)) {
        return Err(ApiError::BadRequest("top_k must be at least 1".into()));
    }
    if matches!(body.comparison_budget, Some(0)) {
        return Err(ApiError::BadRequest(
            "comparison_budget must be at least 1".into(),
        ));
    }

    // Resolve attribute
    let attr_row = sqlx::query_as::<_, (Uuid, String, Option<String>)>(
        "SELECT id, name, description FROM attributes WHERE slug = $1",
    )
    .bind(&body.attribute)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::NotFound(format!("attribute {}", body.attribute)))?;

    let (attribute_id, attr_name, attr_desc) = attr_row;
    let attr_prompt = attr_desc.unwrap_or_else(|| attr_name.clone());

    // Resolve entities and get their text
    let mut entity_ids: Vec<Uuid> = Vec::with_capacity(body.entities.len());
    let mut entity_texts: Vec<(String, String, Option<String>)> =
        Vec::with_capacity(body.entities.len());
    let mut seen_uris = HashSet::with_capacity(body.entities.len());

    for input in &body.entities {
        let (name, kind, text_override) = match input {
            EntityInput::Uri(_) => (None, None, None),
            EntityInput::Full {
                name, kind, text, ..
            } => (name.as_deref(), kind.as_deref(), text.clone()),
        };

        let uri = input.uri().trim();
        if uri.is_empty() {
            return Err(ApiError::BadRequest("entity URI is required".into()));
        }
        if uri.len() > 2048 {
            return Err(ApiError::BadRequest("entity URI is too long".into()));
        }
        if !seen_uris.insert(uri.to_string()) {
            return Err(ApiError::BadRequest(format!("duplicate entity URI {uri}")));
        }
        if let Some(name) = name {
            if name.len() > 256 {
                return Err(ApiError::BadRequest("entity name is too long".into()));
            }
        }
        if let Some(kind) = kind {
            if kind.len() > 64 {
                return Err(ApiError::BadRequest("entity kind is too long".into()));
            }
        }
        if let Some(text) = text_override.as_deref() {
            if text.len() > 64 * 1024 {
                return Err(ApiError::BadRequest(
                    "entity text override is too large".into(),
                ));
            }
        }

        let entity_id = crate::db::ensure_entity(pool, uri, name, kind).await?;
        entity_ids.push(entity_id);

        // Get text for this entity
        let text = if let Some(t) = text_override {
            t
        } else {
            get_entity_text(pool, entity_id).await?
        };

        let entity_name =
            sqlx::query_scalar::<_, Option<String>>("SELECT name FROM entities WHERE id = $1")
                .bind(entity_id)
                .fetch_one(pool)
                .await?;

        entity_texts.push((uri.to_string(), text, entity_name));
    }

    // Build MultiRerankRequest
    let model = body.model.as_deref().unwrap_or(&state.config.default_model);
    let n = entity_ids.len();
    let top_k = body.top_k.unwrap_or(n);

    let rerank_entities: Vec<MultiRerankEntity> = entity_ids
        .iter()
        .zip(entity_texts.iter())
        .map(|(id, (_, text, _))| MultiRerankEntity {
            id: id.to_string(),
            text: text.clone(),
        })
        .collect();

    let rerank_req = MultiRerankRequest {
        entities: rerank_entities,
        attributes: vec![MultiRerankAttributeSpec {
            id: body.attribute.clone(),
            prompt: attr_prompt,
            prompt_template_slug: None,
            weight: 1.0,
        }],
        topk: MultiRerankTopKSpec {
            k: top_k.min(n),
            weight_exponent: 1.3,
            tolerated_error: 0.1,
            band_size: 5,
            effective_resistance_max_active: 64,
            stop_sigma_inflate: 1.25,
            stop_min_consecutive: 2,
        },
        gates: vec![],
        comparison_budget: body.comparison_budget,
        latency_budget_ms: None,
        model: Some(model.to_string()),
        rater_id: None,
        comparison_concurrency: Some(8),
        max_pair_repeats: None,
    };

    // Estimate cost and pre-check credits
    let estimate = estimate_max_rerank_charge(&rerank_req);
    let balance = crate::credits::get_balance(pool, auth.user_id).await?;
    if balance < estimate.user_charge_max_nanodollars {
        return Err(ApiError::Forbidden(format!(
            "insufficient credits: need up to ${:.4}, have ${:.4}",
            estimate.user_charge_max_nanodollars as f64 / 1e9,
            balance as f64 / 1e9,
        )));
    }

    // Build metering gateway
    let metering = Arc::new(crate::metering::MeteringGateway::new(
        state.gateway.clone(),
        state.db.clone(),
        auth.user_id,
    ));

    let pg_cache = PgPairwiseCache::new(state.db.clone());

    let attribution = Attribution::new("openpriors::rate").with_user(auth.user_id);

    // Run the reranker
    let result = multi_rerank_with_trace(
        metering as Arc<dyn cardinal_harness::ChatGateway>,
        Some(&pg_cache as &dyn cardinal_harness::PairwiseCache),
        None, // model_policy
        None, // run_options
        rerank_req,
        attribution,
        None, // warm_start
        None, // observer
        None, // trace
        None, // cancel_flag
    )
    .await
    .map_err(|e| ApiError::Internal(format!("rating failed: {e}")))?;

    // Write scores to DB
    let mut tx = pool.begin().await?;

    sqlx::query("DELETE FROM scores WHERE attribute_id = $1")
        .bind(attribute_id)
        .execute(&mut *tx)
        .await?;

    // Build ranked list from rerank results
    let mut ranked: Vec<_> = result.entities.iter().collect();
    ranked.sort_by(|a, b| {
        b.u_mean
            .partial_cmp(&a.u_mean)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    for (rank_idx, entity_result) in ranked.iter().enumerate() {
        let entity_id = Uuid::parse_str(&entity_result.id)
            .map_err(|e| ApiError::Internal(format!("invalid entity UUID: {e}")))?;

        let score = entity_result.u_mean;
        let uncertainty = entity_result.u_std;

        // Count comparisons for this entity
        let comparison_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM comparisons
             WHERE attribute_id = $1 AND (entity_a_id = $2 OR entity_b_id = $2)",
        )
        .bind(attribute_id)
        .bind(entity_id)
        .fetch_one(&mut *tx)
        .await? as i32;

        sqlx::query(
            "INSERT INTO scores (entity_id, attribute_id, score, uncertainty, comparison_count)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (entity_id, attribute_id) DO UPDATE SET
               score = EXCLUDED.score,
               uncertainty = EXCLUDED.uncertainty,
               comparison_count = EXCLUDED.comparison_count,
               solved_at = now()",
        )
        .bind(entity_id)
        .bind(attribute_id)
        .bind(score)
        .bind(uncertainty)
        .bind(comparison_count)
        .execute(&mut *tx)
        .await?;

        let _ = rank_idx; // used below
    }

    tx.commit().await?;

    // Build response
    let response_entities: Vec<RatedEntity> = ranked
        .iter()
        .enumerate()
        .map(|(rank_idx, entity_result)| {
            let entity_id = Uuid::parse_str(&entity_result.id).unwrap();
            let idx = entity_ids.iter().position(|id| *id == entity_id).unwrap();
            let (uri, _, name) = &entity_texts[idx];

            let comparison_count = result.meta.comparisons_used as i32; // approximate per-entity

            RatedEntity {
                rank: rank_idx + 1,
                entity_id,
                entity_uri: uri.clone(),
                entity_name: name.clone(),
                score: entity_result.u_mean,
                uncertainty: entity_result.u_std,
                comparison_count,
            }
        })
        .collect();

    Ok(Json(RateResponse {
        attribute: body.attribute,
        entities: response_entities,
        meta: RateMeta {
            comparisons_used: result.meta.comparisons_used,
            comparisons_refused: result.meta.comparisons_refused,
            provider_cost_nanodollars: result.meta.provider_cost_nanodollars,
            user_charge_nanodollars: cardinal_harness::rerank::apply_rerank_markup(
                result.meta.provider_cost_nanodollars,
            ),
            model_used: result.meta.model_used,
            stop_reason: format!("{:?}", result.meta.stop_reason),
            latency_ms: result.meta.latency_ms,
        },
    }))
}

async fn get_entity_text(pool: &sqlx::PgPool, entity_id: Uuid) -> Result<String, ApiError> {
    let row = sqlx::query_as::<_, (String, Option<String>, serde_json::Value)>(
        "SELECT uri, name, payload FROM entities WHERE id = $1",
    )
    .bind(entity_id)
    .fetch_one(pool)
    .await
    .map_err(ApiError::Db)?;

    if let Some(name) = &row.1 {
        if !name.is_empty() {
            if let Some(text) = row.2.get("text").and_then(|v| v.as_str()) {
                return Ok(format!("{name}\n\n{text}"));
            }
            return Ok(name.clone());
        }
    }

    if let Some(text) = row.2.get("text").and_then(|v| v.as_str()) {
        return Ok(text.to_string());
    }

    Ok(row.0)
}
