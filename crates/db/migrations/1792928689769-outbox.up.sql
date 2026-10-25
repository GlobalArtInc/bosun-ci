CREATE TABLE outbox (
    id bigserial PRIMARY KEY,
    kind text NOT NULL,
    collapse_key text NOT NULL,
    payload jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    attempts integer NOT NULL DEFAULT 0,
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    last_error text,
    failing_since timestamptz,
    dead_at timestamptz
);

CREATE INDEX outbox_due ON outbox (next_attempt_at, id) WHERE dead_at IS NULL;

CREATE INDEX outbox_collapse_key ON outbox (collapse_key, id);

CREATE TABLE forge_check_run (
    job_id uuid PRIMARY KEY REFERENCES job (id) ON DELETE CASCADE,
    id bigint NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
