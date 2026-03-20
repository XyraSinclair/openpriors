use async_trait::async_trait;
use cardinal_harness::cache::{CacheError, CachedJudgement, PairwiseCache, PairwiseCacheKey};
use sqlx::PgPool;

/// PostgreSQL-backed implementation of cardinal-harness PairwiseCache.
///
/// Maps cache lookups to the `judgements` table using prompt_hash as the key.
#[derive(Clone)]
pub struct PgPairwiseCache {
    pool: PgPool,
}

impl PgPairwiseCache {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl PairwiseCache for PgPairwiseCache {
    async fn get(&self, key: &PairwiseCacheKey) -> Result<Option<CachedJudgement>, CacheError> {
        let key_hash_bytes = hex::decode(&key.key_hash)
            .map_err(|e| CacheError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;

        let row = sqlx::query_as::<_, (Option<f64>, Option<f64>, String, Option<i32>, Option<i32>, Option<i64>)>(
            "SELECT ln_ratio, confidence, status, input_tokens, output_tokens, cost_nanodollars
             FROM judgements
             WHERE prompt_hash = $1
             AND entity_a_id::text = $2
             AND entity_b_id::text = $3
             AND attribute_id IN (SELECT id FROM attributes WHERE slug = $4 OR id::text = $4)
             AND status IN ('success', 'refused')
             ORDER BY created_at DESC
             LIMIT 1",
        )
        .bind(&key_hash_bytes)
        .bind(&key.entity_a_id)
        .bind(&key.entity_b_id)
        .bind(&key.attribute_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| CacheError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;

        match row {
            None => Ok(None),
            Some((ln_ratio, confidence, status, input_tokens, output_tokens, cost)) => {
                let refused = status == "refused";
                let (higher_ranked, ratio) = if refused {
                    (None, None)
                } else if let Some(lr) = ln_ratio {
                    if lr >= 0.0 {
                        (Some("A".to_string()), Some(lr.exp()))
                    } else {
                        (Some("B".to_string()), Some((-lr).exp()))
                    }
                } else {
                    (None, None)
                };

                Ok(Some(CachedJudgement {
                    higher_ranked,
                    ratio,
                    confidence,
                    refused,
                    input_tokens: input_tokens.map(|v| v as u32),
                    output_tokens: output_tokens.map(|v| v as u32),
                    provider_cost_nanodollars: cost,
                }))
            }
        }
    }

    async fn put(&self, _key: &PairwiseCacheKey, _value: &CachedJudgement) -> Result<(), CacheError> {
        // Writes happen via the judge endpoint, not through the cache trait.
        // The multi_rerank_with_trace loop calls put() after each comparison,
        // but we handle persistence in the ComparisonObserver instead.
        Ok(())
    }
}

mod hex {
    pub fn decode(s: &str) -> Result<Vec<u8>, HexError> {
        if s.len() % 2 != 0 {
            return Err(HexError);
        }
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| HexError))
            .collect()
    }

    #[derive(Debug)]
    pub struct HexError;

    impl std::fmt::Display for HexError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "invalid hex string")
        }
    }

    impl std::error::Error for HexError {}
}
