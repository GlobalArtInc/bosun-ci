ALTER TABLE project
    ADD COLUMN workspace text NOT NULL DEFAULT 'persist'
        CONSTRAINT project_workspace_known CHECK (workspace IN ('persist', 'clean'));
