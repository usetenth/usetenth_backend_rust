ALTER TABLE signing_intents DROP CONSTRAINT signing_intents_kind_check;
ALTER TABLE signing_intents ADD CONSTRAINT signing_intents_kind_check CHECK (kind IN ('buy', 'sell', 'withdraw'));
DROP INDEX signing_intents_signed_idx;
CREATE INDEX signing_intents_signed_idx ON signing_intents (user_id, kind, signed_at) WHERE status = 'signed';

-- Sells are sized in qty_base (a decimal string); amount_micros is the estimated USD value.
ALTER TABLE orders ADD COLUMN side VARCHAR NOT NULL DEFAULT 'buy' CHECK (side IN ('buy', 'sell'));
ALTER TABLE orders ADD COLUMN qty_base VARCHAR;
ALTER TABLE orders ADD CONSTRAINT orders_sell_has_qty CHECK (side = 'buy' OR qty_base IS NOT NULL);

-- Where a user has said funds may be sent. A new address cannot be used until usable_at.
CREATE TABLE withdrawal_addresses (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    chain_family VARCHAR NOT NULL CHECK (chain_family IN ('evm', 'solana')),
    address VARCHAR NOT NULL,
    label VARCHAR,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    usable_at TIMESTAMPTZ NOT NULL,
    removed_at TIMESTAMPTZ
);
CREATE UNIQUE INDEX withdrawal_addresses_unique_idx ON withdrawal_addresses (user_id, chain_family, lower(address)) WHERE removed_at IS NULL;

-- Money leaving the platform; never retried once the gateway has been asked to execute.
CREATE TABLE withdrawals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    address_id UUID NOT NULL REFERENCES withdrawal_addresses(id),
    to_address VARCHAR NOT NULL,
    asset_key VARCHAR NOT NULL REFERENCES assets(asset_key),
    amount_micros BIGINT NOT NULL CHECK (amount_micros > 0),
    client_ref VARCHAR,
    status VARCHAR NOT NULL DEFAULT 'pending' CHECK (status IN
        ('pending', 'processing', 'submitted', 'awaiting_settlement', 'completed', 'failed', 'needs_review', 'dry_run')),
    gateway_transfer_id VARCHAR UNIQUE,
    tx_hash VARCHAR,
    failure_reason TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    notified_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX withdrawals_status_idx ON withdrawals (status, created_at);
CREATE INDEX withdrawals_user_idx ON withdrawals (user_id, created_at DESC);
CREATE UNIQUE INDEX withdrawals_client_ref_idx ON withdrawals (user_id, client_ref) WHERE client_ref IS NOT NULL;
