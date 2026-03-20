use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::AppState;
use crate::error::ApiError;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/scores/{attribute_slug}", get(get_scores))
        .route("/scores/{attribute_slug}/solve", post(solve_scores))
}

#[derive(Serialize)]
struct ScoreRow {
    entity_id: Uuid,
    entity_uri: String,
    entity_name: Option<String>,
    score: f64,
    uncertainty: Option<f64>,
    comparison_count: i32,
    solved_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Deserialize)]
struct ScoreParams {
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn get_scores(
    State(state): State<Arc<AppState>>,
    Path(attribute_slug): Path<String>,
    Query(params): Query<ScoreParams>,
) -> Result<Json<Vec<ScoreRow>>, ApiError> {
    let limit = params.limit.unwrap_or(100).min(1000);
    let offset = params.offset.unwrap_or(0);

    let rows = sqlx::query_as::<_, (Uuid, String, Option<String>, f64, Option<f64>, i32, chrono::DateTime<chrono::Utc>)>(
        "SELECT s.entity_id, e.uri, e.name, s.score, s.uncertainty,
                s.comparison_count, s.solved_at
         FROM scores s
         JOIN entities e ON e.id = s.entity_id
         JOIN attributes a ON a.id = s.attribute_id
         WHERE a.slug = $1
         ORDER BY s.score DESC
         LIMIT $2 OFFSET $3",
    )
    .bind(&attribute_slug)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let scores: Vec<_> = rows
        .into_iter()
        .map(|r| ScoreRow {
            entity_id: r.0,
            entity_uri: r.1,
            entity_name: r.2,
            score: r.3,
            uncertainty: r.4,
            comparison_count: r.5,
            solved_at: r.6,
        })
        .collect();

    Ok(Json(scores))
}

async fn solve_scores(
    State(state): State<Arc<AppState>>,
    Path(attribute_slug): Path<String>,
) -> Result<Json<SolveResult>, ApiError> {
    let pool = &state.db;

    let attribute_id = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM attributes WHERE slug = $1",
    )
    .bind(&attribute_slug)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::NotFound(format!("attribute {attribute_slug}")))?;

    let comparisons = sqlx::query_as::<_, (Uuid, Uuid, f64, f64, f64)>(
        "SELECT entity_a_id, entity_b_id, ln_ratio, confidence, repeats
         FROM comparisons
         WHERE attribute_id = $1",
    )
    .bind(attribute_id)
    .fetch_all(pool)
    .await?;

    if comparisons.is_empty() {
        return Err(ApiError::BadRequest("no comparisons to solve".into()));
    }

    let mut entity_ids: Vec<Uuid> = Vec::new();
    let mut entity_index = std::collections::HashMap::new();
    for (a, b, _, _, _) in &comparisons {
        for id in [a, b] {
            if !entity_index.contains_key(id) {
                entity_index.insert(*id, entity_ids.len());
                entity_ids.push(*id);
            }
        }
    }

    let n = entity_ids.len();
    if n < 2 {
        return Err(ApiError::BadRequest("need at least 2 entities".into()));
    }

    use cardinal_harness::rating_engine::{
        AttributeParams, Observation, RatingEngine,
    };

    let observations: Vec<Observation> = comparisons
        .iter()
        .map(|(a, b, ln_ratio, confidence, repeats)| {
            let i = entity_index[a];
            let j = entity_index[b];
            Observation {
                i,
                j,
                ratio: ln_ratio.exp(),
                confidence: *confidence,
                rater_id: "default".to_string(),
                reps: *repeats,
            }
        })
        .collect();

    let mut engine = RatingEngine::new(
        n,
        AttributeParams::default(),
        std::collections::HashMap::new(),
        None,
    )
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    engine.add_observations(&observations);
    let summary = engine.solve();

    let mut tx = pool.begin().await?;

    sqlx::query("DELETE FROM scores WHERE attribute_id = $1")
        .bind(attribute_id)
        .execute(&mut *tx)
        .await?;

    for (idx, &entity_id) in entity_ids.iter().enumerate() {
        let score = summary.scores[idx];
        let uncertainty = Some(summary.diag_cov[idx]);
        let count = observations
            .iter()
            .filter(|o| o.i == idx || o.j == idx)
            .count() as i32;

        sqlx::query(
            "INSERT INTO scores (entity_id, attribute_id, score, uncertainty, comparison_count)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(entity_id)
        .bind(attribute_id)
        .bind(score)
        .bind(uncertainty)
        .bind(count)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    Ok(Json(SolveResult {
        attribute: attribute_slug,
        entities_scored: n,
        comparisons_used: comparisons.len(),
    }))
}

#[derive(Serialize)]
struct SolveResult {
    attribute: String,
    entities_scored: usize,
    comparisons_used: usize,
}
