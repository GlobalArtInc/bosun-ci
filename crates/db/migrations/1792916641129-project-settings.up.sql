ALTER TABLE project
    ADD COLUMN github_workflows boolean NOT NULL DEFAULT false,
    ADD COLUMN fork_policy text NOT NULL DEFAULT 'disabled',
    ADD COLUMN token_permissions jsonb NOT NULL DEFAULT '{"contents": "read", "metadata": "read"}',
    ADD COLUMN report text NOT NULL DEFAULT 'checks',
    ADD COLUMN settings_version bigint NOT NULL DEFAULT 1;
