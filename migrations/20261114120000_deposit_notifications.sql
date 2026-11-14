-- Every deposit is announced once, whether or not anything was invested from it.
ALTER TABLE deposits ADD COLUMN notified_at TIMESTAMPTZ;

-- What the app shows in its notification list; one row per deposit, so retries never duplicate it.
CREATE TABLE notifications (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind VARCHAR NOT NULL,
    title VARCHAR NOT NULL,
    body TEXT NOT NULL,
    deposit_id UUID UNIQUE REFERENCES deposits(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    read_at TIMESTAMPTZ
);
CREATE INDEX notifications_user_idx ON notifications (user_id, created_at DESC);
