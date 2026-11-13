CREATE TABLE signer_keys (
  user_id UUID PRIMARY KEY REFERENCES users(id),
  public_key VARCHAR(66) NOT NULL UNIQUE,
  wrapped_key BYTEA NOT NULL,
  nonce BYTEA NOT NULL,
  key_version INTEGER NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  revoked_at TIMESTAMPTZ
);

CREATE TABLE signing_intents (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id UUID NOT NULL REFERENCES users(id),
  order_id VARCHAR NOT NULL,
  kind VARCHAR NOT NULL CHECK (kind = 'buy'),
  amount_micros BIGINT NOT NULL CHECK (amount_micros > 0),
  status VARCHAR NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'signed')),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  expires_at TIMESTAMPTZ NOT NULL,
  signed_at TIMESTAMPTZ,
  UNIQUE (user_id, order_id)
);

CREATE INDEX signing_intents_signed_idx ON signing_intents (user_id, signed_at) WHERE status = 'signed';

CREATE TABLE signing_audit (
  id BIGSERIAL PRIMARY KEY,
  user_id UUID,
  intent_id UUID,
  outcome VARCHAR NOT NULL,
  reason TEXT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
