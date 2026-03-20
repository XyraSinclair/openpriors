use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use cardinal_harness::gateway::{Attribution, ChatModel, ChatRequest, Message};
use cardinal_harness::ChatGateway;

use crate::auth::{AppState, MaybeAuth};
use crate::db;
use crate::error::ApiError;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/judge", post(submit_judgement))
        .route("/judgements", get(list_judgements))
}

// --- Submit a pairwise judgement ---

#[derive(Deserialize)]
struct JudgeRequest {
    entity_a: EntityRef,
    entity_b: EntityRef,
    attribute: String,
    attribute_description: Option<String>,
    model: Option<String>,
    /// Pre-computed ln_ratio. When absent, server calls LLM (requires auth + credits).
    ln_ratio: Option<f64>,
    confidence: Option<f64>,
    prompt_text: Option<String>,
    reasoning_text: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost_nanodollars: Option<i64>,
}

async fn submit_judgement(
    State(state): State<Arc<AppState>>,
    auth: MaybeAuth,
    Json(body): Json<JudgeRequest>,
) -> Result<Json<JudgeResponse>, ApiError> {
    let pool = &state.db;

    // Resolve entities first (need UUIDs for canonical ordering)
    let a_uri = body.entity_a.uri();
    let b_uri = body.entity_b.uri();
    if a_uri == b_uri {
        return Err(ApiError::BadRequest("entity_a and entity_b must differ".into()));
    }

    let raw_a_id = db::ensure_entity(pool, a_uri, body.entity_a.name(), body.entity_a.kind()).await?;
    let raw_b_id = db::ensure_entity(pool, b_uri, body.entity_b.name(), body.entity_b.kind()).await?;

    // Phase 0a fix: canonical order by UUID, not URI
    let (entity_a_id, entity_b_id, flipped) = if raw_a_id < raw_b_id {
        (raw_a_id, raw_b_id, false)
    } else {
        (raw_b_id, raw_a_id, true)
    };

    // Resolve attribute
    let attribute_id = db::ensure_attribute(
        pool,
        &body.attribute,
        None,
        body.attribute_description.as_deref(),
    )
    .await?;

    // Resolve rater
    let model_name = body.model.as_deref().unwrap_or("manual");
    let rater_kind = if model_name == "manual" { "human" } else { "model" };
    let provider = detect_provider(model_name);
    let rater_id = db::ensure_rater(pool, rater_kind, model_name, provider).await?;

    if let Some(pre_ln_ratio) = body.ln_ratio {
        // Pre-computed judgement path
        let ln_ratio = if flipped { -pre_ln_ratio } else { pre_ln_ratio };
        let confidence = body.confidence.unwrap_or(0.5);

        let prompt_text = body.prompt_text.as_deref().unwrap_or("");
        let prompt_hash = blake3::hash(prompt_text.as_bytes());
        let raw_output = body.raw_output.as_deref().unwrap_or("");

        // Phase 0b fix: check cache before inserting
        let cached = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM judgements
             WHERE prompt_hash = $1 AND entity_a_id = $2 AND entity_b_id = $3 AND attribute_id = $4
             AND status = 'success'
             LIMIT 1",
        )
        .bind(prompt_hash.as_bytes().as_slice())
        .bind(entity_a_id)
        .bind(entity_b_id)
        .bind(attribute_id)
        .fetch_optional(pool)
        .await?;

        if let Some(existing_id) = cached {
            // Return cached result
            let row = sqlx::query_as::<_, (f64, f64, Uuid)>(
                "SELECT ln_ratio, confidence, rater_id FROM judgements WHERE id = $1",
            )
            .bind(existing_id)
            .fetch_one(pool)
            .await?;

            // Find or create comparison
            let comparison_id = db::upsert_comparison(
                pool, entity_a_id, entity_b_id, attribute_id, row.2, row.0, row.1,
            )
            .await?;

            return Ok(Json(JudgeResponse {
                judgement_id: existing_id,
                comparison_id,
                entity_a_id,
                entity_b_id,
                attribute_id,
                rater_id: row.2,
                ln_ratio: row.0,
                confidence: row.1,
                cached: true,
                reasoning_text: None,
                cost_nanodollars: None,
            }));
        }

        let judgement_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO judgements
               (entity_a_id, entity_b_id, attribute_id, rater_id,
                prompt_text, reasoning_text, raw_output,
                ln_ratio, confidence, prompt_hash, status)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'success')
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
        .fetch_one(pool)
        .await?;

        let comparison_id = db::upsert_comparison(
            pool, entity_a_id, entity_b_id, attribute_id, rater_id, ln_ratio, confidence,
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
            reasoning_text: None,
            cost_nanodollars: None,
        }))
    } else {
        // Server-side LLM dispatch — requires auth
        let auth_user = auth.0.ok_or_else(|| {
            ApiError::Unauthorized(
                "authentication required for server-side LLM calls".into(),
            )
        })?;

        // Get entity text
        let entity_a_text = get_entity_text(pool, entity_a_id).await?;
        let entity_b_text = get_entity_text(pool, entity_b_id).await?;

        // Get attribute description for prompt
        let attr_desc = sqlx::query_scalar::<_, Option<String>>(
            "SELECT description FROM attributes WHERE id = $1",
        )
        .bind(attribute_id)
        .fetch_one(pool)
        .await?
        .unwrap_or_else(|| body.attribute.clone());

        // Build prompt
        let (a_label, b_label) = if flipped {
            (&entity_b_text, &entity_a_text)
        } else {
            (&entity_a_text, &entity_b_text)
        };

        let system_prompt = format!(
            "You are an expert evaluator. Compare two entities on the attribute: {attr_desc}\n\n\
             Output a JSON object with exactly these fields:\n\
             - \"higher_ranked\": \"A\" or \"B\" (which entity has MORE of this attribute)\n\
             - \"ratio\": a number from [1.0, 1.05, 1.1, 1.2, 1.3, 1.5, 1.75, 2.1, 2.5, 3.1, 3.9, 5.1, 6.8, 9.2, 12.7, 18.0, 26.0] \
               representing how many times more of the attribute the higher-ranked entity has\n\
             - \"confidence\": a number from 0.5 to 1.0 representing your confidence\n\n\
             If you cannot judge, output: {{\"refused\": true, \"reason\": \"...\"}}\n\n\
             Output only valid JSON, no other text."
        );

        let user_prompt = format!(
            "<entity_A>\n{a_label}\n</entity_A>\n\n<entity_B>\n{b_label}\n</entity_B>"
        );

        let prompt_text = format!("{system_prompt}\n---\n{user_prompt}");
        let prompt_hash = blake3::hash(prompt_text.as_bytes());

        // Check cache
        let cached = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM judgements
             WHERE prompt_hash = $1 AND entity_a_id = $2 AND entity_b_id = $3 AND attribute_id = $4
             AND status IN ('success', 'refused')
             LIMIT 1",
        )
        .bind(prompt_hash.as_bytes().as_slice())
        .bind(entity_a_id)
        .bind(entity_b_id)
        .bind(attribute_id)
        .fetch_optional(pool)
        .await?;

        if let Some(existing_id) = cached {
            let row = sqlx::query_as::<_, (Option<f64>, f64, Uuid, Option<String>, String)>(
                "SELECT ln_ratio, confidence, rater_id, reasoning_text, status FROM judgements WHERE id = $1",
            )
            .bind(existing_id)
            .fetch_one(pool)
            .await?;

            if row.4 == "refused" {
                return Err(ApiError::BadRequest("LLM refused to judge this comparison".into()));
            }

            let ln_ratio = row.0.unwrap_or(0.0);
            let comparison_id = db::upsert_comparison(
                pool, entity_a_id, entity_b_id, attribute_id, row.2, ln_ratio, row.1,
            )
            .await?;

            return Ok(Json(JudgeResponse {
                judgement_id: existing_id,
                comparison_id,
                entity_a_id,
                entity_b_id,
                attribute_id,
                rater_id: row.2,
                ln_ratio,
                confidence: row.1,
                cached: true,
                reasoning_text: row.3,
                cost_nanodollars: None,
            }));
        }

        // Call LLM
        let model_id = body.model.as_deref().unwrap_or(&state.config.default_model);

        // Create metering gateway for this user
        let metering = crate::metering::MeteringGateway::new(
            state.gateway.clone(),
            state.db.clone(),
            auth_user.user_id,
        );

        let attribution = Attribution::new("openpriors::judge")
            .with_user(auth_user.user_id);

        let chat_req = ChatRequest::new(
            ChatModel::openrouter(model_id),
            vec![
                Message::system(system_prompt),
                Message::user(user_prompt),
            ],
            attribution,
        )
        .temperature(0.0)
        .max_tokens(512)
        .json();

        let start = std::time::Instant::now();
        let chat_resp = metering.chat(chat_req).await.map_err(|e| {
            ApiError::Internal(format!("LLM call failed: {e}"))
        })?;
        let latency_ms = start.elapsed().as_millis() as i32;

        // Parse LLM response
        let raw_output = &chat_resp.content;
        let (ln_ratio, confidence, status, reasoning) = parse_llm_response(raw_output, flipped)?;

        // Resolve rater for the actual model used
        let rater_id = db::ensure_rater(pool, "model", model_id, Some("openrouter")).await?;

        let judgement_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO judgements
               (entity_a_id, entity_b_id, attribute_id, rater_id, user_id,
                prompt_text, reasoning_text, raw_output,
                entity_a_text, entity_b_text, question_text,
                ln_ratio, confidence, prompt_hash, status,
                input_tokens, output_tokens, cost_nanodollars, latency_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19)
             RETURNING id",
        )
        .bind(entity_a_id)
        .bind(entity_b_id)
        .bind(attribute_id)
        .bind(rater_id)
        .bind(auth_user.user_id)
        .bind(&prompt_text)
        .bind(&reasoning)
        .bind(raw_output)
        .bind(&entity_a_text)
        .bind(&entity_b_text)
        .bind(&attr_desc)
        .bind(ln_ratio)
        .bind(confidence)
        .bind(prompt_hash.as_bytes().as_slice())
        .bind(&status)
        .bind(chat_resp.input_tokens as i32)
        .bind(chat_resp.output_tokens as i32)
        .bind(chat_resp.cost_nanodollars)
        .bind(latency_ms)
        .fetch_one(pool)
        .await?;

        if status == "refused" {
            return Err(ApiError::BadRequest("LLM refused to judge this comparison".into()));
        }

        let ln_ratio = ln_ratio.unwrap_or(0.0);
        let confidence = confidence.unwrap_or(0.5);

        let comparison_id = db::upsert_comparison(
            pool, entity_a_id, entity_b_id, attribute_id, rater_id, ln_ratio, confidence,
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
            reasoning_text: reasoning,
            cost_nanodollars: Some(chat_resp.cost_nanodollars),
        }))
    }
}

/// Parse the LLM JSON response into (ln_ratio, confidence, status, reasoning).
fn parse_llm_response(
    raw: &str,
    flipped: bool,
) -> Result<(Option<f64>, Option<f64>, String, Option<String>), ApiError> {
    let parsed: serde_json::Value = serde_json::from_str(raw.trim())
        .map_err(|e| ApiError::Internal(format!("failed to parse LLM output: {e}")))?;

    if parsed.get("refused").and_then(|v| v.as_bool()).unwrap_or(false) {
        let reason = parsed.get("reason").and_then(|v| v.as_str()).map(|s| s.to_string());
        return Ok((None, None, "refused".to_string(), reason));
    }

    let higher = parsed.get("higher_ranked")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::Internal("missing higher_ranked in LLM output".into()))?;

    let ratio = parsed.get("ratio")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| ApiError::Internal("missing ratio in LLM output".into()))?;

    let confidence = parsed.get("confidence")
        .and_then(|v| v.as_f64());

    // Convert to ln_ratio in canonical (a < b) order
    let a_is_higher = match higher {
        "A" => !flipped,
        "B" => flipped,
        _ => return Err(ApiError::Internal(format!("invalid higher_ranked: {higher}"))),
    };

    let ln_ratio = if a_is_higher {
        ratio.ln()
    } else {
        -(ratio.ln())
    };

    Ok((
        Some(ln_ratio),
        confidence,
        "success".to_string(),
        None, // reasoning is in the raw output
    ))
}

async fn get_entity_text(pool: &sqlx::PgPool, entity_id: Uuid) -> Result<String, ApiError> {
    let row = sqlx::query_as::<_, (String, Option<String>, serde_json::Value)>(
        "SELECT uri, name, payload FROM entities WHERE id = $1",
    )
    .bind(entity_id)
    .fetch_one(pool)
    .await
    .map_err(ApiError::Db)?;

    // Use name if available, else try payload.text, else URI
    if let Some(name) = &row.1 {
        if !name.is_empty() {
            // If there's payload text, combine name + text
            if let Some(text) = row.2.get("text").and_then(|v| v.as_str()) {
                return Ok(format!("{name}\n\n{text}"));
            }
            return Ok(name.clone());
        }
    }

    if let Some(text) = row.2.get("text").and_then(|v| v.as_str()) {
        return Ok(text.to_string());
    }

    Ok(row.0) // fallback to URI
}

fn detect_provider(model_name: &str) -> Option<&'static str> {
    if model_name == "manual" {
        return None;
    }
    // OpenRouter model IDs have format "provider/model"
    if model_name.contains('/') {
        Some("openrouter")
    } else if model_name.starts_with("claude") {
        Some("anthropic")
    } else if model_name.starts_with("gpt") || model_name.starts_with("o1") || model_name.starts_with("o3") || model_name.starts_with("o4") {
        Some("openai")
    } else {
        None
    }
}

// --- List judgements ---

#[derive(Serialize)]
struct JudgementRow {
    id: Uuid,
    entity_a_id: Uuid,
    entity_b_id: Uuid,
    attribute_id: Uuid,
    rater_id: Uuid,
    ln_ratio: Option<f64>,
    confidence: f64,
    status: String,
    reasoning_text: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

async fn list_judgements(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<ListJudgementsParams>,
) -> Result<Json<Vec<JudgementRow>>, ApiError> {
    let limit = params.limit.unwrap_or(100).min(1000);

    let rows = sqlx::query_as::<_, (Uuid, Uuid, Uuid, Uuid, Uuid, Option<f64>, f64, String, Option<String>, chrono::DateTime<chrono::Utc>)>(
        "SELECT id, entity_a_id, entity_b_id, attribute_id, rater_id,
                ln_ratio, confidence, status, reasoning_text, created_at
         FROM judgements
         WHERE ($1::uuid IS NULL OR attribute_id = $1)
         ORDER BY created_at DESC
         LIMIT $2",
    )
    .bind(params.attribute_id)
    .bind(limit)
    .fetch_all(&state.db)
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
            status: r.7,
            reasoning_text: r.8,
            created_at: r.9,
        })
        .collect();

    Ok(Json(judgements))
}

#[derive(Deserialize)]
struct ListJudgementsParams {
    attribute_id: Option<Uuid>,
    limit: Option<i64>,
}
