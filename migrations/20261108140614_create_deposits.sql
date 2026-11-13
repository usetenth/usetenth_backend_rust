-- A completed stablecoin transfer in; source_id is the gateway's id, so detecting it twice is harmless.
CREATE TABLE deposits (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    source_id VARCHAR NOT NULL UNIQUE,
    tx_hash VARCHAR,
    chain VARCHAR,
    asset VARCHAR NOT NULL,
    amount_micros BIGINT NOT NULL CHECK (amount_micros > 0),
    observed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX deposits_user_idx ON deposits (user_id, created_at DESC);
