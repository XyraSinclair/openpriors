# AGENTS.md

OpenPriors is open infrastructure for structured LLM judgements.

The core primitive: pairwise ratio comparisons between entities on attributes,
with full reasoning traces cached. A robust IRLS solver aggregates noisy
pairwise observations into globally consistent scores. Every judgement is
preserved with complete provenance (prompt, reasoning, output, cost, latency).

The goal: anyone can have an LLM rate any entity in the world by any attribute,
and propagate that measurement as far in the ecology as information technology allows.

---

## Architecture

- **API**: Rust (axum), `src/`
- **Database**: PostgreSQL, `db/schema.sql`
- **Solver**: [cardinal-harness](https://github.com/XyraSinclair/cardinal-harness) — pairwise ratio → IRLS → cardinal scores with uncertainty
- **Server**: planned separate host from ExoPriors

### Core Tables

| Table | Purpose |
|-------|---------|
| `entities` | Anything in the world, identified by URI |
| `attributes` | Dimensions of measurement (slug-keyed) |
| `raters` | LLM models or humans that produce judgements |
| `judgements` | Full LLM reasoning traces (the high-throughput cache) |
| `comparisons` | Aggregated pairwise measurements (input to solver) |
| `scores` | Globally consistent scores derived from comparisons |
| `api_keys` | Simple bearer-token auth |

### API Routes

| Method | Path | Purpose |
|--------|------|---------|
| `POST` | `/v1/judge` | Submit a pairwise judgement (with full trace) |
| `GET` | `/v1/judgements` | Browse cached judgements |
| `GET` | `/v1/scores/{attribute}` | Get global scores for an attribute |
| `POST` | `/v1/scores/{attribute}/solve` | Run IRLS solver, refresh scores |
| `POST` | `/v1/entities` | Create/upsert an entity |
| `GET` | `/v1/entities` | List entities |
| `GET` | `/v1/entities/{id}` | Get entity by ID |
| `POST` | `/v1/attributes` | Create/upsert an attribute |
| `GET` | `/v1/attributes` | List attributes |
| `GET` | `/v1/attributes/{slug}` | Get attribute by slug |
| `GET` | `/health` | Health check |

---

## How to Work

**Open source first.** No credentials in the repo. All secrets via `.env`.

**Read before editing.** Inspect the code you're changing.

**cardinal-harness is the algorithmic core.** Don't reinvent the solver, the
planner, or the prompt templates. Wrap them.

**Every judgement is a cache entry.** The judgement table is the high-throughput
cache of LLM reasoning. Prompt hash enables dedup. Raw output enables replay.
Reasoning text enables introspection.

**Comparisons aggregate.** Multiple judgements for the same
(entity pair, attribute, rater) merge via repeats-weighted averaging in the
comparisons table. This is the input to the IRLS solver.

**Scores are derived.** Run the solver, write the output. Scores are always
refreshable from comparisons. Never edit scores directly.

**Canonical entity ordering.** `entity_a_id < entity_b_id` everywhere.
If the caller submits them in the wrong order, flip the ratio sign.

---

## Principles

- **Ultra parsimony.** Minimal schema, minimal API surface, maximum composability.
- **Structured outputs that compound.** Every judgement improves the global score graph.
- **Full provenance.** Prompt, reasoning, output, cost, latency — all preserved.
- **Propagation-first.** Scores should be trivially consumable: JSON API, feeds, dumps.
- **cardinal-harness for measurement.** Pairwise ratios → IRLS → cardinal scores. The math works.

---

## Running

```bash
cp .env.example .env
# Edit .env with your DATABASE_URL

# Create database and apply schema
createdb openpriors
psql openpriors < db/schema.sql

# Run
cargo run
```

---

## What's Next

- [ ] Wire cardinal-harness `ChatGateway` for server-side LLM calls (currently accepts pre-computed judgements)
- [ ] Atom/JSON feed endpoints for score propagation
- [ ] Webhook subscriptions for score updates
- [ ] Batch judge endpoint (submit many pairs at once)
- [ ] Planner endpoint: "which pairs should I judge next for this attribute?"
- [ ] Receipt signing (Ed25519 proof that a judgement happened)
- [ ] JSON-LD / schema.org structured data in score responses
- [ ] Open data dumps (periodic CSV/Parquet export of all public scores)
