use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

pub async fn connect(database_url: &str) -> PgPool {
    PgPoolOptions::new()
        .max_connections(20)
        .connect(database_url)
        .await
        .expect("failed to connect to database")
}

/// Ensure or retrieve an entity by URI, creating it if absent.
pub async fn ensure_entity(
    pool: &PgPool,
    uri: &str,
    name: Option<&str>,
    kind: Option<&str>,
) -> Result<uuid::Uuid, sqlx::Error> {
    let inserted = sqlx::query_scalar::<_, uuid::Uuid>(
        "INSERT INTO entities (uri, name, kind)
         VALUES ($1, $2, $3)
         ON CONFLICT (uri) DO NOTHING
         RETURNING id",
    )
    .bind(uri)
    .bind(name)
    .bind(kind)
    .fetch_one(pool)
    .await;

    match inserted {
        Ok(id) => Ok(id),
        Err(_) => {
            let existing =
                sqlx::query_scalar::<_, uuid::Uuid>("SELECT id FROM entities WHERE uri = $1")
                    .bind(uri)
                    .fetch_one(pool)
                    .await?;
            Ok(existing)
        }
    }
}

/// Ensure or retrieve an attribute by slug, creating it if absent.
pub async fn ensure_attribute(
    pool: &PgPool,
    slug: &str,
    name: Option<&str>,
    description: Option<&str>,
) -> Result<uuid::Uuid, sqlx::Error> {
    let display_name = name.unwrap_or(slug);
    let inserted = sqlx::query_scalar::<_, uuid::Uuid>(
        "INSERT INTO attributes (slug, name, description)
         VALUES ($1, $2, $3)
         ON CONFLICT (slug) DO NOTHING
         RETURNING id",
    )
    .bind(slug)
    .bind(display_name)
    .bind(description)
    .fetch_one(pool)
    .await;

    match inserted {
        Ok(id) => Ok(id),
        Err(_) => {
            let existing =
                sqlx::query_scalar::<_, uuid::Uuid>("SELECT id FROM attributes WHERE slug = $1")
                    .bind(slug)
                    .fetch_one(pool)
                    .await?;
            Ok(existing)
        }
    }
}

/// Ensure or retrieve a rater by (kind, name, provider).
pub async fn ensure_rater(
    pool: &PgPool,
    kind: &str,
    name: &str,
    provider: Option<&str>,
) -> Result<uuid::Uuid, sqlx::Error> {
    let row = sqlx::query_scalar::<_, uuid::Uuid>(
        "INSERT INTO raters (kind, name, provider)
         VALUES ($1, $2, $3)
         ON CONFLICT (kind, name, provider) DO NOTHING
         RETURNING id",
    )
    .bind(kind)
    .bind(name)
    .bind(provider)
    .fetch_one(pool)
    .await;

    let row = match row {
        Ok(id) => id,
        Err(_) => {
            sqlx::query_scalar::<_, uuid::Uuid>(
                "SELECT id FROM raters WHERE kind = $1 AND name = $2 AND provider IS NOT DISTINCT FROM $3",
            )
            .bind(kind)
            .bind(name)
            .bind(provider)
            .fetch_one(pool)
            .await?
        }
    };
    Ok(row)
}

/// Upsert a comparison with repeats-weighted aggregation.
pub async fn upsert_comparison(
    pool: &PgPool,
    entity_a_id: uuid::Uuid,
    entity_b_id: uuid::Uuid,
    attribute_id: uuid::Uuid,
    rater_id: uuid::Uuid,
    ln_ratio: f64,
    confidence: f64,
) -> Result<uuid::Uuid, sqlx::Error> {
    let row = sqlx::query_scalar::<_, uuid::Uuid>(
        "INSERT INTO comparisons (entity_a_id, entity_b_id, attribute_id, rater_id, ln_ratio, confidence)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (entity_a_id, entity_b_id, attribute_id, rater_id)
         DO UPDATE SET
           ln_ratio = (comparisons.repeats * comparisons.ln_ratio + EXCLUDED.ln_ratio)
                      / (comparisons.repeats + 1.0),
           confidence = GREATEST(comparisons.confidence, EXCLUDED.confidence),
           repeats = comparisons.repeats + 1.0,
           updated_at = now()
         RETURNING id",
    )
    .bind(entity_a_id)
    .bind(entity_b_id)
    .bind(attribute_id)
    .bind(rater_id)
    .bind(ln_ratio)
    .bind(confidence)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Ensure a comparison row exists without counting it as a new observation.
pub async fn ensure_comparison(
    pool: &PgPool,
    entity_a_id: uuid::Uuid,
    entity_b_id: uuid::Uuid,
    attribute_id: uuid::Uuid,
    rater_id: uuid::Uuid,
    ln_ratio: f64,
    confidence: f64,
) -> Result<uuid::Uuid, sqlx::Error> {
    let row = sqlx::query_scalar::<_, uuid::Uuid>(
        "INSERT INTO comparisons (entity_a_id, entity_b_id, attribute_id, rater_id, ln_ratio, confidence)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (entity_a_id, entity_b_id, attribute_id, rater_id)
         DO UPDATE SET
           ln_ratio = comparisons.ln_ratio,
           confidence = comparisons.confidence,
           repeats = comparisons.repeats,
           updated_at = comparisons.updated_at
         RETURNING id",
    )
    .bind(entity_a_id)
    .bind(entity_b_id)
    .bind(attribute_id)
    .bind(rater_id)
    .bind(ln_ratio)
    .bind(confidence)
    .fetch_one(pool)
    .await?;
    Ok(row)
}
