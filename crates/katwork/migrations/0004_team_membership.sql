-- Preserve membership rows: SP1 never deletes referenced domain state.
ALTER TABLE team_members ADD COLUMN active BOOLEAN NOT NULL DEFAULT true;
DROP INDEX team_members_one_lead;
CREATE UNIQUE INDEX team_members_one_lead ON team_members (team_id) WHERE is_lead AND active;

ALTER TABLE teams ADD COLUMN is_default BOOLEAN NOT NULL DEFAULT false;
CREATE UNIQUE INDEX teams_default_per_space ON teams (workspace_id, space_id) WHERE is_default;
ALTER TABLE dots ADD CONSTRAINT dots_daily_token_budget_nonnegative CHECK (daily_token_budget >= 0);
ALTER TABLE dots ADD CONSTRAINT dots_max_concurrency_u16 CHECK (max_concurrency <= 65535);
