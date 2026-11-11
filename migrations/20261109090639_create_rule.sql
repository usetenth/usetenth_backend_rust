CREATE TYPE investment_plan AS ENUM ('steady', 'growth', 'tech', 'custom');

-- One tenth rule per user. It stays disabled until the user passes KYC.
CREATE TABLE rules (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id UUID NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
  plan investment_plan NOT NULL,
  percentage SMALLINT NOT NULL CHECK (percentage BETWEEN 0 AND 100),
  enabled BOOLEAN NOT NULL DEFAULT false,
  token_type TEXT[] NOT NULL DEFAULT '{}',
  fixed_amount BIGINT NOT NULL DEFAULT 0,
  minimum_amount BIGINT NOT NULL DEFAULT 0,
  max_per_transaction BIGINT,
  max_daily_amount BIGINT,
  expires_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
