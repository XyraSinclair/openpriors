ALTER TABLE judgements
    ADD COLUMN IF NOT EXISTS output_logprobs_json JSONB;

ALTER TABLE judgements
    ADD COLUMN IF NOT EXISTS structured_posterior_json JSONB;
