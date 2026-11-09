CREATE TYPE verification_status AS ENUM ('unverified', 'review', 'verified');

CREATE TABLE users (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  telegram_id BIGINT NOT NULL UNIQUE,
  username VARCHAR,
  email VARCHAR,
  password VARCHAR,
  created_at TIMESTAMP,
  updated_at TIMESTAMP,
  verified verification_status,
  external_ref_id VARCHAR UNIQUE
);