-- The scene worker is the only writer that applies a queued structural change
-- to the simulation checkpoint and visible Entity projection.
CREATE TABLE fish_mutations (
    scene_id uuid NOT NULL REFERENCES scenes(id) ON DELETE CASCADE,
    fish_id uuid NOT NULL,
    command_id uuid NOT NULL,
    operation text NOT NULL CHECK (operation IN ('delete', 'restore')),
    accepted_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (scene_id, command_id),
    UNIQUE (scene_id, fish_id)
);
CREATE INDEX fish_mutations_order_idx ON fish_mutations(scene_id, accepted_at, command_id);

-- A trashed fish retains its immutable paint and the state needed to restore
-- its identity, position and capabilities. Behavior targets are reset on return.
CREATE TABLE fish_trash (
    scene_id uuid NOT NULL REFERENCES scenes(id) ON DELETE CASCADE,
    fish_id uuid NOT NULL,
    entity jsonb NOT NULL CHECK (jsonb_typeof(entity) = 'object'),
    fish_state jsonb NOT NULL CHECK (jsonb_typeof(fish_state) = 'object'),
    paint_blob_id text NOT NULL REFERENCES paint_blobs(id) ON DELETE RESTRICT,
    deleted_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL DEFAULT (now() + interval '7 days'),
    PRIMARY KEY (scene_id, fish_id),
    CHECK (expires_at > deleted_at)
);
CREATE INDEX fish_trash_expiry_idx ON fish_trash(expires_at);
CREATE INDEX fish_trash_blob_idx ON fish_trash(scene_id, paint_blob_id);

-- A finalized intent is a durable idempotency tombstone. Its blob hash may
-- outlive the physical paint after the last active/trash reference expires.
ALTER TABLE upload_intents DROP CONSTRAINT upload_intents_paint_blob_fk;
