-- Persistence schema for the next milestone (server currently uses an in-memory store).
-- Plain PostgreSQL; the TimescaleDB hypertable at the bottom is optional.

CREATE TABLE products (
    product_id   BYTEA PRIMARY KEY CHECK (octet_length(product_id) = 16),
    custodian    BYTEA NOT NULL CHECK (octet_length(custodian) = 32),
    head_hash    BYTEA NOT NULL CHECK (octet_length(head_hash) = 32),
    event_count  INTEGER NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Stores the exact signed bytes so any auditor can re-verify without trusting the DB.
CREATE TABLE events (
    product_id   BYTEA   NOT NULL REFERENCES products(product_id),
    seq          INTEGER NOT NULL,
    event_hash   BYTEA   NOT NULL CHECK (octet_length(event_hash) = 32),
    event_bytes  BYTEA   NOT NULL,
    signature    BYTEA   NOT NULL CHECK (octet_length(signature) = 64),
    actor_key    BYTEA   NOT NULL CHECK (octet_length(actor_key) = 32),
    kind         SMALLINT NOT NULL,
    lat_e6       INTEGER NOT NULL,
    lon_e6       INTEGER NOT NULL,
    device_ts    BIGINT  NOT NULL,
    received_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (product_id, seq)
);
CREATE INDEX events_actor_idx ON events (actor_key);

-- Append-only: block edits and deletes at the database level.
CREATE FUNCTION events_immutable() RETURNS trigger AS $$
BEGIN RAISE EXCEPTION 'events are append-only'; END; $$ LANGUAGE plpgsql;
CREATE TRIGGER events_no_update BEFORE UPDATE OR DELETE ON events
    FOR EACH ROW EXECUTE FUNCTION events_immutable();

-- Optional, if TimescaleDB is installed (needs received_at in the key for chunking):
-- SELECT create_hypertable('events', 'received_at', migrate_data => true);
