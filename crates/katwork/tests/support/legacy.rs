use super::Fixture;
use sqlx::PgPool;
use uuid::Uuid;

// Frozen pre-rename SQL exercises upgrades from historical schemas.
impl Fixture {
    pub async fn new_legacy(pool: &PgPool) -> Self {
        let f = Self {
            workspace: Uuid::now_v7(),
            user: Uuid::now_v7(),
            space: Uuid::now_v7(),
            kat: Uuid::now_v7(),
            team: Uuid::now_v7(),
            thread: Uuid::now_v7(),
            message: Uuid::now_v7(),
            job: Uuid::now_v7(),
            task: Uuid::now_v7(),
            run: Uuid::now_v7(),
        };
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("INSERT INTO workspaces (id, name) VALUES ($1, 'test')")
            .bind(f.workspace)
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("INSERT INTO users (id, email, password_hash, display_name, locale) VALUES ($1, $2, $3, 'Test', 'th')").bind(f.user).bind(format!("{}@example.invalid", f.user)).bind(Uuid::now_v7().to_string()).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO spaces (id, workspace_id, name, allow_external) VALUES ($1, $2, 'Space', false)").bind(f.space).bind(f.workspace).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO dots (id, workspace_id, default_space_id, name, role, persona, instructions, tool_allowlist, model_tier, autonomy) VALUES ($1, $2, $3, 'นักวิจัย', 'researcher', '', '', '{}', 'standard', 'ask_first')").bind(f.kat).bind(f.workspace).bind(f.space).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO dot_spaces (workspace_id, dot_id, space_id) VALUES ($1, $2, $3)")
            .bind(f.workspace)
            .bind(f.kat)
            .bind(f.space)
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO teams (id, workspace_id, space_id, name) VALUES ($1, $2, $3, 'Team')",
        )
        .bind(f.team)
        .bind(f.workspace)
        .bind(f.space)
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query("INSERT INTO threads (id, workspace_id, space_id, team_id, title) VALUES ($1, $2, $3, $4, 'Thread')").bind(f.thread).bind(f.workspace).bind(f.space).bind(f.team).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO messages (id, workspace_id, thread_id, author_kind, author_user_id, body) VALUES ($1, $2, $3, 'user', $4, 'Hello')").bind(f.message).bind(f.workspace).bind(f.thread).bind(f.user).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO jobs (id, workspace_id, thread_id, request_message_id, requested_by) VALUES ($1, $2, $3, $4, $5)").bind(f.job).bind(f.workspace).bind(f.thread).bind(f.message).bind(f.user).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO tasks (id, workspace_id, job_id, dot_id, dot_policy_version, input, state) VALUES ($1, $2, $3, $4, 1, '{}', 'queued')").bind(f.task).bind(f.workspace).bind(f.job).bind(f.kat).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO runs (id, workspace_id, job_id, task_id, dot_id, kind, lease_epoch, dot_config_hash, state) VALUES ($1, $2, $3, $4, $5, 'initial', 0, 'hash', 'running')").bind(f.run).bind(f.workspace).bind(f.job).bind(f.task).bind(f.kat).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
        f
    }
}
