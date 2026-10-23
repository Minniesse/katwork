use super::*;

#[sqlx::test]
async fn message_and_task_idempotency_keys_are_unique(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for (table, columns, values) in [
        (
            "messages",
            "id, workspace_id, thread_id, author_kind, author_user_id, body",
            "$1, $2, $3, 'user', $4, ''",
        ),
        (
            "tasks",
            "id, workspace_id, job_id, kat_id, kat_policy_version, input, state",
            "$1, $2, $3, $4, 1, '{}', 'queued'",
        ),
    ] {
        let sql = format!("INSERT INTO {table} ({columns}, idempotency_key) VALUES ({values}, $5)");
        for (key, duplicate) in [
            (Some("first"), false),
            (Some("second"), false),
            (None, false),
            (None, false),
            (Some("first"), true),
        ] {
            let result = sqlx::QueryBuilder::<sqlx::Postgres>::new(&sql)
                .build()
                .bind(Uuid::now_v7())
                .bind(f.workspace)
                .bind(if table == "messages" { f.thread } else { f.job })
                .bind(if table == "messages" { f.user } else { f.kat })
                .bind(key)
                .execute(&pool)
                .await;
            if duplicate {
                sqlstate(result.unwrap_err(), "23505");
            } else {
                result.unwrap();
            }
        }
    }
}

#[sqlx::test]
async fn schema_defaults_and_required_accounting_fields_are_enforced(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let defaults: (bool, bool, i32, i32, i64, Option<serde_json::Value>) = sqlx::query_as(
        "SELECT w.paused, m.synthetic, t.attempts, t.max_attempts, t.lease_epoch, t.pending_tool_call
         FROM workspaces w JOIN messages m ON m.workspace_id=w.id
         JOIN tasks t ON t.workspace_id=w.id WHERE w.id=$1 AND m.id=$2 AND t.id=$3",
    )
    .bind(f.workspace)
    .bind(f.message)
    .bind(f.task)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(defaults, (false, false, 0, 3, 0, None));

    for (column, value) in [("", ""), (", kat_policy_version", ", NULL")] {
        let sql = format!(
            "INSERT INTO tasks (id, workspace_id, job_id, kat_id, input, state{column})
             VALUES ($1, $2, $3, $4, '{{}}', 'queued'{value})"
        );
        let error = sqlx::QueryBuilder::<sqlx::Postgres>::new(&sql)
            .build()
            .bind(Uuid::now_v7())
            .bind(f.workspace)
            .bind(f.job)
            .bind(f.kat)
            .execute(&pool)
            .await
            .unwrap_err();
        assert_eq!(
            error
                .as_database_error()
                .unwrap()
                .downcast_ref::<sqlx::postgres::PgDatabaseError>()
                .column(),
            Some("kat_policy_version")
        );
        sqlstate(error, "23502");
    }
    for (missing, columns, values) in [
        ("charged_tokens", "usage_complete", "false"),
        (
            "charged_tokens",
            "usage_complete, charged_tokens",
            "false, NULL",
        ),
        ("usage_complete", "charged_tokens", "200"),
        (
            "usage_complete",
            "charged_tokens, usage_complete",
            "200, NULL",
        ),
    ] {
        let sql = format!(
            "INSERT INTO model_calls (id, workspace_id, space_id, job_id, task_id, run_id,
             kat_id, provider_id, model, residency, attempt, latency_ms, outcome, {columns})
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'fake', 'test', 'local', 1, 1, 'error', {values})"
        );
        let error = sqlx::QueryBuilder::<sqlx::Postgres>::new(&sql)
            .build()
            .bind(Uuid::now_v7())
            .bind(f.workspace)
            .bind(f.space)
            .bind(f.job)
            .bind(f.task)
            .bind(f.run)
            .bind(f.kat)
            .execute(&pool)
            .await
            .unwrap_err();
        assert_eq!(
            error
                .as_database_error()
                .unwrap()
                .downcast_ref::<sqlx::postgres::PgDatabaseError>()
                .column(),
            Some(missing)
        );
        sqlstate(error, "23502");
    }
}

#[sqlx::test]
async fn client_message_id_is_scoped_to_thread_and_author(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let other = Fixture::new(&pool).await;
    let thread = Uuid::now_v7();
    sqlx::query("INSERT INTO threads (id, workspace_id, space_id, team_id, title) VALUES ($1, $2, $3, $4, 'Another thread')")
        .bind(thread).bind(f.workspace).bind(f.space).bind(f.team)
        .execute(&pool).await.unwrap();
    for (thread, author, duplicate) in [
        (f.thread, f.user, false),
        (thread, f.user, false),
        (f.thread, other.user, false),
        (f.thread, f.user, true),
    ] {
        let result = sqlx::query("INSERT INTO messages (id, workspace_id, thread_id, author_kind, author_user_id, body, client_msg_id) VALUES ($1, $2, $3, 'user', $4, '', 'retry')")
            .bind(Uuid::now_v7()).bind(f.workspace).bind(thread).bind(author)
            .execute(&pool).await;
        if duplicate {
            sqlstate(result.unwrap_err(), "23505");
        } else {
            result.unwrap();
        }
    }
}
