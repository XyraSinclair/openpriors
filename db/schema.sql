-- OpenPriors schema
-- High-throughput structured LLM judgements.
--
-- Core invariant: every judgement is a pairwise ratio comparison
-- between two entities on a single attribute, produced by a single rater,
-- with full reasoning trace preserved.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";
CREATE EXTENSION IF NOT EXISTS "citext";

-------------------------------------------------------------------------------
-- Users
-------------------------------------------------------------------------------

CREATE TABLE users (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email           citext NOT NULL UNIQUE,
    password_hash   TEXT NOT NULL,          -- argon2id
    account_state   TEXT NOT NULL DEFAULT 'active' CHECK (account_state IN ('active', 'suspended', 'deleted')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-------------------------------------------------------------------------------
-- Sessions
-------------------------------------------------------------------------------

CREATE TABLE user_sessions (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     UUID NOT NULL REFERENCES users(id),
    token_hash  BYTEA UNIQUE NOT NULL,     -- blake3(token)
    expires_at  TIMESTAMPTZ NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_sessions_user ON user_sessions (user_id);
CREATE INDEX idx_sessions_expires ON user_sessions (expires_at);

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
    value_type      TEXT NOT NULL DEFAULT 'ratio' CHECK (value_type IN ('ratio')),
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
    user_id         UUID REFERENCES users(id),

    -- Full trace (the cache payload)
    prompt_text     TEXT NOT NULL,
    reasoning_text  TEXT,                   -- chain-of-thought / reasoning trace
    raw_output      TEXT NOT NULL,          -- complete LLM response

    -- Entity text snapshots (for cache key integrity)
    entity_a_text   TEXT,
    entity_b_text   TEXT,
    question_text   TEXT,

    -- Structured extraction
    ln_ratio        DOUBLE PRECISION,       -- ln(score_a / score_b), NULL if refused/error
    confidence      DOUBLE PRECISION DEFAULT 0.5 CHECK (confidence IS NULL OR confidence BETWEEN 0.0 AND 1.0),
    status          TEXT NOT NULL DEFAULT 'success' CHECK (status IN ('success', 'refused', 'error', 'abstain')),

    -- Provenance
    prompt_hash     BYTEA NOT NULL,         -- blake3(prompt_text) for dedup/cache-hit
    input_tokens    INTEGER,
    output_tokens   INTEGER,
    cost_nanodollars BIGINT,                -- 1e-9 USD precision
    latency_ms      INTEGER,

    -- Idempotency
    idempotency_key TEXT,

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
-- Idempotency per user
CREATE UNIQUE INDEX idx_judgements_idempotency ON judgements (user_id, idempotency_key) WHERE idempotency_key IS NOT NULL;
-- Cache dedup: exact match on the four-tuple
CREATE INDEX idx_judgements_cache_lookup ON judgements (prompt_hash, entity_a_id, entity_b_id, attribute_id);

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
-- API keys: bearer-token auth tied to users
-------------------------------------------------------------------------------

CREATE TABLE api_keys (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id         UUID NOT NULL REFERENCES users(id),
    key_hash        BYTEA UNIQUE NOT NULL,      -- blake3(key) — never store plaintext
    key_prefix      TEXT NOT NULL,               -- first 8 chars for identification
    name            TEXT,
    scopes          TEXT[] NOT NULL DEFAULT '{}',
    monthly_spend_limit_nanodollars BIGINT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at      TIMESTAMPTZ
);

CREATE INDEX idx_api_keys_user ON api_keys (user_id);

-------------------------------------------------------------------------------
-- Credits: append-only ledger
--
-- All balance mutations are recorded. The balance is SUM(credits_delta).
-- Nanodollar precision (1e-9 USD) avoids floating-point drift.
-------------------------------------------------------------------------------

CREATE TABLE credit_events (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id         UUID NOT NULL REFERENCES users(id),
    kind            TEXT NOT NULL CHECK (kind IN ('grant', 'burn', 'adjust', 'reversal')),
    credits_delta   BIGINT NOT NULL,            -- positive = grant, negative = burn
    balance_after   BIGINT NOT NULL,            -- running balance for fast reads
    idempotency_key TEXT,
    related_object  TEXT,                        -- e.g. "judgement:{uuid}" or "api_key:{uuid}"
    notes           TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_credit_events_user ON credit_events (user_id, created_at DESC);
CREATE UNIQUE INDEX idx_credit_events_idempotency ON credit_events (user_id, idempotency_key) WHERE idempotency_key IS NOT NULL;

-- Advisory lock helper keyed on user UUID to serialize credit mutations
CREATE OR REPLACE FUNCTION credit_lock_key(uid UUID) RETURNS BIGINT AS $$
    SELECT ('x' || substr(uid::text, 1, 16))::bit(64)::bigint;
$$ LANGUAGE sql IMMUTABLE;

-- Trigger: prevent negative balance
CREATE OR REPLACE FUNCTION prevent_negative_balance() RETURNS trigger AS $$
BEGIN
    IF NEW.balance_after < 0 THEN
        RAISE EXCEPTION 'insufficient credits: balance would be % nanodollars', NEW.balance_after;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trg_credit_events_no_negative
    BEFORE INSERT ON credit_events
    FOR EACH ROW
    EXECUTE FUNCTION prevent_negative_balance();
