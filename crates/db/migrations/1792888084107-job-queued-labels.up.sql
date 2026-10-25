-- migrator:no-transaction
CREATE INDEX CONCURRENTLY job_queued_labels ON job USING gin (labels) WHERE status = 'queued';
