use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};

use cardinal_harness::gateway::{PairwiseAnswer, PairwiseLogprobPosterior, TokenLogprob};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::posterior::derive_pairwise_posterior;

const PROBABILITY_TOLERANCE: f64 = 1e-6;
const DEFAULT_TOP_FINDINGS: usize = 10;
const RESIDUAL_KEY: &str = "__RESIDUAL__";

#[derive(Debug, Clone)]
pub struct JudgementPosteriorRow {
    pub id: Uuid,
    pub entity_a_id: Uuid,
    pub entity_b_id: Uuid,
    pub attribute_id: Uuid,
    pub attribute_slug: String,
    pub rater_id: Uuid,
    pub rater_name: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub ln_ratio: Option<f64>,
    pub confidence: Option<f64>,
    pub output_logprobs_json: Option<serde_json::Value>,
    pub structured_posterior_json: Option<serde_json::Value>,
}

impl<'r> sqlx::FromRow<'r, PgRow> for JudgementPosteriorRow {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            entity_a_id: row.try_get("entity_a_id")?,
            entity_b_id: row.try_get("entity_b_id")?,
            attribute_id: row.try_get("attribute_id")?,
            attribute_slug: row.try_get("attribute_slug")?,
            rater_id: row.try_get("rater_id")?,
            rater_name: row.try_get("rater_name")?,
            status: row.try_get("status")?,
            created_at: row.try_get("created_at")?,
            ln_ratio: row.try_get("ln_ratio")?,
            confidence: row.try_get("confidence")?,
            output_logprobs_json: row.try_get("output_logprobs_json")?,
            structured_posterior_json: row.try_get("structured_posterior_json")?,
        })
    }
}

#[derive(Debug, Clone)]
struct ParsedJudgementPosterior {
    row: JudgementPosteriorRow,
    output_logprobs: Option<Vec<TokenLogprob>>,
    output_logprobs_parse_failed: bool,
    posterior: Option<PairwiseLogprobPosterior>,
    posterior_parse_failed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PosteriorDiagnosticsReport {
    pub generated_at: DateTime<Utc>,
    pub total_rows: usize,
    pub rows_with_output_logprobs: usize,
    pub rows_with_structured_posterior: usize,
    pub integrity: PosteriorIntegrityReport,
    pub residual_mass: ResidualMassReport,
    pub replay: ReplayConsistencyReport,
    pub repeats: RepeatStabilityReport,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PosteriorIntegrityReport {
    pub output_logprobs_parse_failures: usize,
    pub structured_posterior_parse_failures: usize,
    pub higher_ranked_total_probability_violations: usize,
    pub ratio_total_probability_violations: usize,
    pub answer_total_probability_violations: usize,
    pub signed_total_probability_violations: usize,
    pub selected_higher_ranked_missing_from_support: usize,
    pub selected_ratio_bucket_missing_from_support: usize,
    pub selected_answer_missing_from_support: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ResidualMassReport {
    pub higher_ranked: Option<NumericSummary>,
    pub ratio: Option<NumericSummary>,
    pub answer: Option<NumericSummary>,
    pub signed_residual: Option<NumericSummary>,
    pub signed_abstain: Option<NumericSummary>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ReplayConsistencyReport {
    pub replayable_rows: usize,
    pub replay_failures: usize,
    pub selected_higher_ranked_mismatches: usize,
    pub selected_ratio_bucket_mismatches: usize,
    pub selected_answer_mismatches: usize,
    pub answer_js_divergence: Option<NumericSummary>,
    pub signed_mean_delta: Option<NumericSummary>,
    pub confidence_scalar_delta: Option<NumericSummary>,
    pub top_drift_rows: Vec<ReplayDriftRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReplayDriftRow {
    pub judgement_id: Uuid,
    pub attribute_slug: String,
    pub rater_name: String,
    pub selected_answer_mismatch: bool,
    pub answer_js_divergence: f64,
    pub signed_mean_delta: f64,
    pub confidence_scalar_delta: f64,
    pub answer_residual_delta: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RepeatStabilityReport {
    pub repeated_groups: usize,
    pub compared_pairs: usize,
    pub pairwise_answer_js_divergence: Option<NumericSummary>,
    pub group_signed_mean_stddev: Option<NumericSummary>,
    pub group_confidence_stddev: Option<NumericSummary>,
    pub top_unstable_groups: Vec<RepeatGroupInstability>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepeatGroupInstability {
    pub entity_a_id: Uuid,
    pub entity_b_id: Uuid,
    pub attribute_slug: String,
    pub rater_name: String,
    pub judgement_count: usize,
    pub avg_pairwise_answer_js_divergence: f64,
    pub signed_mean_stddev: f64,
    pub confidence_stddev: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct NumericSummary {
    pub count: usize,
    pub min: f64,
    pub mean: f64,
    pub p50: f64,
    pub p90: f64,
    pub p99: f64,
    pub max: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RepeatGroupKey {
    entity_a_id: Uuid,
    entity_b_id: Uuid,
    attribute_id: Uuid,
    rater_id: Uuid,
    attribute_slug: String,
    rater_name: String,
}

pub async fn load_judgement_posteriors(
    pool: &PgPool,
    limit: Option<i64>,
) -> Result<Vec<JudgementPosteriorRow>, sqlx::Error> {
    let base_query = "SELECT
            j.id,
            j.entity_a_id,
            j.entity_b_id,
            j.attribute_id,
            a.slug AS attribute_slug,
            j.rater_id,
            r.name AS rater_name,
            j.status,
            j.created_at,
            j.ln_ratio,
            j.confidence,
            j.output_logprobs_json,
            j.structured_posterior_json
         FROM judgements j
         JOIN attributes a ON a.id = j.attribute_id
         JOIN raters r ON r.id = j.rater_id
         WHERE j.output_logprobs_json IS NOT NULL OR j.structured_posterior_json IS NOT NULL
         ORDER BY j.created_at DESC";

    match limit {
        Some(limit) if limit > 0 => {
            let query = format!("{base_query} LIMIT $1");
            sqlx::query_as::<_, JudgementPosteriorRow>(&query)
                .bind(limit)
                .fetch_all(pool)
                .await
        }
        _ => {
            sqlx::query_as::<_, JudgementPosteriorRow>(base_query)
                .fetch_all(pool)
                .await
        }
    }
}

pub async fn load_and_analyze_judgement_posteriors(
    pool: &PgPool,
    limit: Option<i64>,
) -> Result<PosteriorDiagnosticsReport, sqlx::Error> {
    let rows = load_judgement_posteriors(pool, limit).await?;
    Ok(analyze_judgement_posteriors(&rows))
}

pub fn analyze_judgement_posteriors(rows: &[JudgementPosteriorRow]) -> PosteriorDiagnosticsReport {
    let parsed_rows: Vec<_> = rows.iter().cloned().map(parse_row).collect();

    let mut integrity = PosteriorIntegrityReport::default();
    let mut higher_ranked_residuals = Vec::new();
    let mut ratio_residuals = Vec::new();
    let mut answer_residuals = Vec::new();
    let mut signed_residuals = Vec::new();
    let mut signed_abstains = Vec::new();
    let mut replay_report = ReplayConsistencyReport::default();

    for parsed in &parsed_rows {
        integrity.output_logprobs_parse_failures +=
            usize::from(parsed.output_logprobs_parse_failed);
        integrity.structured_posterior_parse_failures += usize::from(parsed.posterior_parse_failed);

        if let Some(posterior) = &parsed.posterior {
            if probability_violated(posterior.higher_ranked_distribution.total_probability()) {
                integrity.higher_ranked_total_probability_violations += 1;
            }
            if probability_violated(posterior.ratio_distribution.total_probability()) {
                integrity.ratio_total_probability_violations += 1;
            }
            if probability_violated(posterior.answer_distribution.total_probability()) {
                integrity.answer_total_probability_violations += 1;
            }
            if probability_violated(posterior.signed_ln_ratio_distribution.total_probability()) {
                integrity.signed_total_probability_violations += 1;
            }

            if posterior
                .higher_ranked_distribution
                .probability_of(|side| *side == posterior.selected_higher_ranked)
                <= 0.0
            {
                integrity.selected_higher_ranked_missing_from_support += 1;
            }
            if posterior
                .ratio_distribution
                .probability_of(|bucket| *bucket == posterior.selected_ratio_bucket)
                <= 0.0
            {
                integrity.selected_ratio_bucket_missing_from_support += 1;
            }
            if posterior
                .answer_distribution
                .probability_of(|answer| *answer == posterior.selected_answer)
                <= 0.0
            {
                integrity.selected_answer_missing_from_support += 1;
            }

            higher_ranked_residuals.push(posterior.higher_ranked_distribution.residual_probability);
            ratio_residuals.push(posterior.ratio_distribution.residual_probability);
            answer_residuals.push(posterior.answer_distribution.residual_probability);
            signed_residuals.push(
                posterior
                    .signed_ln_ratio_distribution
                    .distribution
                    .residual_probability,
            );
            signed_abstains.push(posterior.signed_ln_ratio_distribution.abstain_probability);
        }

        if let (Some(logprobs), Some(stored)) = (&parsed.output_logprobs, &parsed.posterior) {
            replay_report.replayable_rows += 1;
            match derive_pairwise_posterior(
                Some(logprobs.as_slice()),
                stored.selected_higher_ranked,
                stored.selected_ratio,
            ) {
                Some(replayed) => {
                    let answer_js_divergence = js_divergence(
                        &answer_distribution_map(&stored.answer_distribution),
                        &answer_distribution_map(&replayed.answer_distribution),
                    );
                    let signed_mean_delta = absolute_difference(
                        stored.mean_signed_ln_ratio(),
                        replayed.mean_signed_ln_ratio(),
                    );
                    let confidence_scalar_delta =
                        (stored.confidence.as_scalar() - replayed.confidence.as_scalar()).abs();
                    let answer_residual_delta = (stored.answer_distribution.residual_probability
                        - replayed.answer_distribution.residual_probability)
                        .abs();

                    replay_report.selected_higher_ranked_mismatches += usize::from(
                        stored.selected_higher_ranked != replayed.selected_higher_ranked,
                    );
                    replay_report.selected_ratio_bucket_mismatches +=
                        usize::from(stored.selected_ratio_bucket != replayed.selected_ratio_bucket);
                    replay_report.selected_answer_mismatches +=
                        usize::from(stored.selected_answer != replayed.selected_answer);

                    replay_report.top_drift_rows.push(ReplayDriftRow {
                        judgement_id: parsed.row.id,
                        attribute_slug: parsed.row.attribute_slug.clone(),
                        rater_name: parsed.row.rater_name.clone(),
                        selected_answer_mismatch: stored.selected_answer
                            != replayed.selected_answer,
                        answer_js_divergence,
                        signed_mean_delta,
                        confidence_scalar_delta,
                        answer_residual_delta,
                    });
                }
                None => {
                    replay_report.replay_failures += 1;
                }
            }
        }
    }

    let mut answer_js_values = Vec::new();
    let mut signed_mean_deltas = Vec::new();
    let mut confidence_deltas = Vec::new();
    replay_report.top_drift_rows.retain(|row| {
        answer_js_values.push(row.answer_js_divergence);
        signed_mean_deltas.push(row.signed_mean_delta);
        confidence_deltas.push(row.confidence_scalar_delta);
        row.selected_answer_mismatch
            || row.answer_js_divergence > PROBABILITY_TOLERANCE
            || row.signed_mean_delta > PROBABILITY_TOLERANCE
            || row.confidence_scalar_delta > PROBABILITY_TOLERANCE
            || row.answer_residual_delta > PROBABILITY_TOLERANCE
    });
    replay_report.top_drift_rows.sort_by(replay_drift_ordering);
    replay_report.top_drift_rows.truncate(DEFAULT_TOP_FINDINGS);
    replay_report.answer_js_divergence = summarize(answer_js_values);
    replay_report.signed_mean_delta = summarize(signed_mean_deltas);
    replay_report.confidence_scalar_delta = summarize(confidence_deltas);

    let residual_mass = ResidualMassReport {
        higher_ranked: summarize(higher_ranked_residuals),
        ratio: summarize(ratio_residuals),
        answer: summarize(answer_residuals),
        signed_residual: summarize(signed_residuals),
        signed_abstain: summarize(signed_abstains),
    };

    let repeats = analyze_repeat_stability(&parsed_rows);

    PosteriorDiagnosticsReport {
        generated_at: Utc::now(),
        total_rows: rows.len(),
        rows_with_output_logprobs: rows
            .iter()
            .filter(|row| row.output_logprobs_json.is_some())
            .count(),
        rows_with_structured_posterior: rows
            .iter()
            .filter(|row| row.structured_posterior_json.is_some())
            .count(),
        integrity,
        residual_mass,
        replay: replay_report,
        repeats,
    }
}

fn analyze_repeat_stability(parsed_rows: &[ParsedJudgementPosterior]) -> RepeatStabilityReport {
    let mut groups: HashMap<RepeatGroupKey, Vec<&PairwiseLogprobPosterior>> = HashMap::new();

    for parsed in parsed_rows {
        let Some(posterior) = parsed.posterior.as_ref() else {
            continue;
        };
        let key = RepeatGroupKey {
            entity_a_id: parsed.row.entity_a_id,
            entity_b_id: parsed.row.entity_b_id,
            attribute_id: parsed.row.attribute_id,
            rater_id: parsed.row.rater_id,
            attribute_slug: parsed.row.attribute_slug.clone(),
            rater_name: parsed.row.rater_name.clone(),
        };
        groups.entry(key).or_default().push(posterior);
    }

    let mut pairwise_js_values = Vec::new();
    let mut signed_mean_stddevs = Vec::new();
    let mut confidence_stddevs = Vec::new();
    let mut unstable_groups = Vec::new();
    let mut repeated_groups = 0usize;
    let mut compared_pairs = 0usize;

    for (key, group) in groups {
        if group.len() < 2 {
            continue;
        }
        repeated_groups += 1;

        let mut group_pairwise_js = Vec::new();
        for left in 0..group.len() {
            for right in (left + 1)..group.len() {
                compared_pairs += 1;
                let divergence = js_divergence(
                    &answer_distribution_map(&group[left].answer_distribution),
                    &answer_distribution_map(&group[right].answer_distribution),
                );
                pairwise_js_values.push(divergence);
                group_pairwise_js.push(divergence);
            }
        }

        let signed_means: Vec<f64> = group
            .iter()
            .filter_map(|posterior| posterior.mean_signed_ln_ratio())
            .collect();
        let confidence_scalars: Vec<f64> = group
            .iter()
            .map(|posterior| posterior.confidence.as_scalar())
            .collect();

        let signed_mean_stddev = stddev(&signed_means).unwrap_or(0.0);
        let confidence_stddev = stddev(&confidence_scalars).unwrap_or(0.0);
        signed_mean_stddevs.push(signed_mean_stddev);
        confidence_stddevs.push(confidence_stddev);

        unstable_groups.push(RepeatGroupInstability {
            entity_a_id: key.entity_a_id,
            entity_b_id: key.entity_b_id,
            attribute_slug: key.attribute_slug,
            rater_name: key.rater_name,
            judgement_count: group.len(),
            avg_pairwise_answer_js_divergence: mean(&group_pairwise_js).unwrap_or(0.0),
            signed_mean_stddev,
            confidence_stddev,
        });
    }

    unstable_groups.sort_by(repeat_group_ordering);
    unstable_groups.truncate(DEFAULT_TOP_FINDINGS);

    RepeatStabilityReport {
        repeated_groups,
        compared_pairs,
        pairwise_answer_js_divergence: summarize(pairwise_js_values),
        group_signed_mean_stddev: summarize(signed_mean_stddevs),
        group_confidence_stddev: summarize(confidence_stddevs),
        top_unstable_groups: unstable_groups,
    }
}

fn parse_row(row: JudgementPosteriorRow) -> ParsedJudgementPosterior {
    let output_logprobs = row
        .output_logprobs_json
        .as_ref()
        .and_then(|value| serde_json::from_value::<Vec<TokenLogprob>>(value.clone()).ok());
    let output_logprobs_parse_failed =
        row.output_logprobs_json.is_some() && output_logprobs.is_none();
    let posterior = row
        .structured_posterior_json
        .as_ref()
        .and_then(|value| serde_json::from_value::<PairwiseLogprobPosterior>(value.clone()).ok());
    let posterior_parse_failed = row.structured_posterior_json.is_some() && posterior.is_none();

    ParsedJudgementPosterior {
        output_logprobs,
        output_logprobs_parse_failed,
        posterior,
        posterior_parse_failed,
        row,
    }
}

fn probability_violated(total_probability: f64) -> bool {
    (total_probability - 1.0).abs() > PROBABILITY_TOLERANCE
}

fn answer_distribution_map(
    distribution: &cardinal_harness::DiscreteDistribution<PairwiseAnswer>,
) -> BTreeMap<String, f64> {
    let mut values = BTreeMap::new();
    for entry in &distribution.support {
        values.insert(answer_key(entry.value), entry.probability);
    }
    values.insert(RESIDUAL_KEY.to_string(), distribution.residual_probability);
    values
}

fn answer_key(answer: PairwiseAnswer) -> String {
    match answer {
        PairwiseAnswer::A(bucket) => format!("A_{bucket:?}"),
        PairwiseAnswer::B(bucket) => format!("B_{bucket:?}"),
        PairwiseAnswer::Refuse => "REFUSE".to_string(),
    }
}

fn js_divergence(left: &BTreeMap<String, f64>, right: &BTreeMap<String, f64>) -> f64 {
    let mut keys = BTreeMap::new();
    for key in left.keys() {
        keys.insert(key.clone(), ());
    }
    for key in right.keys() {
        keys.insert(key.clone(), ());
    }

    let mut divergence = 0.0;
    for key in keys.keys() {
        let p = left.get(key).copied().unwrap_or(0.0);
        let q = right.get(key).copied().unwrap_or(0.0);
        let midpoint = 0.5 * (p + q);
        divergence += 0.5 * kl_term(p, midpoint) + 0.5 * kl_term(q, midpoint);
    }
    divergence
}

fn kl_term(value: f64, midpoint: f64) -> f64 {
    if value <= 0.0 || midpoint <= 0.0 {
        0.0
    } else {
        value * (value / midpoint).ln()
    }
}

fn absolute_difference(left: Option<f64>, right: Option<f64>) -> f64 {
    match (left, right) {
        (Some(left), Some(right)) => (left - right).abs(),
        _ => 0.0,
    }
}

fn mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f64>() / values.len() as f64)
    }
}

fn stddev(values: &[f64]) -> Option<f64> {
    let mean = mean(values)?;
    Some(
        (values
            .iter()
            .map(|value| {
                let centered = value - mean;
                centered * centered
            })
            .sum::<f64>()
            / values.len() as f64)
            .sqrt(),
    )
}

fn summarize(mut values: Vec<f64>) -> Option<NumericSummary> {
    values.retain(|value| value.is_finite());
    if values.is_empty() {
        return None;
    }

    values.sort_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal));
    let count = values.len();
    let sum = values.iter().sum::<f64>();

    Some(NumericSummary {
        count,
        min: values[0],
        mean: sum / count as f64,
        p50: percentile(&values, 0.50),
        p90: percentile(&values, 0.90),
        p99: percentile(&values, 0.99),
        max: values[count - 1],
    })
}

fn percentile(sorted_values: &[f64], quantile: f64) -> f64 {
    let idx = ((sorted_values.len() - 1) as f64 * quantile.clamp(0.0, 1.0)).round() as usize;
    sorted_values[idx]
}

fn replay_drift_ordering(left: &ReplayDriftRow, right: &ReplayDriftRow) -> Ordering {
    right
        .selected_answer_mismatch
        .cmp(&left.selected_answer_mismatch)
        .then_with(|| {
            right
                .answer_js_divergence
                .partial_cmp(&left.answer_js_divergence)
                .unwrap_or(Ordering::Equal)
        })
        .then_with(|| {
            right
                .signed_mean_delta
                .partial_cmp(&left.signed_mean_delta)
                .unwrap_or(Ordering::Equal)
        })
        .then_with(|| {
            right
                .confidence_scalar_delta
                .partial_cmp(&left.confidence_scalar_delta)
                .unwrap_or(Ordering::Equal)
        })
}

fn repeat_group_ordering(
    left: &RepeatGroupInstability,
    right: &RepeatGroupInstability,
) -> Ordering {
    right
        .avg_pairwise_answer_js_divergence
        .partial_cmp(&left.avg_pairwise_answer_js_divergence)
        .unwrap_or(Ordering::Equal)
        .then_with(|| {
            right
                .signed_mean_stddev
                .partial_cmp(&left.signed_mean_stddev)
                .unwrap_or(Ordering::Equal)
        })
        .then_with(|| {
            right
                .confidence_stddev
                .partial_cmp(&left.confidence_stddev)
                .unwrap_or(Ordering::Equal)
        })
}

#[cfg(test)]
mod tests {
    use super::{
        analyze_judgement_posteriors, answer_distribution_map, js_divergence, JudgementPosteriorRow,
    };
    use crate::posterior::{output_logprobs_json, pairwise_posterior_json};
    use cardinal_harness::gateway::{
        pairwise_logprob_posterior, PairwiseAnswer, PairwisePreferredSide, RatioBucket,
        TokenAlternative, TokenLogprob,
    };
    use chrono::Utc;
    use serde_json::json;
    use uuid::Uuid;

    fn sample_logprobs(preferred: PairwisePreferredSide, ratio_token: &str) -> Vec<TokenLogprob> {
        let selected_winner = match preferred {
            PairwisePreferredSide::A => "\"A\"",
            PairwisePreferredSide::B => "\"B\"",
        };
        let alt_winner = match preferred {
            PairwisePreferredSide::A => "\"B\"",
            PairwisePreferredSide::B => "\"A\"",
        };

        vec![
            TokenLogprob {
                token: selected_winner.to_string(),
                logprob: -0.05,
                top_alternatives: vec![TokenAlternative {
                    token: alt_winner.to_string(),
                    logprob: -3.0,
                }],
            },
            TokenLogprob {
                token: ratio_token.to_string(),
                logprob: -0.1,
                top_alternatives: vec![TokenAlternative {
                    token: "1.5".to_string(),
                    logprob: -2.0,
                }],
            },
        ]
    }

    fn judgement_row(
        logprobs: Option<Vec<TokenLogprob>>,
        posterior: Option<cardinal_harness::gateway::PairwiseLogprobPosterior>,
        entity_a_id: Uuid,
        entity_b_id: Uuid,
        attribute_id: Uuid,
        rater_id: Uuid,
    ) -> JudgementPosteriorRow {
        JudgementPosteriorRow {
            id: Uuid::new_v4(),
            entity_a_id,
            entity_b_id,
            attribute_id,
            attribute_slug: "test-attribute".to_string(),
            rater_id,
            rater_name: "test-model".to_string(),
            status: "success".to_string(),
            created_at: Utc::now(),
            ln_ratio: posterior.as_ref().and_then(|p| p.mean_signed_ln_ratio()),
            confidence: posterior.as_ref().map(|p| p.confidence.as_scalar()),
            output_logprobs_json: output_logprobs_json(logprobs.as_deref()).expect("json"),
            structured_posterior_json: pairwise_posterior_json(posterior.as_ref()).expect("json"),
        }
    }

    #[test]
    fn js_divergence_is_zero_for_identical_answer_distributions() {
        let left = answer_distribution_map(&cardinal_harness::DiscreteDistribution::new(
            vec![cardinal_harness::WeightedValue {
                value: PairwiseAnswer::A(RatioBucket::R08),
                probability: 0.7,
            }],
            0.3,
        ));
        let right = left.clone();
        assert_eq!(js_divergence(&left, &right), 0.0);
    }

    #[test]
    fn report_detects_replay_drift_and_repeat_instability() {
        let entity_a_id = Uuid::new_v4();
        let entity_b_id = Uuid::new_v4();
        let attribute_id = Uuid::new_v4();
        let rater_id = Uuid::new_v4();

        let stable_logprobs = sample_logprobs(PairwisePreferredSide::A, "2.5");
        let stable_posterior = pairwise_logprob_posterior(
            &stable_logprobs,
            PairwisePreferredSide::A,
            2.5,
            &[1.0, 1.5, 2.5],
        )
        .expect("posterior");
        let stable_row = judgement_row(
            Some(stable_logprobs.clone()),
            Some(stable_posterior.clone()),
            entity_a_id,
            entity_b_id,
            attribute_id,
            rater_id,
        );

        let mut tampered = stable_posterior.clone();
        tampered.selected_answer = PairwiseAnswer::B(RatioBucket::R08);
        let drift_row = judgement_row(
            Some(stable_logprobs),
            Some(tampered),
            entity_a_id,
            entity_b_id,
            attribute_id,
            rater_id,
        );

        let divergent_logprobs = sample_logprobs(PairwisePreferredSide::B, "1.0");
        let divergent_posterior = pairwise_logprob_posterior(
            &divergent_logprobs,
            PairwisePreferredSide::B,
            1.0,
            &[1.0, 1.5, 2.5],
        )
        .expect("posterior");
        let unstable_repeat = judgement_row(
            Some(divergent_logprobs),
            Some(divergent_posterior),
            entity_a_id,
            entity_b_id,
            attribute_id,
            rater_id,
        );

        let report = analyze_judgement_posteriors(&[stable_row, drift_row, unstable_repeat]);

        assert_eq!(report.replay.selected_answer_mismatches, 1);
        assert_eq!(report.repeats.repeated_groups, 1);
        assert_eq!(report.repeats.top_unstable_groups.len(), 1);
        assert!(report.repeats.top_unstable_groups[0].avg_pairwise_answer_js_divergence > 0.0);
    }

    #[test]
    fn report_counts_parse_failures() {
        let row = JudgementPosteriorRow {
            id: Uuid::new_v4(),
            entity_a_id: Uuid::new_v4(),
            entity_b_id: Uuid::new_v4(),
            attribute_id: Uuid::new_v4(),
            attribute_slug: "broken".to_string(),
            rater_id: Uuid::new_v4(),
            rater_name: "test-model".to_string(),
            status: "success".to_string(),
            created_at: Utc::now(),
            ln_ratio: None,
            confidence: None,
            output_logprobs_json: Some(json!({"not": "an array"})),
            structured_posterior_json: Some(json!({"not": "a posterior"})),
        };

        let report = analyze_judgement_posteriors(&[row]);
        assert_eq!(report.integrity.output_logprobs_parse_failures, 1);
        assert_eq!(report.integrity.structured_posterior_parse_failures, 1);
    }
}
