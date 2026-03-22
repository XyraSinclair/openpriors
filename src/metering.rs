use async_trait::async_trait;
use cardinal_harness::gateway::{ChatRequest, ChatResponse, ProviderError};
use cardinal_harness::ChatGateway;
use sqlx::PgPool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use uuid::Uuid;

/// Gateway wrapper that burns credits on each successful LLM call.
///
/// Applies a 20% markup on provider cost. Uses an atomic counter
/// to generate unique idempotency keys per session.
pub struct MeteringGateway {
    inner: Arc<dyn ChatGateway>,
    pool: PgPool,
    user_id: Uuid,
    session_id: Uuid,
    call_counter: AtomicU64,
    markup_numerator: i64,
    markup_denominator: i64,
}

impl MeteringGateway {
    pub fn new(inner: Arc<dyn ChatGateway>, pool: PgPool, user_id: Uuid) -> Self {
        Self {
            inner,
            pool,
            user_id,
            session_id: Uuid::new_v4(),
            call_counter: AtomicU64::new(0),
            markup_numerator: 6,
            markup_denominator: 5,
        }
    }

    fn apply_markup(&self, provider_cost: i64) -> i64 {
        if provider_cost <= 0 {
            return 0;
        }
        (provider_cost.saturating_mul(self.markup_numerator) + (self.markup_denominator - 1))
            / self.markup_denominator
    }
}

#[async_trait]
impl ChatGateway for MeteringGateway {
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let balance = crate::credits::get_balance(&self.pool, self.user_id)
            .await
            .map_err(|e| ProviderError::provider("metering", e.to_string(), false))?;
        if balance <= 0 {
            return Err(ProviderError::provider(
                "metering",
                "insufficient credits",
                false,
            ));
        }

        let resp = self.inner.chat(req).await?;

        let marked_up_cost = self.apply_markup(resp.cost_nanodollars);

        if marked_up_cost > 0 {
            let call_num = self.call_counter.fetch_add(1, Ordering::Relaxed);
            let idempotency_key = format!("llm:{}:{}", self.session_id, call_num);
            let related_object = format!("llm_session:{}:{}", self.session_id, call_num);

            if let Err(e) = crate::credits::burn_credits(
                &self.pool,
                self.user_id,
                marked_up_cost,
                &idempotency_key,
                Some(&related_object),
                Some("llm call"),
            )
            .await
            {
                tracing::warn!(
                    user_id = %self.user_id,
                    cost = marked_up_cost,
                    "failed to burn credits: {e}"
                );
                return Err(ProviderError::provider(
                    "metering",
                    format!("credit settlement failed: {e}"),
                    false,
                ));
            }
        }

        Ok(resp)
    }
}
