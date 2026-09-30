-- Existing intents predate request IDs; new reservations always supply both fields.
ALTER TABLE upload_intents
    ADD COLUMN request_id uuid,
    ADD COLUMN request_hash bytea,
    ADD CONSTRAINT upload_intents_request_pair CHECK (
        (request_id IS NULL AND request_hash IS NULL) OR
        (request_id IS NOT NULL AND request_hash IS NOT NULL AND octet_length(request_hash) = 32)
    );

CREATE UNIQUE INDEX upload_intents_request_unique
    ON upload_intents(session_id, principal_kind, principal_id, request_id)
    WHERE request_id IS NOT NULL;
