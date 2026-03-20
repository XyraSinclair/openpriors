use sqlx::PgPool;
use uuid::Uuid;

use crate::error::ApiError;

/// Get current balance for a user (nanodollars).
pub async fn get_balance(pool: &PgPool, user_id: Uuid) -> Result<i64, ApiError> {
    let balance = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT balance_after FROM credit_events
         WHERE user_id = $1
         ORDER BY created_at DESC
         LIMIT 1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(ApiError::Db)?
    .flatten()
    .unwrap_or(0);

    Ok(balance)
}

/// Grant credits to a user. Idempotent via idempotency_key.
pub async fn grant_credits(
    pool: &PgPool,
    user_id: Uuid,
    nanodollars: i64,
    idempotency_key: &str,
    notes: Option<&str>,
) -> Result<i64, ApiError> {
    if nanodollars <= 0 {
        return Err(ApiError::BadRequest("grant amount must be positive".into()));
    }

    // Advisory lock to serialize credit mutations for this user
    sqlx::query("SELECT pg_advisory_xact_lock(credit_lock_key($1))")
        .bind(user_id)
        .execute(pool)
        .await
        .map_err(ApiError::Db)?;

    // Check idempotency
    let existing = sqlx::query_scalar::<_, i64>(
        "SELECT balance_after FROM credit_events
         WHERE user_id = $1 AND idempotency_key = $2",
    )
    .bind(user_id)
    .bind(idempotency_key)
    .fetch_optional(pool)
    .await
    .map_err(ApiError::Db)?;

    if let Some(balance) = existing {
        return Ok(balance);
    }

    let current_balance = get_balance(pool, user_id).await?;
    let new_balance = current_balance + nanodollars;

    let balance_after = sqlx::query_scalar::<_, i64>(
        "INSERT INTO credit_events (user_id, kind, credits_delta, balance_after, idempotency_key, notes)
         VALUES ($1, 'grant', $2, $3, $4, $5)
         RETURNING balance_after",
    )
    .bind(user_id)
    .bind(nanodollars)
    .bind(new_balance)
    .bind(idempotency_key)
    .bind(notes)
    .fetch_one(pool)
    .await
    .map_err(ApiError::Db)?;

    Ok(balance_after)
}

/// Burn credits from a user's balance. Returns new balance.
/// Fails if balance would go negative.
pub async fn burn_credits(
    pool: &PgPool,
    user_id: Uuid,
    nanodollars: i64,
    idempotency_key: &str,
    related_object: Option<&str>,
    notes: Option<&str>,
) -> Result<i64, ApiError> {
    if nanodollars <= 0 {
        return Err(ApiError::BadRequest("burn amount must be positive".into()));
    }

    // Advisory lock
    sqlx::query("SELECT pg_advisory_xact_lock(credit_lock_key($1))")
        .bind(user_id)
        .execute(pool)
        .await
        .map_err(ApiError::Db)?;

    // Check idempotency
    let existing = sqlx::query_scalar::<_, i64>(
        "SELECT balance_after FROM credit_events
         WHERE user_id = $1 AND idempotency_key = $2",
    )
    .bind(user_id)
    .bind(idempotency_key)
    .fetch_optional(pool)
    .await
    .map_err(ApiError::Db)?;

    if let Some(balance) = existing {
        return Ok(balance);
    }

    let current_balance = get_balance(pool, user_id).await?;
    let new_balance = current_balance - nanodollars;

    // The trigger will reject if new_balance < 0
    let balance_after = sqlx::query_scalar::<_, i64>(
        "INSERT INTO credit_events (user_id, kind, credits_delta, balance_after, idempotency_key, related_object, notes)
         VALUES ($1, 'burn', $2, $3, $4, $5, $6)
         RETURNING balance_after",
    )
    .bind(user_id)
    .bind(-nanodollars)
    .bind(new_balance)
    .bind(idempotency_key)
    .bind(related_object)
    .bind(notes)
    .fetch_one(pool)
    .await
    .map_err(|e| {
        let msg = e.to_string();
        if msg.contains("insufficient credits") {
            ApiError::Forbidden("insufficient credits".into())
        } else {
            ApiError::Db(e)
        }
    })?;

    Ok(balance_after)
}
