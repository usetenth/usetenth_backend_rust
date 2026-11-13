-- Mobile users can sign up with email + password and have no Telegram account yet.
ALTER TABLE users ALTER COLUMN telegram_id DROP NOT NULL;
CREATE UNIQUE INDEX users_email_lower_idx ON users (lower(email)) WHERE email IS NOT NULL;

-- Refresh tokens are hashed and rotated on use; replaying a revoked one revokes its whole login.
CREATE TABLE auth_refresh_tokens (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  family_id UUID NOT NULL,
  token_hash VARCHAR(64) NOT NULL UNIQUE,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  expires_at TIMESTAMPTZ NOT NULL,
  revoked_at TIMESTAMPTZ
);
CREATE INDEX auth_refresh_tokens_family_idx ON auth_refresh_tokens (family_id);

-- One-time sign-in and link codes from the bot: hashed, single use, short lived.
CREATE TABLE telegram_login_codes (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  telegram_id BIGINT NOT NULL,
  username VARCHAR,
  code_hash VARCHAR(64) NOT NULL UNIQUE,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  expires_at TIMESTAMPTZ NOT NULL,
  used_at TIMESTAMPTZ
);

-- One-time codes confirming a single irreversible action; bound to that action and burned after repeated wrong tries.
CREATE TABLE action_confirmations (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  purpose_hash VARCHAR(64) NOT NULL,
  code_hash VARCHAR(64) NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  expires_at TIMESTAMPTZ NOT NULL,
  used_at TIMESTAMPTZ
);
CREATE INDEX action_confirmations_user_idx ON action_confirmations (user_id, created_at DESC);
