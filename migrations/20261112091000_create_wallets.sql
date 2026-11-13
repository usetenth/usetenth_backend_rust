CREATE TABLE wallets (
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  address VARCHAR NOT NULL,
  network VARCHAR NOT NULL CHECK (network IN ('evm', 'solana')),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (user_id, network)
);
