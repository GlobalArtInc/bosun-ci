-- migrator:no-transaction
CREATE UNIQUE INDEX CONCURRENTLY workflow_repository_file ON workflow (project_id, name)
    WHERE source_kind = 'repository';
