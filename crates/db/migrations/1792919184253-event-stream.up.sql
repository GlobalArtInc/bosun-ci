ALTER TABLE run_event ADD COLUMN xact bigint NOT NULL DEFAULT 0;

ALTER TABLE run_event ALTER COLUMN xact SET DEFAULT pg_current_xact_id()::text::bigint;

CREATE TABLE stream_event (
    seq bigserial PRIMARY KEY,
    xact bigint NOT NULL DEFAULT pg_current_xact_id()::text::bigint,
    subject uuid NOT NULL,
    project_id uuid REFERENCES project (id) ON DELETE CASCADE,
    run_id uuid REFERENCES run (id) ON DELETE CASCADE,
    kind text NOT NULL,
    data jsonb NOT NULL,
    at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX stream_event_xact_seq ON stream_event (xact, seq);

CREATE INDEX stream_event_subject_seq ON stream_event (subject, seq);

CREATE INDEX stream_event_project_id ON stream_event (project_id);

CREATE INDEX stream_event_run_id ON stream_event (run_id);
