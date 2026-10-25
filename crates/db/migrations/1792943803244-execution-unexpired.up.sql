-- migrator:no-transaction
CREATE INDEX CONCURRENTLY execution_unexpired ON execution (completed_at) WHERE status = 'completed' AND logs_expired_at IS NULL;
