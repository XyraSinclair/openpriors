# AGENTS.md

OpenPriors is open infrastructure for structured LLM judgements.

The core primitive: pairwise ratio comparisons between entities on attributes,
with full reasoning traces cached. A robust IRLS solver aggregates noisy
pairwise observations into globally consistent scores. Every judgement is
preserved with complete provenance (prompt, reasoning, output, cost, latency).

The goal: anyone can have an LLM rate any entity in the world by any attribute,
and propagate that measurement as far in the ecology as information technology allows.

---

## Collaboration Mode

This repo is in the fast direct-to-main collaboration set.

Default git behavior:

1. Before starting work, if the current checkout is clean:

```bash
./scripts/sync_main.sh
```

2. After a minimum good chunk of work:

```bash
git add <intentional-paths>
./scripts/push_main.sh "<clear message>"
```

Interpretation:

- default to `main`; do not create branches unless there is a strong reason
- commit small, coherent changes frequently
- push soon after useful progress
- pull with rebase, not merge
- stage only the files you intentionally changed
- do not use `git add -A` unless the entire worktree is intentionally part of
  the task
- never force-push `main`
- prefer the repo-local sync scripts over ad hoc git command sequences
- if the checkout is already dirty with unrelated work, or another agent is
  active, prefer a separate worktree or clean checkout rather than disturbing
  existing state
- if `push_main.sh` or `sync_main.sh` aborts because of a rebase conflict, do
  not guess in a half-rebased state; handle it deliberately in a clean worktree
  or branch

Background sync automation, if any, should default to `git fetch`, not blind
`pull --rebase` against an active working tree.

---

## Architecture

- **API**: Rust (axum), `src/`
- **Database**: PostgreSQL, `db/schema.sql`
- **Solver**: [cardinal-harness](https://github.com/XyraSinclair/cardinal-harness) — pairwise ratio → IRLS → cardinal scores with uncertainty
- **LLM Gateway**: OpenRouter via cardinal-harness `ProviderGateway`
- **Server**: dedicated OpenPriors infrastructure only; the confirmed host is the
  inference gateway at `204.168.182.12` (`basin-openpriors-cluster-proxy`).
  See `docs/infrastructure.md`.

### Core Tables

| Table | Purpose |
|-------|---------|
| `users` | Accounts (email + argon2id password) |
| `user_sessions` | Session tokens (blake3 hashed, 30d expiry) |
| `entities` | Anything in the world, identified by URI |
| `attributes` | Dimensions of measurement (slug-keyed) |
| `raters` | LLM models or humans that produce judgements |
| `judgements` | Full LLM reasoning traces (the high-throughput cache) |
| `comparisons` | Aggregated pairwise measurements (input to solver) |
| `scores` | Globally consistent scores derived from comparisons |
| `api_keys` | Bearer-token auth tied to users (blake3 hashed) |
| `credit_events` | Append-only credit ledger (nanodollar precision) |

### API Routes

| Method | Path | Purpose |
|--------|------|---------|
| `POST` | `/v1/auth/signup` | Create account, returns session token |
| `POST` | `/v1/auth/login` | Login, returns session token |
| `POST` | `/v1/auth/api-keys` | Create API key (requires auth) |
| `POST` | `/v1/balance` | Check credit balance (requires auth) |
| `POST` | `/v1/credits/grant` | Admin-only credit grant |
| `POST` | `/v1/judge` | Submit a pairwise judgement (pre-computed or server-side LLM) |
| `POST` | `/v1/rate` | Full rating session — smart pair selection, LLM calls, scoring |
| `GET` | `/v1/judgements` | Browse cached judgements |
| `GET` | `/v1/scores/{attribute}` | Get global scores for an attribute (JSON) |
| `POST` | `/v1/scores/{attribute}/solve` | Run IRLS solver, refresh scores |
| `POST` | `/v1/entities` | Create/upsert an entity |
| `GET` | `/v1/entities` | List entities |
| `GET` | `/v1/entities/{id}` | Get entity by ID |
| `POST` | `/v1/attributes` | Create/upsert an attribute |
| `GET` | `/v1/attributes` | List attributes |
| `GET` | `/v1/attributes/{slug}` | Get attribute by slug |
| `GET` | `/health` | Health check |

### HTML Pages (public, no JS)

| Path | Purpose |
|------|---------|
| `/` | Landing page |
| `/scores/{attribute_slug}` | Public ranked table with OG tags |
| `/judgements/{id}` | Full reasoning trace for one judgement |

### Source Modules

| File | Purpose |
|------|---------|
| `src/main.rs` | Server entry point, AppState construction |
| `src/auth.rs` | `AuthUser` / `MaybeAuth` axum extractors, `AppState` |
| `src/config.rs` | Runtime config from environment |
| `src/credits.rs` | Credit ledger operations (grant, burn, balance) |
| `src/db.rs` | Database helpers (ensure_entity, ensure_attribute, etc.) |
| `src/error.rs` | `ApiError` enum with HTTP status mapping |
| `src/metering.rs` | `MeteringGateway` — wraps ChatGateway, bills per LLM call |
| `src/pg_cache.rs` | `PgPairwiseCache` — cardinal-harness PairwiseCache for PostgreSQL |
| `src/routes/auth.rs` | Auth endpoints (signup, login, API keys, balance, grants) |
| `src/routes/judge.rs` | Pairwise judgement submission (pre-computed or server-side LLM) |
| `src/routes/rate.rs` | Full rating session endpoint (multi_rerank_with_trace) |
| `src/routes/scores.rs` | Score retrieval and IRLS solver trigger |
| `src/routes/pages.rs` | Server-rendered HTML scores pages |
| `src/routes/entities.rs` | Entity CRUD |
| `src/routes/attributes.rs` | Attribute CRUD |

---

## How to Work

**Open source first.** No credentials in the repo. All secrets via `.env`.

**Read before editing.** Inspect the code you're changing.

**Infrastructure boundary.** OpenPriors work stays on OpenPriors-dedicated
hosts. Do not use or mutate ExoPriors or Pivotality infrastructure from this
repo. If a new OpenPriors host is introduced, document it in
`docs/infrastructure.md` before using it.

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

**Canonical entity ordering.** `entity_a_id < entity_b_id` everywhere (UUID ordering).
If the caller submits them in the wrong order, flip the ratio sign.

**Credits are the rate limiter.** No separate rate limiting middleware.
Users pre-purchase credits; LLM calls burn them with 20% markup.

**Auth model.** Two token types:
- `opk_*` prefix → API key (blake3 hash in api_keys, never expires unless revoked)
- `ops_*` prefix → session token (blake3 hash in user_sessions, 30d expiry)

---

## Principles

- **Ultra parsimony.** Minimal schema, minimal API surface, maximum composability.
- **Structured outputs that compound.** Every judgement improves the global score graph.
- **Full provenance.** Prompt, reasoning, output, cost, latency — all preserved.
- **Propagation-first.** Scores should be trivially consumable: JSON API, feeds, dumps.
- **cardinal-harness for measurement.** Pairwise ratios → IRLS → cardinal scores. The math works.
- **Simple externally, sophisticated internally.** Clean API surface hiding cardinal-harness planner, IRLS solver, prompt engineering.

---

## Running

```bash
cp .env.example .env
# Edit .env: DATABASE_URL, OPENROUTER_API_KEY

# Create database and apply schema
createdb openpriors
psql openpriors < db/schema.sql

# Run
cargo run
```

### First-time setup after schema

```bash
# Create account
curl -X POST http://localhost:8080/v1/auth/signup \
  -H "Content-Type: application/json" \
  -d '{"email": "you@example.com", "password": "your-password"}'

# Grant yourself credits (requires ADMIN_API_KEY in .env)
curl -X POST http://localhost:8080/v1/credits/grant \
  -H "Authorization: Bearer $ADMIN_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"user_id": "<your-uuid>", "amount_usd": 10.0}'
```

---

## What's Next

- [x] Server-side LLM calls via cardinal-harness ChatGateway
- [x] Full rating session endpoint (POST /v1/rate)
- [x] User accounts and session auth
- [x] Credit ledger and metering
- [x] Public scores pages with OG tags
- [ ] Atom/JSON feed endpoints for score propagation
- [ ] Webhook subscriptions for score updates
- [ ] Batch judge endpoint (submit many pairs at once)
- [ ] Receipt signing (Ed25519 proof that a judgement happened)
- [ ] JSON-LD / schema.org structured data in score responses
- [ ] Open data dumps (periodic CSV/Parquet export of all public scores)
- [ ] Stripe integration for credit purchase
