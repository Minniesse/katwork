-- Citations a Kat makes with `cite` over harness-minted spans, and the claims writers record
-- with copies of their citations' refs (ADR-0007, F3). Insert-only: a later task adds the
-- append-only trigger and the deletion path's redaction.

-- One row per job, span and quoted range: citing the same range again in the same job returns
-- this row. `start_cp` and `end_cp` are the narrowed ref's position, in code points of the
-- span's text, after the range was widened to whole tokens and snapped to grapheme clusters.
CREATE TABLE citations (
    id UUID PRIMARY KEY,
    workspace_id UUID NOT NULL REFERENCES workspaces (id),
    job_id UUID NOT NULL,
    run_id UUID NOT NULL,
    span_id UUID NOT NULL,
    source_ref JSONB NOT NULL,
    exact TEXT NOT NULL,
    start_cp INT NOT NULL,
    end_cp INT NOT NULL,
    display TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT citations_range_key UNIQUE (job_id, span_id, start_cp, end_cp),
    UNIQUE (id, workspace_id),
    FOREIGN KEY (job_id, workspace_id) REFERENCES jobs (id, workspace_id),
    FOREIGN KEY (run_id, workspace_id) REFERENCES runs (id, workspace_id),
    FOREIGN KEY (span_id, workspace_id) REFERENCES source_spans (id, workspace_id),
    CONSTRAINT citations_range_check CHECK (start_cp >= 0 AND end_cp > start_cp),
    CONSTRAINT citations_exact_check CHECK (exact <> '')
);

-- The deletion path finds a file's citations by span.
CREATE INDEX citations_span_id ON citations (span_id);

-- A claim of an output version, at its place in the file (`location`). `cited` is true exactly
-- when the claim has at least one `claim_refs` row.
CREATE TABLE claims (
    id UUID PRIMARY KEY,
    workspace_id UUID NOT NULL REFERENCES workspaces (id),
    artifact_version_id UUID NOT NULL,
    ordinal INT NOT NULL,
    kind TEXT NOT NULL,
    text TEXT NOT NULL,
    location TEXT NOT NULL,
    cited BOOLEAN NOT NULL,
    derive JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (artifact_version_id, ordinal),
    UNIQUE (id, workspace_id),
    FOREIGN KEY (artifact_version_id, workspace_id)
        REFERENCES artifact_versions (id, workspace_id),
    CONSTRAINT claims_kind_check CHECK (kind IN ('number', 'date', 'quote', 'fact')),
    CONSTRAINT claims_ordinal_check CHECK (ordinal >= 0),
    CONSTRAINT claims_text_check CHECK (text <> '' AND char_length(text) <= 500),
    CONSTRAINT claims_location_check CHECK (location <> '')
);

-- Each ref a copy of its citation's `source_ref`, so the claim keeps what was cited.
CREATE TABLE claim_refs (
    claim_id UUID NOT NULL,
    workspace_id UUID NOT NULL REFERENCES workspaces (id),
    ordinal INT NOT NULL,
    citation_id UUID NOT NULL,
    source_ref JSONB NOT NULL,
    PRIMARY KEY (claim_id, ordinal),
    FOREIGN KEY (claim_id, workspace_id) REFERENCES claims (id, workspace_id),
    FOREIGN KEY (citation_id, workspace_id) REFERENCES citations (id, workspace_id),
    CONSTRAINT claim_refs_ordinal_check CHECK (ordinal >= 0)
);

-- The deletion path finds the refs of a file's citations.
CREATE INDEX claim_refs_citation_id ON claim_refs (citation_id);
