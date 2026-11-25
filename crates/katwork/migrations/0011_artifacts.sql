-- Files of a Space: an input a person uploaded, or an output a Kat wrote. Every version names a
-- content-addressed blob under the workspace's own directory of the artifact volume.
CREATE TABLE artifacts (
    id UUID PRIMARY KEY,
    workspace_id UUID NOT NULL REFERENCES workspaces(id),
    space_id UUID NOT NULL,
    name TEXT NOT NULL,
    role TEXT NOT NULL,
    created_by_user UUID REFERENCES users(id),
    created_by_kat UUID,
    job_id UUID,
    -- Written only by file deletion (a later migration's path), present now so every reader
    -- refuses a deleted file from the start.
    deleted_at TIMESTAMPTZ NULL,
    deleted_by UUID NULL REFERENCES users(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (id, workspace_id),
    FOREIGN KEY (space_id, workspace_id) REFERENCES spaces(id, workspace_id),
    FOREIGN KEY (created_by_kat, workspace_id) REFERENCES kats(id, workspace_id),
    FOREIGN KEY (job_id, workspace_id) REFERENCES jobs(id, workspace_id),
    CONSTRAINT artifacts_name_check CHECK (octet_length(name) BETWEEN 1 AND 512),
    CONSTRAINT artifacts_role_check CHECK (role IN ('input', 'output')),
    -- An input has the person who uploaded it and no Kat; an output has its Kat and job.
    CONSTRAINT artifacts_input_author_check
        CHECK (role <> 'input' OR (created_by_user IS NOT NULL AND created_by_kat IS NULL)),
    CONSTRAINT artifacts_output_author_check
        CHECK (role <> 'output' OR (created_by_kat IS NOT NULL AND job_id IS NOT NULL)),
    CONSTRAINT artifacts_deleted_check CHECK ((deleted_at IS NULL) = (deleted_by IS NULL))
);

CREATE INDEX artifacts_space_name ON artifacts (workspace_id, space_id, name);

CREATE TABLE artifact_versions (
    id UUID PRIMARY KEY,
    workspace_id UUID NOT NULL REFERENCES workspaces(id),
    artifact_id UUID NOT NULL,
    version INTEGER NOT NULL,
    sha256 TEXT NOT NULL,
    bytes BIGINT NOT NULL,
    media_type TEXT NOT NULL,
    run_id UUID,
    -- A Draft tool's receipt: the tool call's idempotency key.
    idempotency_key TEXT UNIQUE,
    gridcraft_check TEXT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (artifact_id, version),
    UNIQUE (id, workspace_id),
    FOREIGN KEY (artifact_id, workspace_id) REFERENCES artifacts(id, workspace_id),
    FOREIGN KEY (run_id, workspace_id) REFERENCES runs(id, workspace_id),
    CONSTRAINT artifact_versions_version_check CHECK (version >= 1),
    CONSTRAINT artifact_versions_sha256_check CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT artifact_versions_bytes_check CHECK (bytes >= 0),
    CONSTRAINT artifact_versions_media_type_check
        CHECK (media_type IN ('pdf', 'docx', 'xlsx', 'text', 'markdown', 'csv', 'png', 'jpeg')),
    -- Only a run versions a file: every version after the first names the run that wrote it.
    CONSTRAINT artifact_versions_run_required_check CHECK (version = 1 OR run_id IS NOT NULL),
    CONSTRAINT artifact_versions_gridcraft_check_check
        CHECK (gridcraft_check IN ('passed', 'unavailable', 'off')),
    CONSTRAINT artifact_versions_gridcraft_xlsx_check
        CHECK (gridcraft_check IS NULL OR media_type = 'xlsx')
);

-- The storage quota sums a workspace's versions.
CREATE INDEX artifact_versions_workspace ON artifact_versions (workspace_id);
