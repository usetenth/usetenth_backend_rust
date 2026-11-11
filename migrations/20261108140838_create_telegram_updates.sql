CREATE TABLE telegram_updates (
  id UUID UNIQUE PRIMARY KEY DEFAULT,
  update_id VARCHAR,
  created_at TIMESTAMPZ
);