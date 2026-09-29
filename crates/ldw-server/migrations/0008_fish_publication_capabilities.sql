-- Freeze the accepted entity definition while publication waits for the scene runner.
-- Existing queued v1 fish retain the original two capabilities.
ALTER TABLE fish_publications
    ADD COLUMN definition_version integer NOT NULL DEFAULT 1 CHECK (definition_version = 1),
    ADD COLUMN capabilities jsonb NOT NULL DEFAULT '{"consume_food":true,"avoid_threat":true}'::jsonb
        CHECK (jsonb_typeof(capabilities) = 'object'
            AND capabilities ? 'consume_food'
            AND capabilities ? 'avoid_threat'
            AND jsonb_typeof(capabilities->'consume_food') = 'boolean'
            AND jsonb_typeof(capabilities->'avoid_threat') = 'boolean');
