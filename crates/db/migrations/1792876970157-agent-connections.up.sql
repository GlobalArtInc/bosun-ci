ALTER TABLE agent ADD COLUMN replica_id uuid;

CREATE TABLE control_message (
    id uuid PRIMARY KEY,
    execution_id uuid NOT NULL REFERENCES execution (id) ON DELETE CASCADE,
    kind text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    acknowledged_at timestamptz,
    took_effect boolean
);

CREATE INDEX control_message_execution_id ON control_message (execution_id);
