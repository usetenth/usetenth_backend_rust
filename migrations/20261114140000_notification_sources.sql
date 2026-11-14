-- Withdrawal and investment notices share the table, keyed by what they are about so each is written once.
ALTER TABLE notifications ADD COLUMN source_id UUID;
CREATE UNIQUE INDEX notifications_source_idx ON notifications (kind, source_id);
