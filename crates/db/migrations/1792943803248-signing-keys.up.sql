CREATE TABLE signing_key (
    id uuid PRIMARY KEY,
    public_key bytea NOT NULL UNIQUE CHECK (octet_length(public_key) = 32),
    ciphertext bytea,
    data_key bytea,
    kek_id text,
    created_at timestamptz NOT NULL,
    retired_at timestamptz,
    CHECK (
        retired_at IS NULL
        OR (ciphertext IS NULL AND data_key IS NULL AND kek_id IS NULL)
    ),
    CHECK (
        retired_at IS NOT NULL
        OR (ciphertext IS NOT NULL AND data_key IS NOT NULL AND kek_id IS NOT NULL)
    )
);

CREATE UNIQUE INDEX signing_key_live ON signing_key ((true)) WHERE retired_at IS NULL;
