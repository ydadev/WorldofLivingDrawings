CREATE TABLE accounts (
    id uuid PRIMARY KEY,
    login text NOT NULL UNIQUE CHECK (length(login) BETWEEN 3 AND 64),
    role text NOT NULL CHECK (role IN ('admin', 'owner')),
    password_hash text NOT NULL,
    disabled_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE sessions (
    id uuid PRIMARY KEY,
    owner_id uuid NOT NULL REFERENCES accounts(id) ON DELETE RESTRICT,
    status text NOT NULL DEFAULT 'running' CHECK (status IN ('running', 'paused', 'closed')),
    active_scene_id uuid,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (id, owner_id)
);
CREATE INDEX sessions_owner_idx ON sessions(owner_id);

CREATE TABLE scenes (
    id uuid PRIMARY KEY,
    session_id uuid NOT NULL REFERENCES sessions(id) ON DELETE RESTRICT,
    world_id text NOT NULL,
    world_version integer NOT NULL CHECK (world_version > 0),
    scene_epoch bigint NOT NULL DEFAULT 1 CHECK (scene_epoch > 0),
    revision bigint NOT NULL DEFAULT 0 CHECK (revision >= 0),
    simulation_tick bigint NOT NULL DEFAULT 0 CHECK (simulation_tick >= 0),
    state jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (session_id, id)
);
CREATE INDEX scenes_session_idx ON scenes(session_id);
ALTER TABLE sessions ADD CONSTRAINT active_scene_belongs_to_session
    FOREIGN KEY (id, active_scene_id) REFERENCES scenes(session_id, id)
    DEFERRABLE INITIALLY IMMEDIATE;

CREATE TABLE pair_invitations (
    session_id uuid PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    qr_hash bytea NOT NULL CHECK (octet_length(qr_hash) = 32),
    pin_hash bytea NOT NULL CHECK (octet_length(pin_hash) = 32),
    generation bigint NOT NULL DEFAULT 1 CHECK (generation > 0),
    approval_required boolean NOT NULL DEFAULT false,
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE device_grants (
    id uuid PRIMARY KEY,
    session_id uuid NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    participant_id uuid,
    role text NOT NULL CHECK (role IN ('viewer', 'viewer_interact', 'controller')),
    token_hash bytea NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    issued_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    last_activity_at timestamptz NOT NULL DEFAULT now(),
    last_heartbeat_at timestamptz,
    revoked_at timestamptz,
    CHECK ((role = 'controller') = (participant_id IS NOT NULL))
);
CREATE INDEX device_grants_session_idx ON device_grants(session_id, role);

CREATE TABLE owner_grants (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    token_hash bytea NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    csrf_hash bytea NOT NULL CHECK (octet_length(csrf_hash) = 32),
    issued_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    revoked_at timestamptz
);
CREATE INDEX owner_grants_account_idx ON owner_grants(account_id);

CREATE TABLE pair_attempts (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    session_id uuid NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    client_key_hash bytea NOT NULL CHECK (octet_length(client_key_hash) = 32),
    ip_hash bytea NOT NULL CHECK (octet_length(ip_hash) = 32),
    attempted_at timestamptz NOT NULL DEFAULT now(),
    accepted boolean NOT NULL DEFAULT false
);
CREATE INDEX pair_attempts_session_time_idx ON pair_attempts(session_id, attempted_at DESC);
CREATE INDEX pair_attempts_client_time_idx ON pair_attempts(client_key_hash, attempted_at DESC);
CREATE INDEX pair_attempts_ip_time_idx ON pair_attempts(ip_hash, attempted_at DESC);
