ALTER TABLE api_token ADD COLUMN permissions text[];

ALTER TABLE api_token ADD COLUMN restricted boolean NOT NULL DEFAULT false;

CREATE TABLE api_token_scope (
    id uuid PRIMARY KEY,
    token_id uuid NOT NULL REFERENCES api_token (id) ON DELETE CASCADE,
    organisation_id uuid NOT NULL REFERENCES organisation (id) ON DELETE CASCADE,
    project_id uuid,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE NULLS NOT DISTINCT (token_id, organisation_id, project_id),
    FOREIGN KEY (project_id, organisation_id) REFERENCES project (id, organisation_id)
        ON DELETE CASCADE
);

CREATE INDEX api_token_scope_organisation_id ON api_token_scope (organisation_id);

CREATE INDEX api_token_scope_project_id ON api_token_scope (project_id);
