#![allow(
    clippy::disallowed_methods,
    reason = "Test fixtures drain SQL; cancellation probes deliberately drop futures"
)]
pub mod cancel_sql;
mod legacy;
pub mod locks;

use std::time::Duration;

use sqlx::PgPool;
use tokio::time::timeout;
use uuid::Uuid;

/// Every connection of `pool` is idle in it, and no other client backend of the test database
/// is busy or inside a transaction. Opens its own observer connection, so it never takes a
/// permit a test pool shares.
#[allow(dead_code, reason = "Only some test binaries cancel database work")]
pub async fn assert_pool_clean(pool: &PgPool) {
    // sqlx test pools share permits: inspecting a pool must never grow it to maximum capacity.
    timeout(Duration::from_secs(30), async {
        use sqlx::Connection;
        let mut observer = sqlx::PgConnection::connect_with(&pool.connect_options())
            .await
            .unwrap();
        loop {
            let clean: bool = sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND backend_type='client backend' AND pid<>pg_backend_pid() AND state IS DISTINCT FROM 'idle')")
                .fetch_one(&mut observer).await.unwrap();
            if pool.num_idle() as u32 == pool.size() && clean {
                break;
            }
            tokio::task::yield_now().await;
        }
        observer.close().await.unwrap();
    })
    .await
    .expect("worker shutdown left a busy pool or a backend inside a transaction");
}

pub struct Fixture {
    pub workspace: Uuid,
    pub user: Uuid,
    pub space: Uuid,
    pub kat: Uuid,
    pub team: Uuid,
    pub thread: Uuid,
    pub message: Uuid,
    pub job: Uuid,
    pub task: Uuid,
    pub run: Uuid,
}

impl Fixture {
    pub async fn new(pool: &PgPool) -> Self {
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
        sqlx::query("INSERT INTO memberships (workspace_id, user_id, role) VALUES ($1,$2,'owner')")
            .bind(f.workspace)
            .bind(f.user)
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("INSERT INTO spaces (id, workspace_id, name, allow_external) VALUES ($1, $2, 'Space', false)").bind(f.space).bind(f.workspace).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO kats (id, workspace_id, default_space_id, name, role, persona, instructions, tool_allowlist, model_tier, autonomy) VALUES ($1, $2, $3, 'นักวิจัย', 'researcher', '', '', '{}', 'standard', 'ask_first')").bind(f.kat).bind(f.workspace).bind(f.space).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO kat_spaces (workspace_id, kat_id, space_id) VALUES ($1, $2, $3)")
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
        sqlx::query("INSERT INTO tasks (id, workspace_id, job_id, kat_id, kat_policy_version, input, state) VALUES ($1, $2, $3, $4, 1, '{}', 'queued')").bind(f.task).bind(f.workspace).bind(f.job).bind(f.kat).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO runs (id, workspace_id, job_id, task_id, kat_id, kind, lease_epoch, kat_config_hash, state) VALUES ($1, $2, $3, $4, $5, 'initial', 0, 'hash', 'running')").bind(f.run).bind(f.workspace).bind(f.job).bind(f.task).bind(f.kat).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
        f
    }

    pub async fn tool_call(
        &self,
        pool: &PgPool,
        step: i32,
        effect: Option<&str>,
        state: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("INSERT INTO tool_calls (id, workspace_id, run_id, step, tool_call_id, tool_name, side_effect, args_canonical, args_sha256, state) VALUES ($1, $2, $3, $4, 'call', 'tool', $5, '{}', 'hash', $6)")
            .bind(Uuid::now_v7()).bind(self.workspace).bind(self.run).bind(step).bind(effect).bind(state).execute(pool).await?;
        Ok(())
    }

    pub async fn live_lease(&self, pool: &PgPool) {
        sqlx::query("UPDATE tasks SET state = 'leased', lease_epoch = 1 WHERE id = $1")
            .bind(self.task)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO task_leases (task_id, worker_id, lease_epoch, expires_at) VALUES ($1, 'worker', 1, clock_timestamp() + interval '1 hour')").bind(self.task).execute(pool).await.unwrap();
    }
}

pub fn sqlstate(error: sqlx::Error, expected: &str) {
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some(expected),
        "{error}"
    );
}
