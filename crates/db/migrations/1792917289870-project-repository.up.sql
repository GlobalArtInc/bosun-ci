-- migrator:no-transaction
CREATE UNIQUE INDEX CONCURRENTLY project_repository_id ON project (repository_id);
