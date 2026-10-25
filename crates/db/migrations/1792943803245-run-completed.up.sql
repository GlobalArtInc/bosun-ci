-- migrator:no-transaction
CREATE INDEX CONCURRENTLY run_completed ON run (completed_at) WHERE status = 'completed';
