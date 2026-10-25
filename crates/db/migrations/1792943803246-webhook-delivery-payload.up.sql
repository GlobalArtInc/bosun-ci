-- migrator:no-transaction
CREATE INDEX CONCURRENTLY webhook_delivery_payload ON webhook_delivery (received_at) WHERE payload <> 'null'::jsonb;
