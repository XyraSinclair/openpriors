use axum::{
    extract::{Path, State},
    http::header,
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::{AppState, MaybeAuth};
use crate::error::ApiError;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(landing))
        .route("/scores/{attribute_slug}", get(scores_page))
        .route("/judgements/{id}", get(judgement_page))
}

// --- Landing ---

async fn landing() -> Html<String> {
    Html(format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>OpenPriors</title>
<meta name="description" content="Structured LLM judgements. Rate anything by any attribute. See the scores.">
<meta property="og:title" content="OpenPriors">
<meta property="og:description" content="Structured LLM judgements. Rate anything by any attribute.">
<meta property="og:type" content="website">
{STYLE}
</head>
<body>
<main>
<h1>OpenPriors</h1>
<p class="subtitle">Structured LLM judgements. Rate anything by any attribute. See the scores.</p>

<section>
<h2>How it works</h2>
<ol>
<li>Define an <strong>attribute</strong> — what dimension you want to measure (insightfulness, clarity, novelty, ...)</li>
<li>Give it <strong>entities</strong> — tweets, papers, repos, ideas, anything with text</li>
<li>OpenPriors asks an LLM to compare pairs: "how many times more insightful is A than B?"</li>
<li>A robust solver (IRLS with Huber loss) combines noisy pairwise observations into <strong>globally consistent scores with uncertainty</strong></li>
<li>Smart pair selection concentrates comparisons where they matter most — certifying the top-K ranking with minimal cost</li>
</ol>
</section>

<section>
<h2>API</h2>
<p>Rate 100 entities for ~$0.50-2.00:</p>
<pre><code>POST /v1/rate
{{
  "entities": [...],
  "attribute": "insightfulness",
  "model": "openai/gpt-5-mini"
}}</code></pre>
<p><a href="/v1/healthz">API health</a> · <a href="/v1/readyz">readiness</a></p>
</section>

<section>
<h2>Explore</h2>
<p>Public scores are visible at <code>/scores/{{attribute_slug}}</code></p>
</section>
</main>
</body>
</html>"#
    ))
}

// --- Scores Page ---

async fn scores_page(
    State(state): State<Arc<AppState>>,
    Path(attribute_slug): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let attr = sqlx::query_as::<_, (Uuid, String, String, Option<String>)>(
        "SELECT id, slug, name, description FROM attributes WHERE slug = $1",
    )
    .bind(&attribute_slug)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::NotFound(format!("attribute {attribute_slug}")))?;

    let (attribute_id, slug, attr_name, attr_desc) = attr;

    let rows = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            Option<String>,
            f64,
            Option<f64>,
            i32,
            chrono::DateTime<chrono::Utc>,
        ),
    >(
        "SELECT s.entity_id, e.uri, e.name, s.score, s.uncertainty,
                s.comparison_count, s.solved_at
         FROM scores s
         JOIN entities e ON e.id = s.entity_id
         WHERE s.attribute_id = $1
         ORDER BY s.score DESC
         LIMIT 500",
    )
    .bind(attribute_id)
    .fetch_all(&state.db)
    .await?;

    let total_comparisons: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM comparisons WHERE attribute_id = $1")
            .bind(attribute_id)
            .fetch_one(&state.db)
            .await?;

    // Normalize scores to 0-100 for display
    let (min_score, max_score) = if rows.is_empty() {
        (0.0, 1.0)
    } else {
        let scores: Vec<f64> = rows.iter().map(|r| r.3).collect();
        let min = scores.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = scores.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        if (max - min).abs() < 1e-10 {
            (min - 0.5, max + 0.5)
        } else {
            (min, max)
        }
    };

    let description = attr_desc.as_deref().unwrap_or(&attr_name);
    let top3: Vec<String> = rows
        .iter()
        .take(3)
        .map(|r| r.2.as_deref().unwrap_or(&r.1).chars().take(40).collect())
        .collect();
    let og_desc = if top3.is_empty() {
        format!("Scores for {attr_name}")
    } else {
        format!("Top: {}. {} entities rated.", top3.join(", "), rows.len())
    };

    let mut table_rows = String::new();
    for (rank, r) in rows.iter().enumerate() {
        let display_name = r.2.as_deref().unwrap_or(&r.1);
        let normalized = ((r.3 - min_score) / (max_score - min_score) * 100.0).round();
        let unc = r.4.unwrap_or(0.0);
        let unc_display = if unc > 0.0 {
            format!("{:.2}", unc)
        } else {
            "-".to_string()
        };

        table_rows.push_str(&format!(
            "<tr>\
            <td class=\"rank\">{rank}</td>\
            <td class=\"name\" title=\"{uri}\">{name}</td>\
            <td class=\"score\">{normalized}</td>\
            <td class=\"unc\">{unc_display}</td>\
            <td class=\"cmp\">{cmp}</td>\
            </tr>\n",
            rank = rank + 1,
            uri = html_escape(&r.1),
            name = html_escape(display_name),
            cmp = r.5,
        ));
    }

    let solved_at = rows
        .first()
        .map(|r| r.6.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "never".to_string());

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{attr_name} — OpenPriors</title>
<meta name="description" content="{og_desc_escaped}">
<meta property="og:title" content="{attr_name} — OpenPriors">
<meta property="og:description" content="{og_desc_escaped}">
<meta property="og:type" content="website">
<meta name="twitter:card" content="summary_large_image">
<meta name="twitter:title" content="{attr_name} — OpenPriors">
<meta name="twitter:description" content="{og_desc_escaped}">
{STYLE}
</head>
<body>
<main>
<h1><a href="/">OpenPriors</a> / {attr_name}</h1>
<p class="subtitle">{description_escaped}</p>
<p class="meta">{n} entities · {total_cmp} comparisons · solved {solved_at}</p>

<table>
<thead>
<tr>
<th class="rank">#</th>
<th class="name">Entity</th>
<th class="score">Score</th>
<th class="unc">±</th>
<th class="cmp">Cmp</th>
</tr>
</thead>
<tbody>
{table_rows}
</tbody>
</table>

<p class="api-link">API: <code>GET /v1/scores/{slug}</code></p>
</main>
</body>
</html>"#,
        attr_name = html_escape(&attr_name),
        og_desc_escaped = html_escape(&og_desc),
        description_escaped = html_escape(description),
        n = rows.len(),
        total_cmp = total_comparisons,
    );

    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html))
}

// --- Judgement Detail ---

async fn judgement_page(
    State(state): State<Arc<AppState>>,
    auth: MaybeAuth,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let row = sqlx::query_as::<
        _,
        (
            Uuid,
            Uuid,
            Uuid,
            Uuid,
            Uuid,
            Option<Uuid>,
            Option<f64>,
            Option<f64>,
            String,
            Option<String>,
            String,
            Option<i32>,
            Option<i32>,
            Option<i64>,
            Option<i32>,
            chrono::DateTime<chrono::Utc>,
        ),
    >(
        "SELECT j.id, j.entity_a_id, j.entity_b_id, j.attribute_id, j.rater_id, j.user_id,
                j.ln_ratio, j.confidence, j.status,
                j.reasoning_text, j.raw_output,
                j.input_tokens, j.output_tokens, j.cost_nanodollars, j.latency_ms,
                j.created_at
         FROM judgements j WHERE j.id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::NotFound(format!("judgement {id}")))?;

    if !state.config.public_judgements {
        let auth_user = auth
            .0
            .ok_or_else(|| ApiError::Unauthorized("authentication required".into()))?;

        if row.5 != Some(auth_user.user_id) {
            return Err(ApiError::Forbidden(
                "not allowed to view this judgement".into(),
            ));
        }
    }

    // Get entity and attribute names
    let entity_a = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT uri, name FROM entities WHERE id = $1",
    )
    .bind(row.1)
    .fetch_one(&state.db)
    .await?;

    let entity_b = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT uri, name FROM entities WHERE id = $1",
    )
    .bind(row.2)
    .fetch_one(&state.db)
    .await?;

    let attr =
        sqlx::query_as::<_, (String, String)>("SELECT slug, name FROM attributes WHERE id = $1")
            .bind(row.3)
            .fetch_one(&state.db)
            .await?;

    let rater_name = sqlx::query_scalar::<_, String>("SELECT name FROM raters WHERE id = $1")
        .bind(row.4)
        .fetch_one(&state.db)
        .await?;

    let entity_a_name = entity_a.1.as_deref().unwrap_or(&entity_a.0);
    let entity_b_name = entity_b.1.as_deref().unwrap_or(&entity_b.0);

    let ratio_display = match row.6 {
        Some(lr) if lr >= 0.0 => format!(
            "{} is {:.1}× more {} than {}",
            entity_a_name,
            lr.exp(),
            attr.1,
            entity_b_name
        ),
        Some(lr) => format!(
            "{} is {:.1}× more {} than {}",
            entity_b_name,
            (-lr).exp(),
            attr.1,
            entity_a_name
        ),
        None => "No ratio (refused/error)".to_string(),
    };

    let reasoning = row.9.as_deref().unwrap_or("(no reasoning trace)");
    let cost = row
        .13
        .map(|c| format!("${:.6}", c as f64 / 1e9))
        .unwrap_or_else(|| "-".to_string());
    let confidence_display = row
        .7
        .map(|value| format!("{value:.2}"))
        .unwrap_or_else(|| "-".to_string());

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Judgement — OpenPriors</title>
{STYLE}
</head>
<body>
<main>
<h1><a href="/">OpenPriors</a> / <a href="/scores/{attr_slug}">Scores</a> / Judgement</h1>

<section class="judgement-detail">
<h2>{ratio_display}</h2>
<table class="detail-table">
<tr><td>Status</td><td>{status}</td></tr>
<tr><td>Confidence</td><td>{confidence_display}</td></tr>
<tr><td>Entity A</td><td>{entity_a_name_escaped}</td></tr>
<tr><td>Entity B</td><td>{entity_b_name_escaped}</td></tr>
<tr><td>Attribute</td><td>{attr_name}</td></tr>
<tr><td>Rater</td><td>{rater}</td></tr>
<tr><td>Cost</td><td>{cost}</td></tr>
<tr><td>Tokens</td><td>{in_tok} in / {out_tok} out</td></tr>
<tr><td>Latency</td><td>{latency_ms}ms</td></tr>
<tr><td>Created</td><td>{created}</td></tr>
</table>

<h3>Reasoning</h3>
<pre class="reasoning">{reasoning_escaped}</pre>

<h3>Raw Output</h3>
<pre class="raw">{raw_escaped}</pre>
</section>
</main>
</body>
</html>"#,
        attr_slug = html_escape(&attr.0),
        ratio_display = html_escape(&ratio_display),
        status = html_escape(&row.8),
        confidence_display = confidence_display,
        entity_a_name_escaped = html_escape(entity_a_name),
        entity_b_name_escaped = html_escape(entity_b_name),
        attr_name = html_escape(&attr.1),
        rater = html_escape(&rater_name),
        in_tok = row
            .11
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_string()),
        out_tok = row
            .12
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_string()),
        latency_ms = row
            .14
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_string()),
        created = row.15.format("%Y-%m-%d %H:%M:%S UTC"),
        reasoning_escaped = html_escape(reasoning),
        raw_escaped = html_escape(&row.10),
    );

    Ok((
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        html,
    ))
}

// --- Helpers ---

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

const STYLE: &str = r#"<style>
:root {
  --bg: #fafaf8;
  --fg: #1a1a1a;
  --muted: #666;
  --border: #ddd;
  --accent: #2a4a2a;
  --code-bg: #f0f0ee;
}
* { margin: 0; padding: 0; box-sizing: border-box; }
body {
  font-family: 'SF Mono', 'Fira Code', 'Consolas', monospace;
  font-size: 14px;
  line-height: 1.6;
  color: var(--fg);
  background: var(--bg);
  max-width: 900px;
  margin: 0 auto;
  padding: 2rem 1.5rem;
}
h1 { font-size: 1.4rem; font-weight: 600; margin-bottom: 0.3rem; }
h1 a { color: var(--fg); text-decoration: none; }
h1 a:hover { text-decoration: underline; }
h2 { font-size: 1.1rem; font-weight: 600; margin: 1.5rem 0 0.5rem; }
h3 { font-size: 0.95rem; font-weight: 600; margin: 1.2rem 0 0.4rem; }
.subtitle { color: var(--muted); margin-bottom: 0.5rem; }
.meta { color: var(--muted); font-size: 0.85rem; margin-bottom: 1rem; }
section { margin: 1.5rem 0; }
ol { padding-left: 1.5rem; }
li { margin: 0.3rem 0; }
pre, code {
  font-family: inherit;
  background: var(--code-bg);
  padding: 0.15rem 0.3rem;
  border-radius: 3px;
  font-size: 0.9em;
}
pre {
  padding: 0.8rem;
  overflow-x: auto;
  white-space: pre-wrap;
  word-break: break-word;
}
table {
  width: 100%;
  border-collapse: collapse;
  margin: 0.5rem 0;
}
th, td {
  text-align: left;
  padding: 0.4rem 0.6rem;
  border-bottom: 1px solid var(--border);
}
th { font-weight: 600; font-size: 0.85rem; color: var(--muted); }
.rank { width: 3rem; text-align: right; }
.score { width: 5rem; text-align: right; font-weight: 600; }
.unc { width: 4rem; text-align: right; color: var(--muted); }
.cmp { width: 3rem; text-align: right; color: var(--muted); }
.name { max-width: 400px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.api-link { margin-top: 1rem; color: var(--muted); font-size: 0.85rem; }
.detail-table td:first-child { font-weight: 600; width: 120px; color: var(--muted); }
.reasoning { max-height: 400px; overflow-y: auto; }
.raw { max-height: 200px; overflow-y: auto; font-size: 0.85em; color: var(--muted); }
a { color: var(--accent); }
</style>"#;
