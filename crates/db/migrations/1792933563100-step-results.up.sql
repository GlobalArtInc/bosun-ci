CREATE TABLE annotation (
    step_id uuid NOT NULL REFERENCES step (id) ON DELETE CASCADE,
    ordinal integer NOT NULL,
    kind text NOT NULL,
    message text NOT NULL,
    title text,
    file text,
    line bigint,
    end_line bigint,
    start_column bigint,
    end_column bigint,
    PRIMARY KEY (step_id, ordinal)
);

CREATE TABLE step_summary (
    step_id uuid PRIMARY KEY REFERENCES step (id) ON DELETE CASCADE,
    key text NOT NULL,
    size bigint NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

ALTER TABLE forge_check_run ADD COLUMN annotations bigint NOT NULL DEFAULT 0;
