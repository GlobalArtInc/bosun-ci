ALTER TABLE "user" ADD COLUMN password_hash text;

ALTER TABLE "user" ADD COLUMN password_change_required boolean NOT NULL DEFAULT false;

CREATE TABLE session (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES "user" (id) ON DELETE CASCADE,
    sha256 bytea NOT NULL UNIQUE,
    created_at timestamptz NOT NULL,
    last_seen_at timestamptz NOT NULL
);

CREATE INDEX session_user_id ON session (user_id);

CREATE TABLE sign_in_failure (
    kind text NOT NULL,
    key text NOT NULL,
    failures integer NOT NULL,
    last_failed_at timestamptz NOT NULL,
    locked_until timestamptz,
    PRIMARY KEY (kind, key)
);
