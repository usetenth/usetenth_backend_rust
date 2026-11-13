-- What the tenth rule decided for one deposit; UNIQUE(deposit_id) stops a deposit being invested twice.
CREATE TABLE investments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    deposit_id UUID NOT NULL UNIQUE REFERENCES deposits(id),
    deposit_micros BIGINT NOT NULL,
    invest_micros BIGINT NOT NULL DEFAULT 0,
    percentage SMALLINT NOT NULL,
    status VARCHAR NOT NULL CHECK (status IN ('pending', 'completed', 'partial', 'failed', 'skipped', 'simulated')),
    skip_reason VARCHAR,
    notified_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX investments_user_idx ON investments (user_id, created_at DESC);
