CREATE TABLE team (
    id uuid PRIMARY KEY,
    organisation_id uuid NOT NULL REFERENCES organisation (id) ON DELETE CASCADE,
    slug text NOT NULL,
    name text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (organisation_id, slug),
    UNIQUE (id, organisation_id)
);

CREATE TABLE team_member (
    team_id uuid NOT NULL REFERENCES team (id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES "user" (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (team_id, user_id)
);

CREATE INDEX team_member_user_id ON team_member (user_id);

CREATE TABLE role (
    id uuid PRIMARY KEY,
    organisation_id uuid REFERENCES organisation (id) ON DELETE CASCADE,
    name text NOT NULL,
    permissions text[] NOT NULL DEFAULT '{}',
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE NULLS NOT DISTINCT (organisation_id, name)
);

CREATE TABLE role_binding (
    id uuid PRIMARY KEY,
    role_id uuid NOT NULL REFERENCES role (id) ON DELETE CASCADE,
    organisation_id uuid NOT NULL REFERENCES organisation (id) ON DELETE CASCADE,
    project_id uuid,
    user_id uuid REFERENCES "user" (id) ON DELETE CASCADE,
    team_id uuid,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((user_id IS NULL) <> (team_id IS NULL)),
    UNIQUE NULLS NOT DISTINCT (organisation_id, project_id, role_id, user_id, team_id),
    FOREIGN KEY (project_id, organisation_id) REFERENCES project (id, organisation_id)
        ON DELETE CASCADE,
    FOREIGN KEY (team_id, organisation_id) REFERENCES team (id, organisation_id) ON DELETE CASCADE
);

CREATE INDEX role_binding_role_id ON role_binding (role_id);

CREATE INDEX role_binding_project_id ON role_binding (project_id);

CREATE INDEX role_binding_user_id ON role_binding (user_id);

CREATE INDEX role_binding_team_id ON role_binding (team_id);

INSERT INTO role (id, name, permissions) VALUES
    ('01a171a0-24b8-7200-9505-840c6f3de8ec', 'viewer', '{project.view}'),
    ('01a171a0-24b9-7d00-add7-f039b5cf197f', 'developer',
     '{project.view,run.dispatch,run.cancel,run.rerun,run.control}'),
    ('01a171a0-24ba-7c00-8ce5-e6349be18ef2', 'maintainer',
     '{project.view,run.dispatch,run.cancel,run.rerun,run.control,run.override,job.debug,'
     'workflow.edit,workflow.commit,workflow.manage,secret.manage,variable.manage,'
     'environment.manage}'),
    ('01a171a0-24bb-7f00-9edb-fbcef86fa6a5', 'admin',
     '{project.view,run.dispatch,run.cancel,run.rerun,run.control,run.override,job.debug,'
     'workflow.edit,workflow.commit,workflow.manage,secret.manage,variable.manage,'
     'environment.manage,environment.bypass,agent.manage,plugin.manage,project.manage,'
     'organisation.manage,audit.view}');
