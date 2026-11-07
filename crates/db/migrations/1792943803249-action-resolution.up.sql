ALTER TABLE organisation ADD COLUMN action_policy jsonb NOT NULL DEFAULT '{"mode": "all"}';

ALTER TABLE job ADD COLUMN remote_uses boolean NOT NULL DEFAULT false;

ALTER TABLE job ADD COLUMN resolving_since timestamptz;

ALTER TABLE job ADD COLUMN resolve_after timestamptz;

ALTER TABLE job ADD COLUMN resolve_attempts integer NOT NULL DEFAULT 0;

ALTER TABLE job ADD COLUMN resolve_claim uuid;

ALTER TABLE job ADD COLUMN resolve_lease_until timestamptz;
