ALTER TABLE users
    ADD COLUMN IF NOT EXISTS role TEXT;

UPDATE users
SET role = 'user'
WHERE role IS NULL;

ALTER TABLE users
    ALTER COLUMN role SET DEFAULT 'user',
    ALTER COLUMN role SET NOT NULL;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'public.users'::regclass
          AND conname = 'users_role_check'
    ) THEN
        ALTER TABLE users
            ADD CONSTRAINT users_role_check
            CHECK (role IN ('user', 'admin'));
    END IF;
END
$$;

ALTER TABLE judgements
    ADD COLUMN IF NOT EXISTS cache_eligible BOOLEAN;

UPDATE judgements
SET cache_eligible = FALSE
WHERE cache_eligible IS NULL;

ALTER TABLE judgements
    ALTER COLUMN cache_eligible SET DEFAULT FALSE,
    ALTER COLUMN cache_eligible SET NOT NULL;

CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_judgements_cache_lookup_trusted
    ON judgements (prompt_hash, entity_a_id, entity_b_id, attribute_id, rater_id)
    WHERE cache_eligible = TRUE;

CREATE OR REPLACE FUNCTION credit_lock_key(uid UUID) RETURNS BIGINT AS $$
    SELECT ('x' || substr(replace(uid::text, '-', ''), 1, 16))::bit(64)::bigint;
$$ LANGUAGE sql IMMUTABLE;
