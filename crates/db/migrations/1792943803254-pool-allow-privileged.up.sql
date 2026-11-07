ALTER TABLE pool ADD COLUMN allow_privileged boolean NOT NULL DEFAULT false;

ALTER TABLE pool ADD CONSTRAINT pool_untrusted_allows_no_privileged
    CHECK (NOT allow_privileged OR trust = 'trusted');
