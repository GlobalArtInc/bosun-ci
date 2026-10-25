CREATE TABLE user_identity (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES "user" (id) ON DELETE CASCADE,
    issuer text NOT NULL,
    subject text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (issuer, subject)
);

CREATE INDEX user_identity_user_id ON user_identity (user_id);

CREATE TABLE oidc_state (
    sha256 bytea PRIMARY KEY,
    expires_at timestamptz NOT NULL
);
