-- OpenPriors schema
-- High-throughput structured LLM judgements.
--
-- Core invariant: every judgement is a pairwise ratio comparison
-- between two entities on a single attribute, produced by a single rater,
-- with full reasoning trace preserved.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-------------------------------------------------------------------------------
-- Entities: anything in the world that can be rated
-------------------------------------------------------------------------------

CREATE TABLE entities (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    uri         TEXT UNIQUE NOT NULL,
    name        TEXT,
    kind        TEXT,                       -- optional taxonomy (paper, person, repo, idea, ...)
    payload     JSONB DEFAULT '{}',         -- arbitrary structured data about the entity
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_entities_kind ON entities (kind) WHERE kind IS NOT NULL;
CREATE INDEX idx_entities_created ON entities (created_at);

-------------------------------------------------------------------------------
-- Attributes: dimensions of measurement
-------------------------------------------------------------------------------

CREATE TABLE attributes (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    slug            TEXT UNIQUE NOT NULL,
    name            TEXT NOT NULL,
    description     TEXT,                   -- full natural-language definition
    prompt_template TEXT,                   -- LLM prompt template; {{entity_a}}, {{entity_b}} placeholders
    value_type      TEXT NOT NULL DEFAULT 'ratio' CHECK (value_type IN ('ratio', 'ordinal')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-------------------------------------------------------------------------------
-- Raters: LLM models or humans that produce judgements
-------------------------------------------------------------------------------

CREATE TABLE raters (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    kind         TEXT NOT NULL CHECK (kind IN ('human', 'model')),
    name         TEXT NOT NULL,             -- e.g. "claude-sonnet-4-6", "gpt-4o", username
    provider     TEXT,                      -- anthropic, openai, human, ...
    model_config JSONB DEFAULT '{}',        -- temperature, system prompt, etc.
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (kind, name, provider)
);

-------------------------------------------------------------------------------
-- Judgements: cached LLM reasoning traces
--
-- This is the high-throughput cache. Every LLM call that produces a
-- structured comparison is preserved here with full provenance:
-- the prompt sent, the reasoning produced, the raw output, and the
-- extracted structured result.
-------------------------------------------------------------------------------

CREATE TABLE judgements (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    entity_a_id     UUID NOT NULL REFERENCES entities(id),
    entity_b_id     UUID NOT NULL REFERENCES entities(id),
    attribute_id    UUID NOT NULL REFERENCES attributes(id),
    rater_id        UUID NOT NULL REFERENCES raters(id),

    -- Full trace (the cache payload)
    prompt_text     TEXT NOT NULL,
    reasoning_text  TEXT,                   -- chain-of-thought / reasoning trace
    raw_output      TEXT NOT NULL,          -- complete LLM response

    -- Structured extraction
    ln_ratio        DOUBLE PRECISION NOT NULL,  -- ln(score_a / score_b)
    confidence      DOUBLE PRECISION DEFAULT 0.5 CHECK (confidence BETWEEN 0.0 AND 1.0),

    -- Provenance
    prompt_hash     BYTEA NOT NULL,         -- blake3(prompt_text) for dedup/cache-hit
    input_tokens    INTEGER,
    output_tokens   INTEGER,
    cost_nanodollars BIGINT,                -- 1e-9 USD precision
    latency_ms      INTEGER,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),

    CHECK (entity_a_id < entity_b_id)       -- canonical ordering
);

-- Cache lookup: same prompt → same judgement
CREATE INDEX idx_judgements_prompt_hash ON judgements (prompt_hash);
-- Browse by attribute
CREATE INDEX idx_judgements_attribute ON judgements (attribute_id, created_at DESC);
-- Browse by entity
CREATE INDEX idx_judgements_entity_a ON judgements (entity_a_id, created_at DESC);
CREATE INDEX idx_judgements_entity_b ON judgements (entity_b_id, created_at DESC);

-------------------------------------------------------------------------------
-- Comparisons: aggregated pairwise measurements
--
-- Multiple judgements for the same (entity_a, entity_b, attribute, rater)
-- are aggregated here via repeats-weighted averaging. This is the input
-- to the IRLS solver.
-------------------------------------------------------------------------------

CREATE TABLE comparisons (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    entity_a_id     UUID NOT NULL REFERENCES entities(id),
    entity_b_id     UUID NOT NULL REFERENCES entities(id),
    attribute_id    UUID NOT NULL REFERENCES attributes(id),
    rater_id        UUID NOT NULL REFERENCES raters(id),

    ln_ratio        DOUBLE PRECISION NOT NULL,
    confidence      DOUBLE PRECISION DEFAULT 0.5,
    repeats         DOUBLE PRECISION NOT NULL DEFAULT 1.0,

    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),

    UNIQUE (entity_a_id, entity_b_id, attribute_id, rater_id),
    CHECK (entity_a_id < entity_b_id)
);

CREATE INDEX idx_comparisons_attribute ON comparisons (attribute_id);
CREATE INDEX idx_comparisons_rater ON comparisons (rater_id, attribute_id);

-------------------------------------------------------------------------------
-- Scores: globally consistent scores derived from comparisons
--
-- Refreshed by running the IRLS solver over all comparisons for an
-- attribute. These are the propagatable output.
-------------------------------------------------------------------------------

CREATE TABLE scores (
    entity_id       UUID NOT NULL REFERENCES entities(id),
    attribute_id    UUID NOT NULL REFERENCES attributes(id),
    score           DOUBLE PRECISION NOT NULL,
    uncertainty     DOUBLE PRECISION,           -- diagonal of covariance
    comparison_count INTEGER NOT NULL DEFAULT 0,
    solved_at       TIMESTAMPTZ NOT NULL DEFAULT now(),

    PRIMARY KEY (entity_id, attribute_id)
);

CREATE INDEX idx_scores_attribute_score ON scores (attribute_id, score DESC);

-------------------------------------------------------------------------------
-- API keys: simple bearer-token auth
-------------------------------------------------------------------------------

CREATE TABLE api_keys (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    key_hash    BYTEA UNIQUE NOT NULL,      -- blake3(key) — never store plaintext
    name        TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at  TIMESTAMPTZ
);
