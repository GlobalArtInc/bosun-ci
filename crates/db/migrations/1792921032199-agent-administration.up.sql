ALTER TABLE agent ADD COLUMN drain boolean NOT NULL DEFAULT false;

ALTER TABLE agent ADD COLUMN deleted_at timestamptz;
