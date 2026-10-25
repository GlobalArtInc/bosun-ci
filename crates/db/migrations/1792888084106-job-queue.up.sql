-- migrator:no-transaction
CREATE INDEX CONCURRENTLY job_queue ON job (priority DESC, queued_at) WHERE status = 'queued';
