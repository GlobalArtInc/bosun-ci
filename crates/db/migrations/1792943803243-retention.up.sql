ALTER TABLE project
    ADD COLUMN log_retention integer NOT NULL DEFAULT 90,
    ADD COLUMN run_retention integer;

ALTER TABLE execution ADD COLUMN logs_expired_at timestamptz;

CREATE TABLE expired_object (
    key text PRIMARY KEY,
    created_at timestamptz NOT NULL DEFAULT now()
);
