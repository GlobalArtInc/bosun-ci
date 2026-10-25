-- migrator:no-transaction
CREATE INDEX CONCURRENTLY job_queued_or_waiting ON job (id) WHERE status IN ('queued', 'waiting');
