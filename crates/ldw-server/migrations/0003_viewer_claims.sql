-- A TV requests a short display code. The Owner approves it on another device;
-- only the original browser, holding the HttpOnly claim cookie, receives a Viewer grant.
CREATE TABLE viewer_claims (
    id uuid PRIMARY KEY,
    session_id uuid NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    claim_hash bytea NOT NULL UNIQUE CHECK (octet_length(claim_hash) = 32),
    csrf_hash bytea NOT NULL CHECK (octet_length(csrf_hash) = 32),
    display_code_hash bytea NOT NULL CHECK (octet_length(display_code_hash) = 32),
    approved_role text CHECK (approved_role IN ('viewer', 'viewer_interact')),
    expires_at timestamptz NOT NULL,
    consumed_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX viewer_claims_session_idx ON viewer_claims(session_id, expires_at);
