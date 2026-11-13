ALTER TABLE rules ADD COLUMN enabled_at TIMESTAMPTZ;
UPDATE rules SET enabled_at = updated_at WHERE enabled;

-- Tradable instruments, synced from the gateway. asset_key is the gateway asset id.
CREATE TABLE assets (
    asset_key VARCHAR PRIMARY KEY,
    symbol VARCHAR NOT NULL,
    name VARCHAR NOT NULL,
    chain VARCHAR,
    address VARCHAR,
    asset_class VARCHAR,
    tradable BOOLEAN NOT NULL DEFAULT false,
    stable BOOLEAN NOT NULL DEFAULT false,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Plan weights are basis points totalling 10000, enforced by the admin API.
CREATE TABLE plan_allocations (
    plan investment_plan NOT NULL,
    asset_key VARCHAR NOT NULL REFERENCES assets(asset_key),
    weight_bps INTEGER NOT NULL CHECK (weight_bps BETWEEN 1 AND 10000),
    PRIMARY KEY (plan, asset_key)
);

CREATE TABLE custom_selections (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    asset_key VARCHAR NOT NULL REFERENCES assets(asset_key),
    weight_bps INTEGER NOT NULL CHECK (weight_bps BETWEEN 1 AND 10000),
    PRIMARY KEY (user_id, asset_key)
);

-- One buy of one asset; anything uncertain after execute goes to needs_review, never a retry.
CREATE TABLE orders (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    investment_id UUID REFERENCES investments(id),
    kind VARCHAR NOT NULL CHECK (kind IN ('auto', 'manual')),
    client_ref VARCHAR,
    asset_key VARCHAR NOT NULL REFERENCES assets(asset_key),
    amount_micros BIGINT NOT NULL CHECK (amount_micros > 0),
    status VARCHAR NOT NULL DEFAULT 'pending' CHECK (status IN
        ('pending', 'processing', 'submitted', 'awaiting_settlement', 'completed', 'failed', 'needs_review', 'dry_run')),
    gateway_order_id VARCHAR UNIQUE,
    tx_hash VARCHAR,
    failure_reason TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX orders_status_idx ON orders (status, created_at);
CREATE INDEX orders_user_idx ON orders (user_id, created_at DESC);
CREATE UNIQUE INDEX orders_client_ref_idx ON orders (user_id, client_ref) WHERE client_ref IS NOT NULL;
