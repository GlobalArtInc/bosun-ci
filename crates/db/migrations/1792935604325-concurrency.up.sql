ALTER TABLE run ADD COLUMN concurrency boolean NOT NULL DEFAULT false;

CREATE TABLE concurrency_member (
    id bigserial PRIMARY KEY,
    project_id uuid NOT NULL REFERENCES project (id) ON DELETE CASCADE,
    key text NOT NULL,
    run_id uuid NOT NULL REFERENCES run (id) ON DELETE CASCADE,
    job_id uuid,
    running boolean NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX concurrency_member_group ON concurrency_member (project_id, key, id);

CREATE INDEX concurrency_member_run ON concurrency_member (run_id);
