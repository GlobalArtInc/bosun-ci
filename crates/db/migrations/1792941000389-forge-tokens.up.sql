CREATE TABLE forge_token (
    id uuid PRIMARY KEY,
    execution_id uuid NOT NULL REFERENCES execution (id) ON DELETE CASCADE,
    repository_id bigint NOT NULL,
    permissions jsonb NOT NULL,
    ciphertext bytea NOT NULL,
    data_key bytea NOT NULL,
    kek_id text NOT NULL,
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX forge_token_execution ON forge_token (execution_id, expires_at);
