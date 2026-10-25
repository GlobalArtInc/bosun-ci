CREATE TABLE forge_installation (
    id bigint PRIMARY KEY,
    account_id bigint NOT NULL,
    account_login text NOT NULL,
    organisation_id uuid UNIQUE REFERENCES organisation (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE forge_repository (
    id bigint PRIMARY KEY,
    installation_id bigint REFERENCES forge_installation (id) ON DELETE SET NULL,
    owner text NOT NULL,
    name text NOT NULL,
    default_branch text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX forge_repository_installation_id ON forge_repository (installation_id);

CREATE TABLE webhook_delivery (
    id uuid PRIMARY KEY,
    delivery text NOT NULL UNIQUE,
    event text NOT NULL,
    payload jsonb NOT NULL,
    received_at timestamptz NOT NULL DEFAULT now(),
    attempts integer NOT NULL DEFAULT 0,
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    last_error text,
    processed_at timestamptz
);

CREATE INDEX webhook_delivery_pending ON webhook_delivery (next_attempt_at, id)
    WHERE processed_at IS NULL;

ALTER TABLE project ADD COLUMN repository_id bigint REFERENCES forge_repository (id);
