-- Durable command outcomes and scene events allow an ACK to be recovered after
-- a lost connection. Revisions are assigned while the scene row is locked.
CREATE TABLE scene_commands (
    session_id uuid NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    grant_id uuid NOT NULL,
    command_id uuid NOT NULL,
    body_hash bytea NOT NULL CHECK (octet_length(body_hash) = 32),
    outcome jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (session_id, grant_id, command_id)
);
CREATE INDEX scene_commands_age_idx ON scene_commands(created_at);

CREATE TABLE scene_events (
    scene_id uuid NOT NULL REFERENCES scenes(id) ON DELETE CASCADE,
    revision bigint NOT NULL CHECK (revision > 0),
    scene_epoch bigint NOT NULL CHECK (scene_epoch > 0),
    event jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (scene_id, revision)
);
