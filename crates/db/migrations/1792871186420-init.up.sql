CREATE TABLE organisation (
    id uuid PRIMARY KEY,
    slug text NOT NULL UNIQUE,
    name text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE project (
    id uuid PRIMARY KEY,
    organisation_id uuid NOT NULL REFERENCES organisation (id) ON DELETE CASCADE,
    slug text NOT NULL,
    name text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (organisation_id, slug)
);

CREATE TABLE "user" (
    id uuid PRIMARY KEY,
    login text NOT NULL UNIQUE,
    name text NOT NULL,
    email text,
    owner boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE api_token (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES "user" (id) ON DELETE CASCADE,
    name text NOT NULL,
    sha256 bytea NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    revoked_at timestamptz
);

CREATE INDEX api_token_user_id ON api_token (user_id);

CREATE TABLE workflow (
    id uuid PRIMARY KEY,
    project_id uuid NOT NULL REFERENCES project (id) ON DELETE CASCADE,
    name text NOT NULL,
    source_kind text NOT NULL,
    enabled boolean NOT NULL DEFAULT true,
    revision_id uuid,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX workflow_project_id ON workflow (project_id);

CREATE TABLE workflow_revision (
    id uuid PRIMARY KEY,
    workflow_id uuid NOT NULL REFERENCES workflow (id) ON DELETE CASCADE,
    sha256 bytea NOT NULL,
    source text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workflow_id, sha256)
);

ALTER TABLE workflow ADD FOREIGN KEY (revision_id) REFERENCES workflow_revision (id);

CREATE INDEX workflow_revision_id ON workflow (revision_id);

CREATE TABLE run (
    id uuid PRIMARY KEY,
    project_id uuid NOT NULL REFERENCES project (id) ON DELETE CASCADE,
    workflow_id uuid NOT NULL REFERENCES workflow (id) ON DELETE CASCADE,
    revision_id uuid NOT NULL REFERENCES workflow_revision (id),
    ir jsonb NOT NULL,
    number bigint NOT NULL,
    global_number bigserial UNIQUE,
    event text NOT NULL,
    trigger jsonb NOT NULL,
    ref text,
    sha text,
    inputs jsonb NOT NULL DEFAULT '{}',
    status text NOT NULL,
    conclusion text,
    created_at timestamptz NOT NULL DEFAULT now(),
    started_at timestamptz,
    completed_at timestamptz,
    UNIQUE (workflow_id, number)
);

CREATE INDEX run_project_id ON run (project_id);

CREATE INDEX run_revision_id ON run (revision_id);

CREATE TABLE attempt (
    id uuid PRIMARY KEY,
    run_id uuid NOT NULL REFERENCES run (id) ON DELETE CASCADE,
    number integer NOT NULL,
    actor jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (run_id, number)
);

CREATE TABLE job (
    id uuid PRIMARY KEY,
    attempt_id uuid NOT NULL REFERENCES attempt (id) ON DELETE CASCADE,
    key text NOT NULL,
    name text NOT NULL,
    needs text[] NOT NULL DEFAULT '{}',
    status text NOT NULL,
    waiting_reason text,
    conclusion text,
    conclusion_source text,
    priority integer NOT NULL DEFAULT 0,
    labels text[] NOT NULL DEFAULT '{}',
    pool_ids uuid[] NOT NULL DEFAULT '{}',
    queued_at timestamptz,
    started_at timestamptz,
    completed_at timestamptz
);

CREATE INDEX job_attempt_id ON job (attempt_id);

CREATE TABLE pool (
    id uuid PRIMARY KEY,
    name text NOT NULL UNIQUE,
    trust text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE agent (
    id uuid PRIMARY KEY,
    pool_id uuid NOT NULL REFERENCES pool (id),
    name text NOT NULL,
    credential_sha256 bytea NOT NULL UNIQUE,
    labels text[] NOT NULL DEFAULT '{}',
    capabilities text[] NOT NULL DEFAULT '{}',
    version text,
    online boolean NOT NULL DEFAULT false,
    last_seen_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX agent_pool_id ON agent (pool_id);

CREATE TABLE registration_token (
    id uuid PRIMARY KEY,
    pool_id uuid NOT NULL REFERENCES pool (id) ON DELETE CASCADE,
    sha256 bytea NOT NULL UNIQUE,
    reusable boolean NOT NULL,
    expires_at timestamptz NOT NULL,
    used_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX registration_token_pool_id ON registration_token (pool_id);

CREATE TABLE execution (
    id uuid PRIMARY KEY,
    job_id uuid NOT NULL REFERENCES job (id) ON DELETE CASCADE,
    number integer NOT NULL,
    agent_id uuid NOT NULL REFERENCES agent (id),
    lease_until timestamptz,
    status text NOT NULL,
    conclusion text,
    reason text,
    started_at timestamptz,
    completed_at timestamptz,
    UNIQUE (job_id, number)
);

CREATE INDEX execution_agent_id ON execution (agent_id);

CREATE TABLE step (
    id uuid PRIMARY KEY,
    execution_id uuid NOT NULL REFERENCES execution (id) ON DELETE CASCADE,
    index integer NOT NULL,
    name text NOT NULL,
    status text NOT NULL,
    outcome text,
    conclusion text,
    conclusion_source text,
    started_at timestamptz,
    completed_at timestamptz
);

CREATE INDEX step_execution_id ON step (execution_id);

CREATE TABLE run_event (
    seq bigserial PRIMARY KEY,
    run_id uuid NOT NULL REFERENCES run (id) ON DELETE CASCADE,
    attempt_id uuid,
    job_id uuid,
    execution_id uuid,
    step_id uuid,
    kind text NOT NULL,
    actor jsonb NOT NULL,
    data jsonb NOT NULL,
    at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX run_event_run_id_seq ON run_event (run_id, seq);

CREATE TABLE log_chunk (
    execution_id uuid NOT NULL REFERENCES execution (id) ON DELETE CASCADE,
    seq bigint NOT NULL,
    step_id uuid NOT NULL,
    data bytea NOT NULL,
    PRIMARY KEY (execution_id, seq)
);

CREATE TABLE log_object (
    step_id uuid PRIMARY KEY REFERENCES step (id) ON DELETE CASCADE,
    execution_id uuid NOT NULL,
    key text NOT NULL,
    size bigint NOT NULL,
    lines bigint NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE idempotency_key (
    principal text NOT NULL,
    key text NOT NULL,
    request_sha256 bytea NOT NULL,
    status integer NOT NULL,
    response jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (principal, key)
);
