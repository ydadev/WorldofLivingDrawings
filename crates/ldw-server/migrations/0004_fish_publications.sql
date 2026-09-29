-- Trusted uploads enter a durable inbox. The scene worker applies each batch
-- with its live checkpoint and matching durable Entity/events in one transaction.
CREATE TABLE fish_publications (
    scene_id uuid NOT NULL REFERENCES scenes(id) ON DELETE CASCADE,
    fish_id uuid NOT NULL,
    definition_id text NOT NULL CHECK (definition_id IN ('coral-fish', 'stream-fish')),
    paint_blob_id text NOT NULL,
    position_x real NOT NULL,
    position_y real NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (scene_id, fish_id)
);
CREATE INDEX fish_publications_order_idx ON fish_publications(scene_id, created_at, fish_id);
