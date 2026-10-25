CREATE TABLE schedule (
    id uuid PRIMARY KEY,
    workflow_id uuid NOT NULL REFERENCES workflow (id) ON DELETE CASCADE,
    revision_id uuid NOT NULL REFERENCES workflow_revision (id) ON DELETE CASCADE,
    expression text NOT NULL,
    timezone text NOT NULL,
    cron jsonb NOT NULL,
    git_ref text,
    sha text,
    actor jsonb NOT NULL,
    next_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workflow_id, expression, timezone)
);

CREATE INDEX schedule_revision_id ON schedule (revision_id);

CREATE INDEX schedule_due ON schedule (next_at, id) WHERE next_at IS NOT NULL;
