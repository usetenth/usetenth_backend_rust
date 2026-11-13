CREATE TABLE telegram_sessions (
    telegram_id VARCHAR NOT NULL UNIQUE PRIMARY KEY,
    telegram_username VARCHAR,
    expires TIMESTAMPTZ
);
