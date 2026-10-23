ALTER TABLE teams ADD UNIQUE (id, space_id, workspace_id);
ALTER TABLE threads ADD FOREIGN KEY (team_id, space_id, workspace_id)
    REFERENCES teams (id, space_id, workspace_id);

-- Memberships are never deleted or rekeyed; future removal must preserve these references.
ALTER TABLE messages ADD FOREIGN KEY (workspace_id, author_user_id)
    REFERENCES memberships (workspace_id, user_id);
ALTER TABLE jobs ADD FOREIGN KEY (workspace_id, requested_by)
    REFERENCES memberships (workspace_id, user_id);

ALTER TABLE messages ADD COLUMN meta JSONB;
ALTER TABLE messages ADD COLUMN stopped BOOLEAN NOT NULL DEFAULT false
    CHECK (NOT stopped OR author_kind = 'kat');

CREATE INDEX messages_thread_id ON messages (thread_id, id);
CREATE INDEX threads_space_id ON threads (space_id, id DESC);
CREATE INDEX events_run_started ON events (thread_id, seq) WHERE kind = 'RUN_STARTED';
CREATE INDEX jobs_thread_id ON jobs (thread_id, id DESC);
CREATE INDEX tasks_kat_created ON tasks (kat_id, created_at);
