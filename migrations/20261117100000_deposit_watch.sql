-- When each user's incoming payments were last looked up at the gateway.
CREATE TABLE deposit_checks (
  user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  last_checked_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Set once the wallet address has been registered with the chain webhook provider.
ALTER TABLE wallets ADD COLUMN watched_at TIMESTAMPTZ;

CREATE INDEX wallets_address_lower_idx ON wallets (lower(address));
