# OpenPriors

High-throughput structured LLM judgements. Rate any entity by any attribute. Propagate everywhere.

## What this is

OpenPriors caches LLM pairwise ratio judgements and aggregates them into globally consistent scores using robust statistics (IRLS with Huber loss, via [cardinal-harness](https://github.com/XyraSinclair/cardinal-harness)).

The core loop:

1. Pick two entities and an attribute
2. An LLM judges "how many times more *attribute* does A have than B?"
3. The full reasoning trace is cached (prompt, chain-of-thought, output, cost)
4. Pairwise observations aggregate across raters and sessions
5. The IRLS solver produces cardinal scores with uncertainty estimates
6. Scores propagate via API, feeds, and dumps

Every judgement compounds. Every score is refreshable from the underlying observations. Every reasoning trace is preserved and inspectable.

## Quick start

```bash
git clone https://github.com/XyraSinclair/openpriors
cd openpriors

cp .env.example .env
# Set DATABASE_URL in .env

createdb openpriors
psql openpriors < db/schema.sql

cargo run
```

## API

Submit a judgement:

```bash
curl -X POST http://localhost:8080/v1/judge \
  -H "Content-Type: application/json" \
  -d '{
    "entity_a": "https://en.wikipedia.org/wiki/Rust_(programming_language)",
    "entity_b": "https://en.wikipedia.org/wiki/Go_(programming_language)",
    "attribute": "ergonomics_for_systems_programming",
    "model": "claude-sonnet-4-6",
    "ln_ratio": 0.7,
    "confidence": 0.8,
    "reasoning_text": "Rust has a steeper learning curve but more expressive type system..."
  }'
```

Solve scores for an attribute:

```bash
curl -X POST http://localhost:8080/v1/scores/ergonomics_for_systems_programming/solve
```

Get the leaderboard:

```bash
curl http://localhost:8080/v1/scores/ergonomics_for_systems_programming
```

## Architecture

- **Rust** (axum) API server
- **PostgreSQL** for all persistence
- **cardinal-harness** for the IRLS solver and pairwise comparison framework
- No credentials in the repo. All secrets via `.env`.

See [AGENTS.md](AGENTS.md) for the full technical reference.

## License

MIT
