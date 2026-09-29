CREATE TABLE owner_login_attempts (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    login_hash bytea NOT NULL CHECK (octet_length(login_hash) = 32),
    ip_hash bytea NOT NULL CHECK (octet_length(ip_hash) = 32),
    attempted_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX owner_login_attempts_login_time_idx
    ON owner_login_attempts(login_hash, attempted_at DESC);
CREATE INDEX owner_login_attempts_ip_time_idx
    ON owner_login_attempts(ip_hash, attempted_at DESC);
CREATE INDEX owner_login_attempts_time_idx
    ON owner_login_attempts(attempted_at);
