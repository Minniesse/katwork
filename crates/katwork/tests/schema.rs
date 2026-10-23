#[path = "schema/legacy.rs"]
mod legacy;
mod support;

use katwork::{
    db,
    fence::{self, Fence},
    ids::{LeaseEpoch, TaskId},
    types::{HoldReason, JobStatus, OutwardKind, SideEffect},
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
            "events",
            "jobs",
            "kat_spaces",
            "kats",
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
    let old_names: Vec<String> = sqlx::query_scalar(
        "SELECT relname::text FROM pg_class WHERE relnamespace='public'::regnamespace AND relname LIKE '%dot%'
         UNION ALL SELECT conname::text FROM pg_constraint WHERE connamespace='public'::regnamespace AND (conname LIKE '%dot%' OR pg_get_constraintdef(oid) LIKE '%dot%')
         UNION ALL SELECT column_name::text FROM information_schema.columns WHERE table_schema='public' AND column_name LIKE '%dot%'",
    ).fetch_all(&pool).await.unwrap();
    assert!(old_names.is_empty(), "legacy schema names: {old_names:?}");
    for name in [
        "kats_pkey",
        "kats_id_workspace_id_key",
        "kat_spaces_pkey",
        "tasks_live_leases_by_kat",
        "model_calls_kat_daily_tokens",
    ] {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(name)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(exists, "missing index: {name}");
    }
    Fixture::new(&pool).await;
}

#[sqlx::test]
async fn job_status_rejects_unknown_value(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut tx = pool.begin().await.unwrap();
    let message = Uuid::now_v7();
    sqlx::query("INSERT INTO messages (id, workspace_id, thread_id, author_kind, body) VALUES ($1, $2, $3, 'system', '')").bind(message).bind(f.workspace).bind(f.thread).execute(&mut *tx).await.unwrap();
    let error = sqlx::query("INSERT INTO jobs (id, workspace_id, thread_id, request_message_id, requested_by, status) VALUES ($1, $2, $3, $4, $5, 'done')").bind(Uuid::now_v7()).bind(f.workspace).bind(f.thread).bind(message).bind(f.user).execute(&mut *tx).await.unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().constraint(),
        Some("jobs_status_check")
    );
    sqlstate(error, "23514");
}

#[sqlx::test]
async fn job_status_wire_values_pass_the_schema_check(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for status in [
        JobStatus::Accepted,
        JobStatus::Ready,
        JobStatus::Answered,
        JobStatus::Stopped,
        JobStatus::NeedsInput,
        JobStatus::Cancelled,
        JobStatus::Error,
    ] {
        sqlx::query("UPDATE jobs SET status = $1 WHERE id = $2")
            .bind(status.wire())
            .bind(f.job)
            .execute(&pool)
            .await
            .unwrap();
    }
}

#[sqlx::test]
async fn hold_reason_wire_values_pass_the_schema_check(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for reason in [
        HoldReason::KatBusy,
        HoldReason::ProviderQuota,
        HoldReason::WorkspacePaused,
    ] {
        sqlx::query("UPDATE tasks SET hold_reason = $1 WHERE id = $2")
            .bind(reason.wire())
            .bind(f.task)
            .execute(&pool)
            .await
            .unwrap();
    }
}

#[sqlx::test]
async fn message_author_columns_match_kind(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for kind in ["user", "kat", "system"] {
        for user in [None, Some(f.user)] {
            for kat in [None, Some(f.kat)] {
                let result = sqlx::query("INSERT INTO messages (id, workspace_id, thread_id, author_kind, author_user_id, author_kat_id, body) VALUES ($1, $2, $3, $4, $5, $6, '')")
                    .bind(Uuid::now_v7()).bind(f.workspace).bind(f.thread).bind(kind).bind(user).bind(kat).execute(&pool).await;
                if (kind == "user") == user.is_some() && (kind == "kat") == kat.is_some() {
                    result.unwrap();
                } else {
                    sqlstate(result.unwrap_err(), "23514");
                }
            }
        }
    }
}

#[sqlx::test]
async fn client_msg_id_requires_user_author(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for (kind, kat) in [("system", None), ("kat", Some(f.kat))] {
        sqlstate(sqlx::query("INSERT INTO messages (id, workspace_id, thread_id, author_kind, author_kat_id, body, client_msg_id) VALUES ($1, $2, $3, $4, $5, '', 'retry')")
            .bind(Uuid::now_v7()).bind(f.workspace).bind(f.thread).bind(kind).bind(kat).execute(&pool).await.unwrap_err(), "23514");
    }
}

#[sqlx::test]
async fn task_lease_is_reached_only_by_task_id(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    sqlx::query("INSERT INTO task_leases (task_id, worker_id, lease_epoch, expires_at) VALUES ($1, 'worker', 1, clock_timestamp() + interval '1 hour')")
        .bind(f.task).execute(&pool).await.unwrap();
    let columns: Vec<String> = sqlx::query_scalar("SELECT column_name FROM information_schema.columns WHERE table_schema = 'public' AND table_name = 'task_leases' ORDER BY ordinal_position").fetch_all(&pool).await.unwrap();
    assert_eq!(
        columns,
        ["task_id", "worker_id", "lease_epoch", "expires_at"]
    );
}

#[sqlx::test]
async fn fence_uses_current_time_after_transaction_start(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.live_lease(&pool).await;
    let fence = Fence {
        task_id: TaskId(f.task),
        epoch: LeaseEpoch(1),
    };
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE task_leases SET expires_at = clock_timestamp() WHERE task_id = $1")
        .bind(f.task)
        .execute(&pool)
        .await
        .unwrap();
    let expired_after_begin: bool = sqlx::query_scalar("SELECT expires_at > now() AND expires_at <= clock_timestamp() FROM task_leases WHERE task_id = $1").bind(f.task).fetch_one(&mut *tx).await.unwrap();
    assert!(expired_after_begin);
    assert_eq!(
        fence::check(&mut tx, &fence).await.unwrap_err().code(),
        "lease_lost"
    );
    assert_eq!(
        fence::lock(&mut tx, &fence).await.unwrap_err().code(),
        "lease_lost"
    );
}

#[sqlx::test]
async fn fence_waiter_rechecks_task_state_and_epoch(pool: PgPool) {
    use std::time::Duration;
    let f = Fixture::new(&pool).await;
    f.live_lease(&pool).await;
    let fence = Fence {
        task_id: TaskId(f.task),
        epoch: LeaseEpoch(1),
    };
    for exclusive in [false, true] {
        for mutation in [
            "UPDATE tasks SET state = 'done' WHERE id = $1",
            "UPDATE tasks SET lease_epoch = 2 WHERE id = $1",
        ] {
            sqlx::query("UPDATE tasks SET state = 'leased', lease_epoch = 1 WHERE id = $1")
                .bind(f.task)
                .execute(&pool)
                .await
                .unwrap();
            let mut terminal = pool.begin().await.unwrap();
            fence::lock(&mut terminal, &fence).await.unwrap();
            let mut waiting = pool.begin().await.unwrap();
            let check = async {
                if exclusive {
                    fence::lock(&mut waiting, &fence).await
                } else {
                    fence::check(&mut waiting, &fence).await
                }
            };
            tokio::pin!(check);
            assert!(
                tokio::time::timeout(Duration::from_millis(100), check.as_mut())
                    .await
                    .is_err()
            );
            sqlx::query(mutation)
                .bind(f.task)
                .execute(&mut *terminal)
                .await
                .unwrap();
            terminal.commit().await.unwrap();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(5), check)
                    .await
                    .unwrap()
                    .unwrap_err()
                    .code(),
                "lease_lost"
            );
        }
    }
}

#[sqlx::test]
async fn fence_check_rejects_expiry_during_lock_wait_after_commit(pool: PgPool) {
    fence_rejects_expiry_during_lock_wait(pool, false, false).await;
}

#[sqlx::test]
async fn fence_check_rejects_expiry_during_lock_wait_after_rollback(pool: PgPool) {
    fence_rejects_expiry_during_lock_wait(pool, false, true).await;
}

#[sqlx::test]
async fn fence_lock_rejects_expiry_during_lock_wait_after_commit(pool: PgPool) {
    fence_rejects_expiry_during_lock_wait(pool, true, false).await;
}

#[sqlx::test]
async fn fence_lock_rejects_expiry_during_lock_wait_after_rollback(pool: PgPool) {
    fence_rejects_expiry_during_lock_wait(pool, true, true).await;
}

async fn fence_rejects_expiry_during_lock_wait(pool: PgPool, exclusive: bool, rollback: bool) {
    use std::time::Duration;
    let f = Fixture::new(&pool).await;
    f.live_lease(&pool).await;
    let fence = Fence {
        task_id: TaskId(f.task),
        epoch: LeaseEpoch(1),
    };
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM tasks WHERE id = $1 FOR NO KEY UPDATE")
        .bind(f.task)
        .execute(&mut *holder)
        .await
        .unwrap();
    let mut waiting = pool.begin().await.unwrap();
    let waiting_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *waiting)
        .await
        .unwrap();
    sqlx::query("UPDATE task_leases SET expires_at = clock_timestamp() + interval '1 second' WHERE task_id = $1")
        .bind(f.task).execute(&pool).await.unwrap();
    let check = async {
        if exclusive {
            fence::lock(&mut waiting, &fence).await
        } else {
            fence::check(&mut waiting, &fence).await
        }
    };
    let release = async {
        while !sqlx::query_scalar::<_, bool>("SELECT cardinality(pg_blocking_pids($1)) > 0")
            .bind(waiting_pid)
            .fetch_one(&pool)
            .await
            .unwrap()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let expired = || {
            sqlx::query_scalar::<_, bool>(
                "SELECT expires_at <= clock_timestamp() FROM task_leases WHERE task_id = $1",
            )
            .bind(f.task)
            .fetch_one(&pool)
        };
        assert!(!expired().await.unwrap(), "lease must be live when blocked");
        while !expired().await.unwrap() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        if rollback {
            holder.rollback().await.unwrap();
        } else {
            holder.commit().await.unwrap();
        }
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(check, release)
    })
    .await
    .expect("fence must finish after the lock holder exits");
    assert_eq!(result.unwrap_err().code(), "lease_lost");
}

#[sqlx::test]
async fn claim_live_lease_and_quota_queries_have_indexes(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL enable_seqscan = off")
        .execute(&mut *tx)
        .await
        .unwrap();
    for (query, index) in [
        (
            "EXPLAIN SELECT id FROM tasks WHERE state = 'queued' ORDER BY created_at LIMIT 1",
            "tasks_claim_state_created_at",
        ),
        (
            "EXPLAIN SELECT count(*) FROM tasks JOIN task_leases ON task_leases.task_id = tasks.id WHERE tasks.kat_id = $1 AND tasks.state = 'leased' AND task_leases.expires_at > clock_timestamp()",
            "tasks_live_leases_by_kat",
        ),
        (
            // Task 12: run this quota query after locking the Kat, with one fresh cutoff.
            "EXPLAIN SELECT sum(charged_tokens) FROM model_calls WHERE kat_id = $1 AND created_at > statement_timestamp() - interval '24 hours'",
            "model_calls_kat_daily_tokens",
        ),
    ] {
        let has_kat = query.contains("$1");
        let query = sqlx::query_scalar::<_, String>(query);
        let query = if has_kat { query.bind(f.kat) } else { query };
        let plan = query.fetch_all(&mut *tx).await.unwrap().join("\n");
        assert!(plan.contains(index), "{index}: {plan}");
        if index == "model_calls_kat_daily_tokens" {
            assert!(
                plan.lines()
                    .any(|line| line.contains("Index Cond:") && line.contains("created_at")),
                "quota cutoff must be an index condition: {plan}"
            );
        }
    }
}

#[sqlx::test]
async fn thai_text_round_trips(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let name: String = sqlx::query_scalar("SELECT name FROM kats WHERE id = $1")
        .bind(f.kat)
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
async fn kat_default_space_must_be_granted(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO kats (id, workspace_id, default_space_id, name, role, persona, instructions, tool_allowlist, model_tier, autonomy) SELECT $1, workspace_id, default_space_id, name, role, persona, instructions, tool_allowlist, model_tier, autonomy FROM kats WHERE id = $2").bind(Uuid::now_v7()).bind(f.kat).execute(&mut *tx).await.unwrap();
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
    let usage = sqlx::query("INSERT INTO model_calls (id, workspace_id, space_id, job_id, task_id, run_id, kat_id, provider_id, model, residency, attempt, usage_complete, charged_tokens, latency_ms, outcome) VALUES ($1, $2, $3, $4, $5, $6, $7, 'fake', 'test', 'local', 1, false, 200, 1, 'error') RETURNING input_tokens, cache_read_tokens, cache_write_tokens, output_tokens, cost_micros, charged_tokens, usage_complete").bind(Uuid::now_v7()).bind(f.workspace).bind(f.space).bind(f.job).bind(f.task).bind(f.run).bind(f.kat).fetch_one(&pool).await.unwrap();
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
    sqlx::query("INSERT INTO runs (id, workspace_id, job_id, task_id, kat_id, kind, lease_epoch, kat_config_hash, state) VALUES ($1, $2, $3, $4, $5, 'recovery', 1, 'hash', 'running')").bind(Uuid::now_v7()).bind(f.workspace).bind(f.job).bind(f.task).bind(f.kat).execute(&mut *other).await.unwrap();
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
    sqlx::query("INSERT INTO runs (id, workspace_id, job_id, task_id, kat_id, kind, lease_epoch, kat_config_hash, state) VALUES ($1, $2, $3, $4, $5, 'recovery', 1, 'hash', 'running')").bind(Uuid::now_v7()).bind(f.workspace).bind(f.job).bind(f.task).bind(f.kat).execute(&mut *blocked).await.unwrap();
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
        "UPDATE kats SET status = 'unknown'",
        "UPDATE kats SET model_tier = 'unknown'",
        "UPDATE kats SET autonomy = 'unknown'",
        "UPDATE kats SET max_concurrency = 0",
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
    sqlx::query("INSERT INTO team_members (workspace_id, team_id, kat_id, is_lead, position) VALUES ($1, $2, $3, true, 0)").bind(f.workspace).bind(f.team).bind(f.kat).execute(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    let kat = Uuid::now_v7();
    sqlx::query("INSERT INTO kats (id, workspace_id, default_space_id, name, role, persona, instructions, tool_allowlist, model_tier, autonomy) SELECT $1, workspace_id, default_space_id, name, role, persona, instructions, tool_allowlist, model_tier, autonomy FROM kats WHERE id = $2").bind(kat).bind(f.kat).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO kat_spaces (workspace_id, kat_id, space_id) VALUES ($1, $2, $3)")
        .bind(f.workspace)
        .bind(kat)
        .bind(f.space)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlstate(sqlx::query("INSERT INTO team_members (workspace_id, team_id, kat_id, is_lead, position) VALUES ($1, $2, $3, true, 1)").bind(f.workspace).bind(f.team).bind(kat).execute(&mut *tx).await.unwrap_err(), "23505");
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

fn workspace_fk(error: sqlx::Error, constraint: &str) {
    assert_eq!(
        error.as_database_error().unwrap().constraint(),
        Some(constraint)
    );
    sqlstate(error, "23503");
}

#[sqlx::test]
async fn kat_default_space_rejects_another_workspace(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let other = Fixture::new(&pool).await;
    let mut tx = pool.begin().await.unwrap();
    workspace_fk(sqlx::query("INSERT INTO kats (id, workspace_id, default_space_id, name, role, persona, instructions, tool_allowlist, model_tier, autonomy) SELECT $1, workspace_id, $2, name, role, persona, instructions, tool_allowlist, model_tier, autonomy FROM kats WHERE id = $3")
        .bind(Uuid::now_v7()).bind(other.space).bind(f.kat).execute(&mut *tx).await.unwrap_err(), "kats_default_space_id_workspace_id_fkey");
}

#[sqlx::test]
async fn kat_space_grants_reject_cross_workspace_parents(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let other = Fixture::new(&pool).await;
    for (kat, space, constraint) in [
        (other.kat, f.space, "kat_spaces_kat_id_workspace_id_fkey"),
        (f.kat, other.space, "kat_spaces_space_id_workspace_id_fkey"),
    ] {
        workspace_fk(
            sqlx::query(
                "INSERT INTO kat_spaces (workspace_id, kat_id, space_id) VALUES ($1, $2, $3)",
            )
            .bind(f.workspace)
            .bind(kat)
            .bind(space)
            .execute(&pool)
            .await
            .unwrap_err(),
            constraint,
        );
    }
}

#[sqlx::test]
async fn team_rejects_space_in_another_workspace(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let other = Fixture::new(&pool).await;
    workspace_fk(
        sqlx::query(
            "INSERT INTO teams (id, workspace_id, space_id, name) VALUES ($1, $2, $3, 'Team')",
        )
        .bind(Uuid::now_v7())
        .bind(f.workspace)
        .bind(other.space)
        .execute(&pool)
        .await
        .unwrap_err(),
        "teams_space_id_workspace_id_fkey",
    );
}

#[sqlx::test]
async fn team_members_reject_cross_workspace_parents(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let other = Fixture::new(&pool).await;
    for (team, kat, constraint) in [
        (other.team, f.kat, "team_members_team_id_workspace_id_fkey"),
        (f.team, other.kat, "team_members_kat_id_workspace_id_fkey"),
    ] {
        workspace_fk(sqlx::query("INSERT INTO team_members (workspace_id, team_id, kat_id, is_lead, position) VALUES ($1, $2, $3, false, 0)")
            .bind(f.workspace).bind(team).bind(kat).execute(&pool).await.unwrap_err(), constraint);
    }
}

#[sqlx::test]
async fn threads_reject_cross_workspace_parents(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let other = Fixture::new(&pool).await;
    for (space, team, constraint) in [
        (other.space, f.team, "threads_space_id_workspace_id_fkey"),
        (f.space, other.team, "threads_team_id_workspace_id_fkey"),
    ] {
        workspace_fk(sqlx::query("INSERT INTO threads (id, workspace_id, space_id, team_id, title) VALUES ($1, $2, $3, $4, 'Thread')")
            .bind(Uuid::now_v7()).bind(f.workspace).bind(space).bind(team).execute(&pool).await.unwrap_err(), constraint);
    }
}

#[sqlx::test]
async fn messages_reject_cross_workspace_parents(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let other = Fixture::new(&pool).await;
    for (thread, kat, job, constraint) in [
        (
            other.thread,
            f.kat,
            f.job,
            "messages_thread_id_workspace_id_fkey",
        ),
        (
            f.thread,
            other.kat,
            f.job,
            "messages_author_kat_id_workspace_id_fkey",
        ),
        (
            f.thread,
            f.kat,
            other.job,
            "messages_job_id_workspace_id_fkey",
        ),
    ] {
        workspace_fk(sqlx::query("INSERT INTO messages (id, workspace_id, thread_id, author_kind, author_kat_id, job_id, body) VALUES ($1, $2, $3, 'kat', $4, $5, '')")
            .bind(Uuid::now_v7()).bind(f.workspace).bind(thread).bind(kat).bind(job).execute(&pool).await.unwrap_err(), constraint);
    }
}

#[sqlx::test]
async fn jobs_reject_cross_workspace_parents(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let other = Fixture::new(&pool).await;
    let local_message = Uuid::now_v7();
    let foreign_message = Uuid::now_v7();
    for (message, fixture) in [(local_message, &f), (foreign_message, &other)] {
        sqlx::query("INSERT INTO messages (id, workspace_id, thread_id, author_kind, body) VALUES ($1, $2, $3, 'system', '')")
            .bind(message).bind(fixture.workspace).bind(fixture.thread).execute(&pool).await.unwrap();
    }
    for (thread, message, parent, constraint) in [
        (
            other.thread,
            local_message,
            f.job,
            "jobs_thread_id_workspace_id_fkey",
        ),
        (
            f.thread,
            foreign_message,
            f.job,
            "jobs_request_message_id_workspace_id_fkey",
        ),
        (
            f.thread,
            local_message,
            other.job,
            "jobs_parent_job_id_workspace_id_fkey",
        ),
    ] {
        workspace_fk(sqlx::query("INSERT INTO jobs (id, workspace_id, thread_id, request_message_id, requested_by, parent_job_id) VALUES ($1, $2, $3, $4, $5, $6)")
            .bind(Uuid::now_v7()).bind(f.workspace).bind(thread).bind(message).bind(f.user).bind(parent).execute(&pool).await.unwrap_err(), constraint);
    }
}

#[sqlx::test]
async fn tasks_reject_cross_workspace_parents(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let other = Fixture::new(&pool).await;
    for (job, kat, constraint) in [
        (other.job, f.kat, "tasks_job_id_workspace_id_fkey"),
        (f.job, other.kat, "tasks_kat_id_workspace_id_fkey"),
    ] {
        workspace_fk(sqlx::query("INSERT INTO tasks (id, workspace_id, job_id, kat_id, kat_policy_version, input, state) VALUES ($1, $2, $3, $4, 1, '{}', 'queued')")
            .bind(Uuid::now_v7()).bind(f.workspace).bind(job).bind(kat).execute(&pool).await.unwrap_err(), constraint);
    }
}

#[sqlx::test]
async fn runs_reject_cross_workspace_parents(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let other = Fixture::new(&pool).await;
    for (job, task, kat, constraint) in [
        (other.job, f.task, f.kat, "runs_job_id_workspace_id_fkey"),
        (f.job, other.task, f.kat, "runs_task_id_workspace_id_fkey"),
        (f.job, f.task, other.kat, "runs_kat_id_workspace_id_fkey"),
    ] {
        workspace_fk(sqlx::query("INSERT INTO runs (id, workspace_id, job_id, task_id, kat_id, kind, lease_epoch, kat_config_hash, state) VALUES ($1, $2, $3, $4, $5, 'initial', 1, 'hash', 'running')")
            .bind(Uuid::now_v7()).bind(f.workspace).bind(job).bind(task).bind(kat).execute(&pool).await.unwrap_err(), constraint);
    }
}

#[sqlx::test]
async fn tool_calls_reject_run_in_another_workspace(pool: PgPool) {
    let mut f = Fixture::new(&pool).await;
    let other = Fixture::new(&pool).await;
    f.run = other.run;
    workspace_fk(
        f.tool_call(&pool, 0, Some("read"), "started")
            .await
            .unwrap_err(),
        "tool_calls_run_id_workspace_id_fkey",
    );
}

#[sqlx::test]
async fn model_calls_reject_cross_workspace_parents(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let other = Fixture::new(&pool).await;
    for (space, job, task, run, kat, constraint) in [
        (
            other.space,
            f.job,
            f.task,
            f.run,
            f.kat,
            "model_calls_space_id_workspace_id_fkey",
        ),
        (
            f.space,
            other.job,
            f.task,
            f.run,
            f.kat,
            "model_calls_job_id_workspace_id_fkey",
        ),
        (
            f.space,
            f.job,
            other.task,
            f.run,
            f.kat,
            "model_calls_task_id_workspace_id_fkey",
        ),
        (
            f.space,
            f.job,
            f.task,
            other.run,
            f.kat,
            "model_calls_run_id_workspace_id_fkey",
        ),
        (
            f.space,
            f.job,
            f.task,
            f.run,
            other.kat,
            "model_calls_kat_id_workspace_id_fkey",
        ),
    ] {
        workspace_fk(sqlx::query("INSERT INTO model_calls (id, workspace_id, space_id, job_id, task_id, run_id, kat_id, provider_id, model, residency, attempt, usage_complete, charged_tokens, latency_ms, outcome) VALUES ($1, $2, $3, $4, $5, $6, $7, 'fake', 'test', 'local', 1, false, 200, 1, 'error')")
            .bind(Uuid::now_v7()).bind(f.workspace).bind(space).bind(job).bind(task).bind(run).bind(kat).execute(&pool).await.unwrap_err(), constraint);
    }
}

#[sqlx::test]
async fn events_reject_cross_workspace_parents(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let other = Fixture::new(&pool).await;
    for (thread, run, constraint) in [
        (other.thread, f.run, "events_thread_id_workspace_id_fkey"),
        (f.thread, other.run, "events_run_id_workspace_id_fkey"),
    ] {
        workspace_fk(sqlx::query("INSERT INTO events (thread_id, seq, workspace_id, run_id, kind, payload) VALUES ($1, 1, $2, $3, 'CUSTOM', '{}')")
            .bind(thread).bind(f.workspace).bind(run).execute(&pool).await.unwrap_err(), constraint);
    }
}

#[sqlx::test]
async fn normalized_emails_are_unique(pool: PgPool) {
    sqlx::query("INSERT INTO users (id, email, password_hash, display_name, locale) VALUES ($1, 'a@x', 'hash', 'A', 'en')")
        .bind(Uuid::now_v7()).execute(&pool).await.unwrap();
    sqlstate(sqlx::query("INSERT INTO users (id, email, password_hash, display_name, locale) VALUES ($1, 'a@x', 'hash', 'a', 'en')")
        .bind(Uuid::now_v7()).execute(&pool).await.unwrap_err(), "23505");
}

#[sqlx::test]
async fn users_reject_non_lowercase_email_on_insert_and_update(pool: PgPool) {
    sqlstate(sqlx::query("INSERT INTO users (id, email, password_hash, display_name, locale) VALUES ($1, 'Mixed@example.test', 'hash', 'Test', 'en')")
        .bind(Uuid::now_v7()).execute(&pool).await.unwrap_err(), "23514");
    let f = Fixture::new(&pool).await;
    sqlstate(
        sqlx::query("UPDATE users SET email = 'Mixed@example.test' WHERE id = $1")
            .bind(f.user)
            .execute(&pool)
            .await
            .unwrap_err(),
        "23514",
    );
}
