CREATE SEQUENCE artifact_id_seq AS bigint MINVALUE 1 MAXVALUE 9007199254740991 NO CYCLE;

CREATE TABLE artifact (
    id bigint PRIMARY KEY DEFAULT nextval('artifact_id_seq')
        CHECK (id BETWEEN 1 AND 9007199254740991),
    project_id uuid NOT NULL REFERENCES project (id) ON DELETE CASCADE,
    attempt_id uuid NOT NULL REFERENCES attempt (id) ON DELETE CASCADE,
    execution_id uuid NOT NULL REFERENCES execution (id) ON DELETE CASCADE,
    name text NOT NULL CHECK (octet_length(name) BETWEEN 1 AND 255),
    key text NOT NULL,
    content_type text NOT NULL,
    created_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    finalized_at timestamptz,
    size bigint CHECK (size >= 0),
    sha256 bytea CHECK (octet_length(sha256) = 32),
    blocks integer NOT NULL DEFAULT 0 CHECK (blocks BETWEEN 0 AND 50000),
    CHECK ((finalized_at IS NULL) = (size IS NULL)),
    CHECK (sha256 IS NULL OR finalized_at IS NOT NULL),
    UNIQUE (attempt_id, name)
);

CREATE INDEX artifact_project_id ON artifact (project_id);
CREATE INDEX artifact_execution_id ON artifact (execution_id);
CREATE INDEX artifact_expiry ON artifact (expires_at) WHERE finalized_at IS NOT NULL;
CREATE INDEX artifact_unfinished ON artifact (created_at) WHERE finalized_at IS NULL;

CREATE TABLE artifact_block (
    artifact_id bigint NOT NULL REFERENCES artifact (id) ON DELETE CASCADE,
    block text NOT NULL CHECK (octet_length(block) BETWEEN 1 AND 88),
    PRIMARY KEY (artifact_id, block)
);
