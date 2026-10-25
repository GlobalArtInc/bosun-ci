-- migrator:no-transaction
CREATE INDEX CONCURRENTLY stream_event_agent_status ON stream_event (at) WHERE kind = 'agent.status';
