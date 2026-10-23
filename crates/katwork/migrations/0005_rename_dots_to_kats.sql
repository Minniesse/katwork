-- Rename domain identifiers without rewriting historical migration checksums.
ALTER TABLE dots RENAME TO kats;
ALTER TABLE dot_spaces RENAME TO kat_spaces;
ALTER TABLE kat_spaces RENAME COLUMN dot_id TO kat_id;
ALTER TABLE team_members RENAME COLUMN dot_id TO kat_id;
ALTER TABLE messages RENAME COLUMN author_dot_id TO author_kat_id;
ALTER TABLE tasks RENAME COLUMN dot_id TO kat_id;
ALTER TABLE tasks RENAME COLUMN dot_policy_version TO kat_policy_version;
ALTER TABLE runs RENAME COLUMN dot_id TO kat_id;
ALTER TABLE runs RENAME COLUMN dot_config_hash TO kat_config_hash;
ALTER TABLE model_calls RENAME COLUMN dot_id TO kat_id;

-- PostgreSQL updates dependencies, but retains constraint and index names.
ALTER TABLE kats RENAME CONSTRAINT dots_autonomy_check TO kats_autonomy_check;
ALTER TABLE kats RENAME CONSTRAINT dots_daily_token_budget_nonnegative TO kats_daily_token_budget_nonnegative;
ALTER TABLE kats RENAME CONSTRAINT dots_default_space_id_workspace_id_fkey TO kats_default_space_id_workspace_id_fkey;
ALTER TABLE kats RENAME CONSTRAINT dots_id_default_space_id_fkey TO kats_id_default_space_id_fkey;
ALTER TABLE kats RENAME CONSTRAINT dots_id_workspace_id_key TO kats_id_workspace_id_key;
ALTER TABLE kats RENAME CONSTRAINT dots_max_concurrency_check TO kats_max_concurrency_check;
ALTER TABLE kats RENAME CONSTRAINT dots_max_concurrency_u16 TO kats_max_concurrency_u16;
ALTER TABLE kats RENAME CONSTRAINT dots_model_tier_check TO kats_model_tier_check;
ALTER TABLE kats RENAME CONSTRAINT dots_pkey TO kats_pkey;
ALTER TABLE kats RENAME CONSTRAINT dots_status_check TO kats_status_check;
ALTER TABLE kats RENAME CONSTRAINT dots_workspace_id_fkey TO kats_workspace_id_fkey;
ALTER TABLE kat_spaces RENAME CONSTRAINT dot_spaces_dot_id_workspace_id_fkey TO kat_spaces_kat_id_workspace_id_fkey;
ALTER TABLE kat_spaces RENAME CONSTRAINT dot_spaces_pkey TO kat_spaces_pkey;
ALTER TABLE kat_spaces RENAME CONSTRAINT dot_spaces_space_id_workspace_id_fkey TO kat_spaces_space_id_workspace_id_fkey;
ALTER TABLE kat_spaces RENAME CONSTRAINT dot_spaces_workspace_id_fkey TO kat_spaces_workspace_id_fkey;
ALTER TABLE team_members RENAME CONSTRAINT team_members_dot_id_workspace_id_fkey TO team_members_kat_id_workspace_id_fkey;
ALTER TABLE messages RENAME CONSTRAINT messages_author_dot_id_workspace_id_fkey TO messages_author_kat_id_workspace_id_fkey;
ALTER TABLE tasks RENAME CONSTRAINT tasks_dot_id_workspace_id_fkey TO tasks_kat_id_workspace_id_fkey;
ALTER TABLE runs RENAME CONSTRAINT runs_dot_id_workspace_id_fkey TO runs_kat_id_workspace_id_fkey;
ALTER TABLE model_calls RENAME CONSTRAINT model_calls_dot_id_workspace_id_fkey TO model_calls_kat_id_workspace_id_fkey;
ALTER INDEX tasks_live_leases_by_dot RENAME TO tasks_live_leases_by_kat;
ALTER INDEX model_calls_dot_daily_tokens RENAME TO model_calls_kat_daily_tokens;

-- SQLx runs this migration in one transaction, including CHECK replacement.
ALTER TABLE messages DROP CONSTRAINT messages_author_kind_check;
ALTER TABLE messages DROP CONSTRAINT messages_dot_author_check;
UPDATE messages SET author_kind = 'kat' WHERE author_kind = 'dot';
ALTER TABLE messages ADD CONSTRAINT messages_author_kind_check CHECK (author_kind IN ('user', 'kat', 'system'));
ALTER TABLE messages ADD CONSTRAINT messages_kat_author_check CHECK ((author_kind = 'kat') = (author_kat_id IS NOT NULL));
ALTER TABLE tasks DROP CONSTRAINT tasks_hold_reason_check;
UPDATE tasks SET hold_reason = 'kat_busy' WHERE hold_reason = 'dot_busy';
ALTER TABLE tasks ADD CONSTRAINT tasks_hold_reason_check CHECK (hold_reason IN ('kat_busy', 'provider_quota', 'workspace_paused'));

UPDATE jobs SET reason = replace(reason, 'dot', 'kat')
WHERE reason IN ('dot_paused', 'no_eligible_dot', 'dot_not_in_space');
UPDATE runs SET stop_reason = replace(stop_reason, 'dot', 'kat')
WHERE stop_reason IN ('dot_paused', 'no_eligible_dot', 'dot_not_in_space');
UPDATE kats SET appearance = jsonb_set(appearance, '{avatar}', '"kat"')
WHERE appearance->>'avatar' = 'dot';
