CREATE TYPE verification_status AS ENUM ('unverified', 'review', 'verified');

CREATE TABLE users (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  telegram_id BIGINT NOT NULL UNIQUE,
  username VARCHAR,
  email VARCHAR,
  password VARCHAR,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  verified verification_status NOT NULL DEFAULT 'unverified',
  gateway_user_id VARCHAR UNIQUE
);
