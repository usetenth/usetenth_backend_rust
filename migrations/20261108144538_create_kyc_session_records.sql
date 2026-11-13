-- One identity check per row; the NIN is kept encrypted only until a decision, then wiped, leaving only its salted hash.
CREATE TABLE IF NOT EXISTS kyc_session_records (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    full_name VARCHAR NOT NULL,
    nin_hash VARCHAR(64) NOT NULL,
    nin_masked VARCHAR NOT NULL,
    nin_encrypted TEXT,
    status VARCHAR NOT NULL CHECK (status IN ('pending', 'verified', 'review', 'failed')),
    reason VARCHAR,
    vendor VARCHAR,
    name_match_hits INTEGER,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX kyc_session_records_user_idx ON kyc_session_records (user_id, created_at DESC);
CREATE UNIQUE INDEX kyc_session_records_open_idx ON kyc_session_records (user_id) WHERE status IN ('pending', 'review');
CREATE UNIQUE INDEX kyc_session_records_verified_nin_idx ON kyc_session_records (nin_hash) WHERE status = 'verified';

-- Billable vendor lookups, counted per user and per NIN for the daily limits.
CREATE TABLE kyc_lookups (
    id BIGSERIAL PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    nin_hash VARCHAR(64) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX kyc_lookups_user_idx ON kyc_lookups (user_id, created_at);
CREATE INDEX kyc_lookups_nin_idx ON kyc_lookups (nin_hash, created_at);

-- NIN hashes that are never verified, whoever presents them.
CREATE TABLE kyc_blocklist (
    nin_hash VARCHAR(64) PRIMARY KEY,
    reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
