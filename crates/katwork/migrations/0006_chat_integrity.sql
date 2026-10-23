ALTER TABLE teams ADD UNIQUE (id, space_id, workspace_id);
ALTER TABLE threads ADD FOREIGN KEY (team_id, space_id, workspace_id)
    REFERENCES teams (id, space_id, workspace_id);

-- Check membership at intake; user FKs preserve history after a member leaves.

ALTER TABLE messages ADD COLUMN meta JSONB;
ALTER TABLE messages ADD COLUMN stopped BOOLEAN NOT NULL DEFAULT false
    CHECK (NOT stopped OR author_kind = 'kat');

CREATE INDEX messages_thread_id ON messages (thread_id, id);
CREATE INDEX threads_space_id ON threads (space_id, id DESC);
CREATE INDEX events_run_started ON events (thread_id, seq) WHERE kind = 'RUN_STARTED';
