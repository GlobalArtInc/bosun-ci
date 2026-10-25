ALTER TABLE run ADD COLUMN trusted boolean NOT NULL DEFAULT true;

ALTER TABLE job ADD COLUMN capabilities text[] NOT NULL DEFAULT '{}';

CREATE TABLE pool_access (
    id uuid PRIMARY KEY,
    pool_id uuid NOT NULL REFERENCES pool (id) ON DELETE CASCADE,
    organisation_id uuid REFERENCES organisation (id) ON DELETE CASCADE,
    project_id uuid,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (project_id IS NULL OR organisation_id IS NOT NULL),
    UNIQUE NULLS NOT DISTINCT (pool_id, organisation_id, project_id),
    FOREIGN KEY (project_id, organisation_id) REFERENCES project (id, organisation_id)
        ON DELETE CASCADE
);

CREATE INDEX pool_access_organisation_id ON pool_access (organisation_id);

CREATE INDEX pool_access_project_id ON pool_access (project_id);

INSERT INTO pool_access (id, pool_id) VALUES
    ('01a16f73-3ff5-7c11-8a42-5d1e0c7b3f21', '01a16f73-3ff4-7bb5-a8cf-f165bd9cefbb');

CREATE TABLE label_alias (
    id uuid PRIMARY KEY,
    organisation_id uuid REFERENCES organisation (id) ON DELETE CASCADE,
    label text NOT NULL,
    pool_id uuid REFERENCES pool (id),
    capability text,
    image text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE NULLS NOT DISTINCT (organisation_id, label)
);

CREATE INDEX label_alias_pool_id ON label_alias (pool_id);
