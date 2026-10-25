-- migrator:no-transaction
CREATE INDEX CONCURRENTLY execution_lease ON execution (lease_until) WHERE status = 'assigned';
