mod support;

use katwork::{
    db,
    fence::{self, Fence},
    ids::{LeaseEpoch, TaskId},
    types::{OutwardKind, SideEffect},
};
use sqlx::{PgPool, Row};
use support::{Fixture, sqlstate};
use uuid::Uuid;

#[sqlx::test(migrations = false)]
async fn migrations_apply_cleanly(pool: PgPool) {
    db::migrate(&pool).await.unwrap();
    db::migrate(&pool).await.unwrap();
    let tables: Vec<String> = sqlx::query_scalar("SELECT tablename FROM pg_tables WHERE schemaname = 'public' AND tablename <> '_sqlx_migrations' ORDER BY tablename").fetch_all(&pool).await.unwrap();
    assert_eq!(
        tables,
        [
            "dot_spaces",
            "dots",
            "events",
            "jobs",
            "memberships",
            "messages",
            "model_calls",
            "password_resets",
            "runs",
            "sessions",
            "spaces",
            "task_leases",
            "tasks",
            "team_members",
            "teams",
            "threads",
            "tool_calls",
            "users",
            "workspaces"
        ]
    );
    Fixture::new(&pool).await;
}

#[sqlx::test]
async fn job_status_rejects_unknown_value(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO messages (id, workspace_id, thread_id, author_kind, body) VALUES ($1, $2, $3, 'system', '')").bind(Uuid::now_v7()).bind(f.workspace).bind(f.thread).execute(&mut *tx).await.unwrap();
    let error = sqlx::query("INSERT INTO jobs (id, workspace_id, thread_id, request_message_id, requested_by, status) VALUES ($1, $2, $3, $4, $5, 'done')").bind(Uuid::now_v7()).bind(f.workspace).bind(f.thread).bind(f.message).bind(f.user).execute(&mut *tx).await.unwrap_err();
    sqlstate(error, "23514");
}

#[sqlx::test]
async fn thai_text_round_trips(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let name: String = sqlx::query_scalar("SELECT name FROM dots WHERE id = $1")
        .bind(f.dot)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name.as_bytes(), "นักวิจัย".as_bytes());
}

#[sqlx::test]
async fn task_state_accepts_interrupted_rejects_unknown(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    sqlx::query("UPDATE tasks SET state = 'interrupted' WHERE id = $1")
        .bind(f.task)
        .execute(&pool)
        .await
        .unwrap();
    sqlstate(
        sqlx::query("UPDATE tasks SET state = 'unknown' WHERE id = $1")
            .bind(f.task)
            .execute(&pool)
            .await
            .unwrap_err(),
        "23514",
    );
}

#[sqlx::test]
async fn event_seq_is_per_thread_and_unique(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let other = Uuid::now_v7();
    sqlx::query("INSERT INTO threads (id, workspace_id, space_id, team_id, title) VALUES ($1, $2, $3, $4, 'Other')").bind(other).bind(f.workspace).bind(f.space).bind(f.team).execute(&pool).await.unwrap();
    for thread in [f.thread, other] {
        sqlx::query("INSERT INTO events (thread_id, seq, workspace_id, kind, payload) VALUES ($1, 1, $2, 'CUSTOM', '{}')").bind(thread).bind(f.workspace).execute(&pool).await.unwrap();
    }
    sqlstate(sqlx::query("INSERT INTO events (thread_id, seq, workspace_id, kind, payload) VALUES ($1, 1, $2, 'CUSTOM', '{}')").bind(f.thread).bind(f.workspace).execute(&pool).await.unwrap_err(), "23505");
}

#[sqlx::test]
async fn dot_default_space_must_be_granted(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO dots (id, workspace_id, default_space_id, name, role, persona, instructions, tool_allowlist, model_tier, autonomy) SELECT $1, workspace_id, default_space_id, name, role, persona, instructions, tool_allowlist, model_tier, autonomy FROM dots WHERE id = $2").bind(Uuid::now_v7()).bind(f.dot).execute(&mut *tx).await.unwrap();
    sqlstate(tx.commit().await.unwrap_err(), "23503");
}

#[sqlx::test]
async fn tool_call_unique_per_run_step_call(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.tool_call(&pool, 1, Some("read"), "started")
        .await
        .unwrap();
    f.tool_call(&pool, 2, Some("read"), "started")
        .await
        .unwrap();
    sqlstate(
        f.tool_call(&pool, 1, Some("read"), "started")
            .await
            .unwrap_err(),
        "23505",
    );
}

#[sqlx::test]
async fn client_msg_id_unique_per_thread_and_author(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for key in [Some("retry"), None, None] {
        sqlx::query("INSERT INTO messages (id, workspace_id, thread_id, author_kind, author_user_id, body, client_msg_id) VALUES ($1, $2, $3, 'user', $4, '', $5)").bind(Uuid::now_v7()).bind(f.workspace).bind(f.thread).bind(f.user).bind(key).execute(&pool).await.unwrap();
    }
    sqlstate(sqlx::query("INSERT INTO messages (id, workspace_id, thread_id, author_kind, author_user_id, body, client_msg_id) VALUES ($1, $2, $3, 'user', $4, '', 'retry')").bind(Uuid::now_v7()).bind(f.workspace).bind(f.thread).bind(f.user).execute(&pool).await.unwrap_err(), "23505");
    sqlstate(
        sqlx::query("UPDATE messages SET client_msg_id = $1 WHERE id = $2")
            .bind("ก".repeat(65))
            .bind(f.message)
            .execute(&pool)
            .await
            .unwrap_err(),
        "23514",
    );
    sqlx::query("UPDATE messages SET client_msg_id = $1 WHERE id = $2")
        .bind("ก".repeat(64))
        .bind(f.message)
        .execute(&pool)
        .await
        .unwrap();
}

#[sqlx::test]
async fn job_request_message_is_unique(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    sqlstate(sqlx::query("INSERT INTO jobs (id, workspace_id, thread_id, request_message_id, requested_by) VALUES ($1, $2, $3, $4, $5)").bind(Uuid::now_v7()).bind(f.workspace).bind(f.thread).bind(f.message).bind(f.user).execute(&pool).await.unwrap_err(), "23505");
}

#[sqlx::test]
async fn fence_check_fails_after_epoch_or_state_change(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.live_lease(&pool).await;
    let fence = Fence {
        task_id: TaskId(f.task),
        epoch: LeaseEpoch(1),
    };
    let mut conn = pool.acquire().await.unwrap();
    fence::check(&mut conn, &fence).await.unwrap();
    fence::lock(&mut conn, &fence).await.unwrap();
    for mutation in [
        "UPDATE tasks SET lease_epoch = 2 WHERE id = $1",
        "UPDATE task_leases SET lease_epoch = 2 WHERE task_id = $1",
        "UPDATE tasks SET state = 'done' WHERE id = $1",
        "UPDATE task_leases SET expires_at = now() - interval '1 second' WHERE task_id = $1",
        "DELETE FROM task_leases WHERE task_id = $1",
    ] {
        let mut tx = pool.begin().await.unwrap();
        sqlx::query(mutation)
            .bind(f.task)
            .execute(&mut *tx)
            .await
            .unwrap();
        assert_eq!(
            fence::check(&mut tx, &fence).await.unwrap_err().code(),
            "lease_lost"
        );
        assert_eq!(
            fence::lock(&mut tx, &fence).await.unwrap_err().code(),
            "lease_lost"
        );
        tx.rollback().await.unwrap();
    }
}

#[sqlx::test]
async fn missing_usage_is_stored_as_null(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let usage = sqlx::query("INSERT INTO model_calls (id, workspace_id, space_id, job_id, task_id, run_id, dot_id, provider_id, model, residency, attempt, usage_complete, charged_tokens, latency_ms, outcome) VALUES ($1, $2, $3, $4, $5, $6, $7, 'fake', 'test', 'local', 1, false, 200, 1, 'error') RETURNING input_tokens, cache_read_tokens, cache_write_tokens, output_tokens, cost_micros, charged_tokens, usage_complete").bind(Uuid::now_v7()).bind(f.workspace).bind(f.space).bind(f.job).bind(f.task).bind(f.run).bind(f.dot).fetch_one(&pool).await.unwrap();
    for column in [
        "input_tokens",
        "cache_read_tokens",
        "cache_write_tokens",
        "output_tokens",
        "cost_micros",
    ] {
        assert_eq!(usage.get::<Option<i64>, _>(column), None);
    }
    assert_eq!(usage.get::<i64, _>("charged_tokens"), 200);
    assert!(!usage.get::<bool, _>("usage_complete"));
}

#[sqlx::test]
async fn space_allow_external_has_no_default(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    sqlstate(
        sqlx::query(
            "INSERT INTO spaces (id, workspace_id, name) VALUES ($1, $2, 'Missing policy')",
        )
        .bind(Uuid::now_v7())
        .bind(f.workspace)
        .execute(&pool)
        .await
        .unwrap_err(),
        "23502",
    );
}

#[sqlx::test]
async fn side_effect_wire_values_pass_the_schema_check(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let effects = [
        SideEffect::Read,
        SideEffect::Navigate,
        SideEffect::WorkspaceRead,
        SideEffect::Draft,
        SideEffect::WorkspaceWrite,
        SideEffect::Exec,
        SideEffect::CredentialUse,
        SideEffect::Destructive,
        SideEffect::Outward(OutwardKind::Send),
        SideEffect::Outward(OutwardKind::Publish),
        SideEffect::Outward(OutwardKind::Pay),
        SideEffect::Outward(OutwardKind::Share),
        SideEffect::Outward(OutwardKind::ExternalApi),
    ];
    for (step, effect) in effects.iter().enumerate() {
        f.tool_call(&pool, step as i32, Some(effect.wire()), "started")
            .await
            .unwrap();
    }
    sqlstate(
        f.tool_call(&pool, 99, Some("shell"), "started")
            .await
            .unwrap_err(),
        "23514",
    );
}

#[sqlx::test]
async fn only_refused_calls_may_have_no_side_effect(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.tool_call(&pool, 1, None, "refused").await.unwrap();
    for state in [
        "started",
        "succeeded",
        "failed",
        "timed_out",
        "outcome_unknown",
    ] {
        sqlstate(
            f.tool_call(&pool, 2, None, state).await.unwrap_err(),
            "23514",
        );
    }
}

#[sqlx::test]
async fn fence_locks_last_until_commit_and_allow_foreign_keys(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.live_lease(&pool).await;
    let fence = Fence {
        task_id: TaskId(f.task),
        epoch: LeaseEpoch(1),
    };
    let mut writer = pool.begin().await.unwrap();
    fence::check(&mut writer, &fence).await.unwrap();
    let mut other = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL lock_timeout = '100ms'")
        .execute(&mut *other)
        .await
        .unwrap();
    fence::check(&mut other, &fence).await.unwrap();
    sqlx::query("INSERT INTO runs (id, workspace_id, job_id, task_id, dot_id, kind, lease_epoch, dot_config_hash, state) VALUES ($1, $2, $3, $4, $5, 'recovery', 1, 'hash', 'running')").bind(Uuid::now_v7()).bind(f.workspace).bind(f.job).bind(f.task).bind(f.dot).execute(&mut *other).await.unwrap();
    other.commit().await.unwrap();
    let mut updater = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL lock_timeout = '100ms'")
        .execute(&mut *updater)
        .await
        .unwrap();
    sqlstate(
        sqlx::query("UPDATE tasks SET state = 'done' WHERE id = $1")
            .bind(f.task)
            .execute(&mut *updater)
            .await
            .unwrap_err(),
        "55P03",
    );
    updater.rollback().await.unwrap();
    writer.commit().await.unwrap();

    let mut terminal = pool.begin().await.unwrap();
    fence::lock(&mut terminal, &fence).await.unwrap();
    let mut blocked = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL lock_timeout = '100ms'")
        .execute(&mut *blocked)
        .await
        .unwrap();
    sqlx::query("INSERT INTO runs (id, workspace_id, job_id, task_id, dot_id, kind, lease_epoch, dot_config_hash, state) VALUES ($1, $2, $3, $4, $5, 'recovery', 1, 'hash', 'running')").bind(Uuid::now_v7()).bind(f.workspace).bind(f.job).bind(f.task).bind(f.dot).execute(&mut *blocked).await.unwrap();
    let error = fence::check(&mut blocked, &fence).await.unwrap_err();
    let katwork::error::KatError::Database(error) = error else {
        panic!("expected database lock timeout")
    };
    sqlstate(error, "55P03");
    blocked.rollback().await.unwrap();
    sqlx::query("UPDATE tasks SET state = 'done' WHERE id = $1")
        .bind(f.task)
        .execute(&mut *terminal)
        .await
        .unwrap();
    terminal.commit().await.unwrap();
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        fence::check(&mut conn, &fence).await.unwrap_err().code(),
        "lease_lost"
    );
}

#[sqlx::test]
async fn schema_checks_reject_invalid_domain_values(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for query in [
        "UPDATE users SET locale = 'unknown'",
        "UPDATE dots SET status = 'unknown'",
        "UPDATE dots SET model_tier = 'unknown'",
        "UPDATE dots SET autonomy = 'unknown'",
        "UPDATE dots SET max_concurrency = 0",
        "UPDATE messages SET author_kind = 'unknown'",
        "UPDATE jobs SET decided_by = 'unknown'",
        "UPDATE tasks SET hold_reason = 'unknown'",
        "UPDATE runs SET kind = 'unknown'",
        "UPDATE runs SET state = 'unknown'",
    ] {
        sqlstate(
            sqlx::query(query).execute(&pool).await.unwrap_err(),
            "23514",
        );
    }
    sqlstate(
        sqlx::query(
            "INSERT INTO memberships (workspace_id, user_id, role) VALUES ($1, $2, 'unknown')",
        )
        .bind(f.workspace)
        .bind(f.user)
        .execute(&pool)
        .await
        .unwrap_err(),
        "23514",
    );
    f.tool_call(&pool, 1, Some("read"), "started")
        .await
        .unwrap();
    sqlstate(
        sqlx::query("UPDATE tool_calls SET attempts = 3")
            .execute(&pool)
            .await
            .unwrap_err(),
        "23514",
    );
    sqlstate(
        sqlx::query("UPDATE tool_calls SET state = 'unknown'")
            .execute(&pool)
            .await
            .unwrap_err(),
        "23514",
    );
}

#[sqlx::test]
async fn team_allows_only_one_lead(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    sqlx::query("INSERT INTO team_members (workspace_id, team_id, dot_id, is_lead, position) VALUES ($1, $2, $3, true, 0)").bind(f.workspace).bind(f.team).bind(f.dot).execute(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    let dot = Uuid::now_v7();
    sqlx::query("INSERT INTO dots (id, workspace_id, default_space_id, name, role, persona, instructions, tool_allowlist, model_tier, autonomy) SELECT $1, workspace_id, default_space_id, name, role, persona, instructions, tool_allowlist, model_tier, autonomy FROM dots WHERE id = $2").bind(dot).bind(f.dot).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO dot_spaces (workspace_id, dot_id, space_id) VALUES ($1, $2, $3)")
        .bind(f.workspace)
        .bind(dot)
        .bind(f.space)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlstate(sqlx::query("INSERT INTO team_members (workspace_id, team_id, dot_id, is_lead, position) VALUES ($1, $2, $3, true, 1)").bind(f.workspace).bind(f.team).bind(dot).execute(&mut *tx).await.unwrap_err(), "23505");
}

#[sqlx::test]
async fn typed_ids_bind_and_decode(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let task: TaskId = sqlx::query_scalar("SELECT id FROM tasks WHERE id = $1")
        .bind(TaskId(f.task))
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(task, TaskId(f.task));
    let epoch: LeaseEpoch = sqlx::query_scalar("SELECT lease_epoch FROM tasks WHERE id = $1")
        .bind(task)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(epoch, LeaseEpoch(0));
}
