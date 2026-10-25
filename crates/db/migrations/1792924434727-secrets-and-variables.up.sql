CREATE TABLE secret (
    id uuid PRIMARY KEY,
    organisation_id uuid NOT NULL REFERENCES organisation (id) ON DELETE CASCADE,
    project_id uuid,
    name text NOT NULL,
    ciphertext bytea NOT NULL,
    data_key bytea NOT NULL,
    kek_id text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE NULLS NOT DISTINCT (organisation_id, project_id, name),
    FOREIGN KEY (project_id, organisation_id) REFERENCES project (id, organisation_id)
        ON DELETE CASCADE
);

CREATE INDEX secret_project_id ON secret (project_id);

CREATE TABLE variable (
    id uuid PRIMARY KEY,
    organisation_id uuid NOT NULL REFERENCES organisation (id) ON DELETE CASCADE,
    project_id uuid,
    name text NOT NULL,
    value text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE NULLS NOT DISTINCT (organisation_id, project_id, name),
    FOREIGN KEY (project_id, organisation_id) REFERENCES project (id, organisation_id)
        ON DELETE CASCADE
);

CREATE INDEX variable_project_id ON variable (project_id);
