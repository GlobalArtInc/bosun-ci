-- migrator:no-transaction
CREATE UNIQUE INDEX CONCURRENTLY project_id_organisation_id ON project (id, organisation_id);
