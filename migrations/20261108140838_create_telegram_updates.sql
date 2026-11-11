CREATE TABLE telegram_updates (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  update_id BIGINT NOT NULL UNIQUE,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
