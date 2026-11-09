CREATE TABLE kyc_session_records IF NOT EXISTS (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id VARCHAR NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name VARCHAR NOT NULL,
    nin_number VARCHAR NOT NULL,
    phone_number VARCHAR NOT NULL,
    country VARCHAR NOT NULL,
    file_url VARCHAR,
    status VARCHAR NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT now(),
    updated_at TIMESTAMP NOT NULL DEFAULT now(),
    expiry TIMESTAMP NOT NULL DEFAULT now(),
);