-- migrator:no-transaction
CREATE INDEX CONCURRENTLY run_event_xact_seq ON run_event (xact, seq);
