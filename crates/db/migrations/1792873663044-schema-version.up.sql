CREATE TABLE schema_version (
    major integer PRIMARY KEY,
    migrated_at timestamptz NOT NULL DEFAULT now()
);
