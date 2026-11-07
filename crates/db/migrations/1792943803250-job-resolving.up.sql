-- migrator:no-transaction
CREATE INDEX CONCURRENTLY job_resolving ON job (id) WHERE status = 'waiting' AND waiting_reason = 'resolving';
