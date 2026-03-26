use std::env;
use std::process::ExitCode;

use openpriors::config::Config;
use openpriors::db;
use openpriors::diagnostics::{load_and_analyze_judgement_posteriors, PosteriorDiagnosticsReport};

#[tokio::main]
async fn main() -> ExitCode {
    if let Err(error) = run().await {
        eprintln!("logprob diagnostics failed: {error}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut limit = None;
    let mut json = false;

    let mut idx = 0usize;
    while idx < args.len() {
        match args[idx].as_str() {
            "--limit" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or("missing value after --limit")?
                    .parse::<i64>()?;
                if value > 0 {
                    limit = Some(value);
                }
            }
            "--json" => {
                json = true;
            }
            other => {
                return Err(format!("unknown argument: {other}").into());
            }
        }
        idx += 1;
    }

    let config = Config::from_env();
    let pool = db::connect(
        &config.database_url,
        config.database_max_connections,
        config.database_acquire_timeout(),
    )
    .await?;

    let report = load_and_analyze_judgement_posteriors(&pool, limit).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human_report(&report);
    }

    Ok(())
}

fn print_human_report(report: &PosteriorDiagnosticsReport) {
    println!("Logprob diagnostics");
    println!("generated_at: {}", report.generated_at);
    println!("rows: {}", report.total_rows);
    println!(
        "with_output_logprobs: {} | with_structured_posterior: {}",
        report.rows_with_output_logprobs, report.rows_with_structured_posterior
    );
    println!();
    println!(
        "integrity: output_logprob_parse_failures={} structured_posterior_parse_failures={} answer_total_probability_violations={} selected_answer_missing_from_support={}",
        report.integrity.output_logprobs_parse_failures,
        report.integrity.structured_posterior_parse_failures,
        report.integrity.answer_total_probability_violations,
        report.integrity.selected_answer_missing_from_support,
    );
    println!(
        "replay: replayable_rows={} replay_failures={} selected_answer_mismatches={}",
        report.replay.replayable_rows,
        report.replay.replay_failures,
        report.replay.selected_answer_mismatches,
    );
    if let Some(summary) = &report.replay.answer_js_divergence {
        println!(
            "replay.answer_js_divergence: mean={:.6} p90={:.6} max={:.6}",
            summary.mean, summary.p90, summary.max
        );
    }
    if let Some(summary) = &report.residual_mass.answer {
        println!(
            "answer_residual_mass: mean={:.6} p90={:.6} max={:.6}",
            summary.mean, summary.p90, summary.max
        );
    }
    if let Some(summary) = &report.repeats.pairwise_answer_js_divergence {
        println!(
            "repeat_pairwise_answer_js: mean={:.6} p90={:.6} max={:.6}",
            summary.mean, summary.p90, summary.max
        );
    }
    println!(
        "repeat_groups: {} | compared_pairs: {}",
        report.repeats.repeated_groups, report.repeats.compared_pairs
    );
    println!();

    if !report.replay.top_drift_rows.is_empty() {
        println!("Top replay drift rows:");
        for row in &report.replay.top_drift_rows {
            println!(
                "  {} {} {} mismatch={} js={:.6} mean_delta={:.6} confidence_delta={:.6} residual_delta={:.6}",
                row.judgement_id,
                row.attribute_slug,
                row.rater_name,
                row.selected_answer_mismatch,
                row.answer_js_divergence,
                row.signed_mean_delta,
                row.confidence_scalar_delta,
                row.answer_residual_delta,
            );
        }
        println!();
    }

    if !report.repeats.top_unstable_groups.is_empty() {
        println!("Top unstable repeat groups:");
        for group in &report.repeats.top_unstable_groups {
            println!(
                "  {} {} {} count={} avg_js={:.6} signed_stddev={:.6} confidence_stddev={:.6}",
                group.entity_a_id,
                group.entity_b_id,
                group.attribute_slug,
                group.judgement_count,
                group.avg_pairwise_answer_js_divergence,
                group.signed_mean_stddev,
                group.confidence_stddev,
            );
        }
    }
}
