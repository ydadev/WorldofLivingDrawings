-- Immutable normalized PNG catalog. A scene is charged once for each distinct
-- referenced blob even when another scene already stores identical pixels.
CREATE TABLE paint_blobs (
    id text PRIMARY KEY CHECK (id ~ '^[0-9a-f]{64}$'),
    byte_size integer NOT NULL CHECK (byte_size BETWEEN 1 AND 2097152),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE scene_paint_blobs (
    scene_id uuid NOT NULL REFERENCES scenes(id) ON DELETE CASCADE,
    blob_id text NOT NULL REFERENCES paint_blobs(id) ON DELETE RESTRICT,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (scene_id, blob_id)
);
CREATE INDEX scene_paint_blobs_blob_idx ON scene_paint_blobs(blob_id);

ALTER TABLE upload_intents ADD CONSTRAINT upload_intents_paint_blob_fk
    FOREIGN KEY (paint_blob_id) REFERENCES paint_blobs(id) ON DELETE RESTRICT;
