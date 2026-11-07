CREATE TABLE upstream_subscription (
    id uuid PRIMARY KEY,
    workflow_id uuid NOT NULL REFERENCES workflow (id) ON DELETE CASCADE,
    revision_id uuid NOT NULL REFERENCES workflow_revision (id) ON DELETE CASCADE,
    trigger text NOT NULL,
    source_project text,
    filter jsonb NOT NULL,
    git_ref text,
    sha text,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workflow_id, trigger)
);

CREATE INDEX upstream_subscription_revision_id ON upstream_subscription (revision_id);

CREATE INDEX upstream_subscription_source_project ON upstream_subscription (source_project)
    WHERE source_project IS NOT NULL;

CREATE TABLE triggered_run (
    source_run_id uuid NOT NULL REFERENCES run (id) ON DELETE CASCADE,
    attempt_id uuid NOT NULL REFERENCES attempt (id) ON DELETE CASCADE,
    workflow_id uuid NOT NULL REFERENCES workflow (id) ON DELETE CASCADE,
    transition text NOT NULL,
    run_id uuid REFERENCES run (id) ON DELETE CASCADE,
    skipped text,
    depth integer NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (source_run_id, attempt_id, workflow_id, transition),
    CHECK (run_id IS NULL OR skipped IS NULL)
);

CREATE UNIQUE INDEX triggered_run_run_id ON triggered_run (run_id) WHERE run_id IS NOT NULL;

CREATE INDEX triggered_run_attempt_id ON triggered_run (attempt_id);

CREATE INDEX triggered_run_workflow_id ON triggered_run (workflow_id);

CREATE TABLE upstream_follower (
    project_id uuid NOT NULL REFERENCES project (id) ON DELETE CASCADE,
    follower_id uuid NOT NULL REFERENCES project (id) ON DELETE CASCADE,
    PRIMARY KEY (project_id, follower_id)
);

CREATE INDEX upstream_follower_follower_id ON upstream_follower (follower_id);
