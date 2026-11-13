-- ORGANIZATIONS AND DEVICE KEYS: admin-managed enrollment and revocation for genesis signers.

CREATE TABLE organizations (
    id         BIGSERIAL PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE CHECK (length(trim(name)) > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE devices (
    public_key     BYTEA PRIMARY KEY CHECK (octet_length(public_key) = 32),
    organization_id BIGINT NOT NULL REFERENCES organizations(id),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at     TIMESTAMPTZ
);
CREATE INDEX devices_organization_idx ON devices (organization_id);
