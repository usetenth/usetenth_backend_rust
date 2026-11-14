-- A confirmation can be approved with a button in Telegram, so nobody has to copy a code between apps.
ALTER TABLE action_confirmations ADD COLUMN approved_at TIMESTAMPTZ;
