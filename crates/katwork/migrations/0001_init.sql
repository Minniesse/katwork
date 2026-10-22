CREATE TABLE workspaces (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    paused BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE users (
    id UUID PRIMARY KEY,
    email TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    display_name TEXT NOT NULL,
    locale TEXT NOT NULL CHECK (locale IN ('en', 'th')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE memberships (
    workspace_id UUID NOT NULL REFERENCES workspaces(id),
    user_id UUID NOT NULL REFERENCES users(id),
    role TEXT NOT NULL CHECK (role IN ('owner', 'admin', 'member')),
    PRIMARY KEY (workspace_id, user_id)
);

CREATE TABLE sessions (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id),
    token_sha256 TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ
);

CREATE TABLE password_resets (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id),
    token_sha256 TEXT NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    used_at TIMESTAMPTZ
);

CREATE TABLE spaces (
    id UUID PRIMARY KEY,
    workspace_id UUID NOT NULL REFERENCES workspaces(id),
    name TEXT NOT NULL,
    allow_external BOOLEAN NOT NULL,
    UNIQUE (id, workspace_id)
);

CREATE TABLE dots (
    id UUID PRIMARY KEY,
    workspace_id UUID NOT NULL REFERENCES workspaces(id),
    default_space_id UUID NOT NULL,
    name TEXT NOT NULL,
    role TEXT NOT NULL,
    persona TEXT NOT NULL,
    instructions TEXT NOT NULL,
    tool_allowlist TEXT[] NOT NULL,
    model_tier TEXT NOT NULL CHECK (model_tier IN ('fast', 'standard', 'strong')),
    autonomy TEXT NOT NULL CHECK (autonomy IN ('ask_first', 'standard')),
    appearance JSONB NOT NULL DEFAULT '{}',
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'paused')),
    max_concurrency INT NOT NULL DEFAULT 1 CHECK (max_concurrency > 0),
    policy_version BIGINT NOT NULL DEFAULT 1,
    daily_token_budget BIGINT,
    UNIQUE (id, workspace_id),
    FOREIGN KEY (default_space_id, workspace_id) REFERENCES spaces(id, workspace_id)
);

CREATE TABLE dot_spaces (
    workspace_id UUID NOT NULL REFERENCES workspaces(id),
    dot_id UUID NOT NULL,
    space_id UUID NOT NULL,
    PRIMARY KEY (dot_id, space_id),
    FOREIGN KEY (dot_id, workspace_id) REFERENCES dots(id, workspace_id),
    FOREIGN KEY (space_id, workspace_id) REFERENCES spaces(id, workspace_id)
);

ALTER TABLE dots ADD FOREIGN KEY (id, default_space_id)
    REFERENCES dot_spaces(dot_id, space_id) DEFERRABLE INITIALLY DEFERRED;

CREATE TABLE teams (
    id UUID PRIMARY KEY,
    workspace_id UUID NOT NULL REFERENCES workspaces(id),
    space_id UUID NOT NULL,
    name TEXT NOT NULL,
    UNIQUE (id, workspace_id),
    FOREIGN KEY (space_id, workspace_id) REFERENCES spaces(id, workspace_id)
);

CREATE TABLE team_members (
    workspace_id UUID NOT NULL REFERENCES workspaces(id),
    team_id UUID NOT NULL,
    dot_id UUID NOT NULL,
    is_lead BOOLEAN NOT NULL,
    position INT NOT NULL,
    PRIMARY KEY (team_id, dot_id),
    FOREIGN KEY (team_id, workspace_id) REFERENCES teams(id, workspace_id),
    FOREIGN KEY (dot_id, workspace_id) REFERENCES dots(id, workspace_id)
);

CREATE UNIQUE INDEX team_members_one_lead ON team_members (team_id) WHERE is_lead;

CREATE TABLE threads (
    id UUID PRIMARY KEY,
    workspace_id UUID NOT NULL REFERENCES workspaces(id),
    space_id UUID NOT NULL,
    team_id UUID NOT NULL,
    title TEXT NOT NULL,
    last_event_seq BIGINT NOT NULL DEFAULT 0,
    UNIQUE (id, workspace_id),
    FOREIGN KEY (space_id, workspace_id) REFERENCES spaces(id, workspace_id),
    FOREIGN KEY (team_id, workspace_id) REFERENCES teams(id, workspace_id)
);

CREATE TABLE messages (
    id UUID PRIMARY KEY,
    workspace_id UUID NOT NULL REFERENCES workspaces(id),
    thread_id UUID NOT NULL,
    author_kind TEXT NOT NULL CHECK (author_kind IN ('user', 'dot', 'system')),
    author_user_id UUID REFERENCES users(id),
    author_dot_id UUID,
    body TEXT NOT NULL,
    client_msg_id TEXT CHECK (char_length(client_msg_id) <= 64),
    idempotency_key TEXT UNIQUE,
    synthetic BOOLEAN NOT NULL DEFAULT false,
    question JSONB,
    job_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (thread_id, author_user_id, client_msg_id),
    UNIQUE (id, workspace_id),
    FOREIGN KEY (thread_id, workspace_id) REFERENCES threads(id, workspace_id),
    FOREIGN KEY (author_dot_id, workspace_id) REFERENCES dots(id, workspace_id),
    CONSTRAINT messages_user_author_check CHECK ((author_kind = 'user') = (author_user_id IS NOT NULL)),
    CONSTRAINT messages_dot_author_check CHECK ((author_kind = 'dot') = (author_dot_id IS NOT NULL)),
    CONSTRAINT messages_client_author_check CHECK (client_msg_id IS NULL OR author_kind = 'user')
);

CREATE TABLE jobs (
    id UUID PRIMARY KEY,
    workspace_id UUID NOT NULL REFERENCES workspaces(id),
    thread_id UUID NOT NULL,
    request_message_id UUID NOT NULL UNIQUE,
    requested_by UUID NOT NULL REFERENCES users(id),
    parent_job_id UUID,
    status TEXT CHECK (status IS NULL OR status IN ('accepted', 'ready', 'answered', 'stopped', 'needs_input', 'cancelled', 'error')),
    reason TEXT,
    decided_by TEXT CHECK (decided_by IN ('policy', 'harness')),
    decision_seq BIGINT,
    contract_sha256 TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ,
    UNIQUE (id, workspace_id),
    FOREIGN KEY (thread_id, workspace_id) REFERENCES threads(id, workspace_id),
    FOREIGN KEY (request_message_id, workspace_id) REFERENCES messages(id, workspace_id),
    FOREIGN KEY (parent_job_id, workspace_id) REFERENCES jobs(id, workspace_id)
);

ALTER TABLE messages ADD FOREIGN KEY (job_id, workspace_id) REFERENCES jobs(id, workspace_id);

CREATE TABLE tasks (
    id UUID PRIMARY KEY,
    workspace_id UUID NOT NULL REFERENCES workspaces(id),
    job_id UUID NOT NULL,
    dot_id UUID NOT NULL,
    dot_policy_version BIGINT NOT NULL,
    input JSONB NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('queued', 'leased', 'done', 'failed', 'interrupted', 'cancelled')),
    attempts INT NOT NULL DEFAULT 0,
    max_attempts INT NOT NULL DEFAULT 3,
    idempotency_key TEXT UNIQUE,
    effects_started_at TIMESTAMPTZ,
    hold_reason TEXT CHECK (hold_reason IN ('dot_busy', 'provider_quota', 'workspace_paused')),
    lease_epoch BIGINT NOT NULL DEFAULT 0,
    pending_tool_call JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (id, workspace_id),
    FOREIGN KEY (job_id, workspace_id) REFERENCES jobs(id, workspace_id),
    FOREIGN KEY (dot_id, workspace_id) REFERENCES dots(id, workspace_id)
);

CREATE TABLE task_leases (
    task_id UUID PRIMARY KEY REFERENCES tasks(id),
    worker_id TEXT NOT NULL,
    lease_epoch BIGINT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX tasks_claim_state_created_at ON tasks (state, created_at)
    WHERE state IN ('queued', 'leased');
CREATE INDEX tasks_live_leases_by_dot ON tasks (dot_id) WHERE state = 'leased';

CREATE TABLE runs (
    id UUID PRIMARY KEY,
    workspace_id UUID NOT NULL REFERENCES workspaces(id),
    job_id UUID NOT NULL,
    task_id UUID NOT NULL,
    dot_id UUID NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('initial', 'repair', 'recovery')),
    lease_epoch BIGINT NOT NULL,
    dot_config_hash TEXT NOT NULL,
    model TEXT,
    state TEXT NOT NULL CHECK (state IN ('running', 'finished', 'interrupted', 'cancelled', 'error')),
    stop_reason TEXT,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at TIMESTAMPTZ,
    UNIQUE (id, workspace_id),
    FOREIGN KEY (job_id, workspace_id) REFERENCES jobs(id, workspace_id),
    FOREIGN KEY (task_id, workspace_id) REFERENCES tasks(id, workspace_id),
    FOREIGN KEY (dot_id, workspace_id) REFERENCES dots(id, workspace_id)
);

CREATE TABLE tool_calls (
    id UUID PRIMARY KEY,
    workspace_id UUID NOT NULL REFERENCES workspaces(id),
    run_id UUID NOT NULL,
    step INT NOT NULL,
    tool_call_id TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    side_effect TEXT CHECK (side_effect IN ('read', 'navigate', 'workspace_read', 'draft', 'workspace_write', 'exec', 'credential_use', 'destructive', 'outward_send', 'outward_publish', 'outward_pay', 'outward_share', 'outward_external_api')),
    args_canonical TEXT NOT NULL,
    args_sha256 TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('started', 'succeeded', 'failed', 'timed_out', 'outcome_unknown', 'refused')),
    attempts INT NOT NULL DEFAULT 1 CHECK (attempts IN (1, 2)),
    result TEXT,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at TIMESTAMPTZ,
    UNIQUE (run_id, step, tool_call_id),
    CHECK (side_effect IS NOT NULL OR state = 'refused'),
    FOREIGN KEY (run_id, workspace_id) REFERENCES runs(id, workspace_id)
);

CREATE TABLE model_calls (
    id UUID PRIMARY KEY,
    workspace_id UUID NOT NULL REFERENCES workspaces(id),
    space_id UUID NOT NULL,
    job_id UUID NOT NULL,
    task_id UUID NOT NULL,
    run_id UUID NOT NULL,
    dot_id UUID NOT NULL,
    provider_id TEXT NOT NULL,
    model TEXT NOT NULL,
    residency TEXT NOT NULL,
    attempt INT NOT NULL,
    input_tokens BIGINT,
    cache_read_tokens BIGINT,
    cache_write_tokens BIGINT,
    output_tokens BIGINT,
    usage_complete BOOLEAN NOT NULL,
    charged_tokens BIGINT NOT NULL,
    cost_micros BIGINT,
    latency_ms INT NOT NULL,
    outcome TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (space_id, workspace_id) REFERENCES spaces(id, workspace_id),
    FOREIGN KEY (job_id, workspace_id) REFERENCES jobs(id, workspace_id),
    FOREIGN KEY (task_id, workspace_id) REFERENCES tasks(id, workspace_id),
    FOREIGN KEY (run_id, workspace_id) REFERENCES runs(id, workspace_id),
    FOREIGN KEY (dot_id, workspace_id) REFERENCES dots(id, workspace_id)
);

CREATE TABLE events (
    thread_id UUID NOT NULL,
    seq BIGINT NOT NULL,
    workspace_id UUID NOT NULL REFERENCES workspaces(id),
    run_id UUID,
    kind TEXT NOT NULL,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (thread_id, seq),
    FOREIGN KEY (thread_id, workspace_id) REFERENCES threads(id, workspace_id),
    FOREIGN KEY (run_id, workspace_id) REFERENCES runs(id, workspace_id)
);

CREATE INDEX model_calls_dot_daily_tokens ON model_calls (dot_id, created_at) INCLUDE (charged_tokens);
