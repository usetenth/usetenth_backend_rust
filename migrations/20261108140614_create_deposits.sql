CREATE TABLE deposits (
    id UUID PRIMARY KEY,
    amount INT NOT NULL,
    asset VARCHAR,
    created_at TIMESTAMPZ
);