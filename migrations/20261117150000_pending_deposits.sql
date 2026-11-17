-- Incoming stablecoin transfers seen on the chain or listed as pending by the gateway, shown in the app until they are credited.
CREATE TABLE pending_deposits (
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  tx_hash VARCHAR NOT NULL,
  asset VARCHAR NOT NULL,
  amount_micros BIGINT,
  seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (user_id, tx_hash)
);
