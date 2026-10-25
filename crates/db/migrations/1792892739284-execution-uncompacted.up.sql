-- migrator:no-transaction
CREATE INDEX CONCURRENTLY execution_uncompacted ON execution (completed_at) WHERE status = 'completed' AND log_compacted_at IS NULL;
