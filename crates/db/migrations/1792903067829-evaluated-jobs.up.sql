ALTER TABLE run ADD COLUMN display_name text;

UPDATE run SET display_name = coalesce(ir ->> 'name', (SELECT w.name FROM workflow w WHERE w.id = run.workflow_id));

ALTER TABLE job ADD COLUMN continue_on_error boolean NOT NULL DEFAULT false;

ALTER TABLE job ADD COLUMN evaluated jsonb;

ALTER TABLE job ADD COLUMN outputs jsonb NOT NULL DEFAULT '{}';
