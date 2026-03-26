use async_trait::async_trait;
use cardinal_harness::rerank::{
    ComparisonEvent, ComparisonObserver, ObserverError, PairwiseJudgement,
};
use sqlx::PgPool;
use uuid::Uuid;

use crate::db;
use crate::posterior::{output_logprobs_json, pairwise_posterior_json};

#[derive(Clone)]
pub struct PgJudgementObserver {
    pool: PgPool,
    user_id: Uuid,
}

impl PgJudgementObserver {
    pub fn new(pool: PgPool, user_id: Uuid) -> Self {
        Self { pool, user_id }
    }
}

#[async_trait]
impl ComparisonObserver for PgJudgementObserver {
    async fn on_comparison(&self, event: ComparisonEvent) -> Result<(), ObserverError> {
        if event.usage.cached {
            return Ok(());
        }

        let raw_entity_a_id = Uuid::parse_str(&event.entity_a_id)
            .map_err(|e| ObserverError::Message(format!("invalid entity_a_id: {e}")))?;
        let raw_entity_b_id = Uuid::parse_str(&event.entity_b_id)
            .map_err(|e| ObserverError::Message(format!("invalid entity_b_id: {e}")))?;

        let (entity_a_id, entity_b_id, flipped) = if raw_entity_a_id < raw_entity_b_id {
            (raw_entity_a_id, raw_entity_b_id, false)
        } else {
            (raw_entity_b_id, raw_entity_a_id, true)
        };

        let attribute_id = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM attributes WHERE slug = $1 OR id::text = $1",
        )
        .bind(&event.attribute_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| ObserverError::Message(format!("attribute lookup failed: {e}")))?;

        let provider = detect_provider(&event.model);
        let rater_id = db::ensure_rater(&self.pool, "model", &event.model, provider)
            .await
            .map_err(|e| ObserverError::Message(format!("ensure_rater failed: {e}")))?;

        let prompt_text = event.usage.prompt_text.clone().ok_or_else(|| {
            ObserverError::Message("missing prompt_text for live comparison".into())
        })?;
        let prompt_hash = blake3::hash(prompt_text.as_bytes());
        let raw_output = event.usage.raw_output.clone().unwrap_or_default();
        let output_logprobs_json = output_logprobs_json(event.usage.output_logprobs.as_deref())
            .map_err(|e| {
                ObserverError::Message(format!("serialize output_logprobs failed: {e}"))
            })?;
        let structured_posterior_json =
            pairwise_posterior_json(event.usage.pairwise_logprob_posterior.as_ref())
                .map_err(|e| ObserverError::Message(format!("serialize posterior failed: {e}")))?;

        let (ln_ratio, confidence, status) = match event.judgement {
            PairwiseJudgement::Refused => (None, None, "refused"),
            PairwiseJudgement::Observation {
                higher_ranked,
                ratio,
                confidence,
            } => {
                let a_is_higher = match higher_ranked {
                    cardinal_harness::rerank::HigherRanked::A => !flipped,
                    cardinal_harness::rerank::HigherRanked::B => flipped,
                };
                let ln_ratio = if a_is_higher {
                    ratio.ln()
                } else {
                    -(ratio.ln())
                };
                (Some(ln_ratio), Some(confidence), "success")
            }
        };

        sqlx::query(
            "INSERT INTO judgements
               (entity_a_id, entity_b_id, attribute_id, rater_id, user_id,
                prompt_text, reasoning_text, raw_output, output_logprobs_json, structured_posterior_json,
                entity_a_text, entity_b_text, question_text,
                ln_ratio, confidence, prompt_hash, status, cache_eligible,
                input_tokens, output_tokens, cost_nanodollars, latency_ms)
             VALUES
               ($1, $2, $3, $4, $5,
                $6, NULL, $7, $8, $9,
                NULL, NULL, $10,
                $11, $12, $13, $14, TRUE,
                $15, $16, $17, NULL)",
        )
        .bind(entity_a_id)
        .bind(entity_b_id)
        .bind(attribute_id)
        .bind(rater_id)
        .bind(self.user_id)
        .bind(&prompt_text)
        .bind(&raw_output)
        .bind(output_logprobs_json)
        .bind(structured_posterior_json)
        .bind(event.usage.question_text.clone())
        .bind(ln_ratio)
        .bind(confidence)
        .bind(prompt_hash.as_bytes().as_slice())
        .bind(status)
        .bind(event.usage.input_tokens as i32)
        .bind(event.usage.output_tokens as i32)
        .bind(Some(event.usage.provider_cost_nanodollars))
        .execute(&self.pool)
        .await
        .map_err(|e| ObserverError::Message(format!("insert judgement failed: {e}")))?;

        if let (Some(ln_ratio), Some(confidence)) = (ln_ratio, confidence) {
            db::upsert_comparison(
                &self.pool,
                entity_a_id,
                entity_b_id,
                attribute_id,
                rater_id,
                ln_ratio,
                confidence,
            )
            .await
            .map_err(|e| ObserverError::Message(format!("upsert_comparison failed: {e}")))?;
        }

        Ok(())
    }
}

fn detect_provider(model_name: &str) -> Option<&'static str> {
    if model_name == "manual" {
        return None;
    }
    if model_name.contains('/') {
        Some("openrouter")
    } else if model_name.starts_with("claude") {
        Some("anthropic")
    } else if model_name.starts_with("gpt")
        || model_name.starts_with("o1")
        || model_name.starts_with("o3")
        || model_name.starts_with("o4")
    {
        Some("openai")
    } else {
        None
    }
}
