ALTER TABLE execution ADD COLUMN log_bytes bigint NOT NULL DEFAULT 0;

ALTER TABLE execution ADD COLUMN log_compacted_at timestamptz;

ALTER TABLE log_chunk ADD COLUMN dropped bigint NOT NULL DEFAULT 0;

ALTER TABLE log_object ADD COLUMN frames bigint[] NOT NULL DEFAULT '{}';
