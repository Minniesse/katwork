-- Spans the harness mints over a file version's extracted blocks (ADR-0007, F3). Insert-only:
-- the same block of the same version under the same extractor always has one row, so a reread
-- returns the same span ids. `part` is the DOCX package part, since paragraph ordinals restart
-- in each part, and '' for every other file type.
CREATE TABLE source_spans (
    id UUID PRIMARY KEY,
    workspace_id UUID NOT NULL REFERENCES workspaces (id),
    artifact_version_id UUID NOT NULL,
    extractor TEXT NOT NULL,
    extractor_version INT NOT NULL,
    part TEXT NOT NULL DEFAULT '',
    ordinal INT NOT NULL,
    piece INT NOT NULL,
    source_ref JSONB NOT NULL,
    text TEXT NOT NULL,
    text_sha256 TEXT NOT NULL,
    match_key TEXT NOT NULL,
    normalize_version INT NOT NULL,
    generated BOOLEAN NOT NULL,
    -- The `HiddenWhy` wire name; readers add reasons, so no list is checked here.
    hidden TEXT,
    -- The instruction detector's pattern id.
    instruction_like TEXT,
    -- The block's suspect reason, else its page's text layer reason.
    suspect_text_layer TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT source_spans_block_key
        UNIQUE (artifact_version_id, extractor, extractor_version, part, ordinal, piece),
    UNIQUE (id, workspace_id),
    FOREIGN KEY (artifact_version_id, workspace_id)
        REFERENCES artifact_versions (id, workspace_id),
    CONSTRAINT source_spans_ordinal_check CHECK (ordinal >= 0),
    CONSTRAINT source_spans_piece_check CHECK (piece >= 0),
    CONSTRAINT source_spans_extractor_version_check CHECK (extractor_version >= 0),
    CONSTRAINT source_spans_text_sha256_check CHECK (text_sha256 ~ '^[0-9a-f]{64}$')
);

-- U4: whether a tool's result may carry content no person of the workspace wrote. Rows written
-- before this migration read as untrusted.
ALTER TABLE tool_calls ADD COLUMN output_trust TEXT NOT NULL DEFAULT 'untrusted'
    CONSTRAINT tool_calls_output_trust_check CHECK (output_trust IN ('trusted', 'untrusted'));
