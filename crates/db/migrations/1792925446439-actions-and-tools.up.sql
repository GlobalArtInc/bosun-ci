CREATE TABLE action_ref (
    id uuid PRIMARY KEY,
    owner text NOT NULL,
    repository text NOT NULL,
    sha text NOT NULL,
    key text NOT NULL,
    size bigint NOT NULL,
    sha256 bytea NOT NULL,
    metadata jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (owner, repository, sha)
);

CREATE TABLE tool_object (
    id uuid PRIMARY KEY,
    tool text NOT NULL,
    version text NOT NULL,
    platform text NOT NULL,
    key text NOT NULL,
    size bigint NOT NULL,
    sha256 bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tool, version, platform)
);

ALTER TABLE job ADD COLUMN actions jsonb;
