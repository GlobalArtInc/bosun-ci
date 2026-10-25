ALTER TABLE job ADD COLUMN group_id uuid;

ALTER TABLE job ADD COLUMN matrix jsonb;

ALTER TABLE job ADD COLUMN strategy jsonb;
