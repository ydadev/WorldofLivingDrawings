-- A drawing reserves one scene slot only after the user chooses to publish it.
-- The intent survives retries and keeps the eventual fish ID until scene deletion.
CREATE TABLE upload_intents (
    id uuid PRIMARY KEY,
    session_id uuid NOT NULL,
    scene_id uuid NOT NULL,
    scene_epoch bigint NOT NULL CHECK (scene_epoch > 0),
    principal_kind text NOT NULL CHECK (principal_kind IN ('owner', 'controller')),
    principal_id uuid NOT NULL,
    fish_id uuid NOT NULL UNIQUE,
    definition_id text NOT NULL CHECK (definition_id IN ('coral-fish', 'stream-fish')),
    template_id text NOT NULL CHECK (template_id IN ('coral', 'stream')),
    template_version integer NOT NULL CHECK (template_version > 0),
    layout_hash text NOT NULL CHECK (layout_hash ~ '^[0-9a-f]{64}$'),
    source_kind text NOT NULL CHECK (source_kind IN ('paper', 'browser')),
    position_x real NOT NULL,
    position_y real NOT NULL,
    status text NOT NULL DEFAULT 'reserved' CHECK (status IN ('reserved', 'uploaded', 'finalized')),
    normalized_png bytea,
    paint_blob_id text,
    result_entity_id uuid,
    reserved_bytes integer NOT NULL DEFAULT 2097152 CHECK (reserved_bytes BETWEEN 1 AND 2097152),
    reservation_until timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (session_id, scene_id) REFERENCES scenes(session_id, id) ON DELETE CASCADE,
    CHECK (reservation_until <= expires_at),
    CHECK (status <> 'uploaded' OR normalized_png IS NOT NULL),
    CHECK (status <> 'finalized' OR (paint_blob_id IS NOT NULL AND result_entity_id IS NOT NULL))
);
CREATE INDEX upload_intents_scene_reservations_idx
    ON upload_intents(scene_id, reservation_until) WHERE status <> 'finalized';
CREATE INDEX upload_intents_principal_idx
    ON upload_intents(session_id, principal_kind, principal_id, reservation_until)
    WHERE status <> 'finalized';
