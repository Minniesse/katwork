use super::{Fixture, sqlstate};
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test(migrations = false)]
async fn team_membership_migration_preserves_existing_rows_and_constraints(pool: PgPool) {
    for migration in [
        include_str!("../../migrations/0001_init.sql"),
        include_str!("../../migrations/0002_email_unique.sql"),
        include_str!("../../migrations/0003_email_lowercase.sql"),
    ] {
        sqlx::raw_sql(migration).execute(&pool).await.unwrap();
    }
    let f = Fixture::new_legacy(&pool).await;
    sqlx::query("INSERT INTO team_members (workspace_id,team_id,dot_id,is_lead,position) VALUES ($1,$2,$3,true,7)")
        .bind(f.workspace).bind(f.team).bind(f.kat).execute(&pool).await.unwrap();
    let mut member: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(m) FROM team_members m")
            .fetch_one(&pool)
            .await
            .unwrap();
    let mut team: serde_json::Value = sqlx::query_scalar("SELECT to_jsonb(t) FROM teams t")
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("../../migrations/0004_team_membership.sql"))
        .execute(&pool)
        .await
        .unwrap();
    member["active"] = serde_json::json!(true);
    team["is_default"] = serde_json::json!(false);
    assert_eq!(
        sqlx::query_scalar::<_, serde_json::Value>("SELECT to_jsonb(m) FROM team_members m")
            .fetch_one(&pool)
            .await
            .unwrap(),
        member
    );
    assert_eq!(
        sqlx::query_scalar::<_, serde_json::Value>("SELECT to_jsonb(t) FROM teams t")
            .fetch_one(&pool)
            .await
            .unwrap(),
        team
    );
    for query in [
        "UPDATE dots SET daily_token_budget=-1 WHERE id=$1",
        "UPDATE dots SET max_concurrency=65536 WHERE id=$1",
    ] {
        sqlstate(
            sqlx::query(query)
                .bind(f.kat)
                .execute(&pool)
                .await
                .unwrap_err(),
            "23514",
        );
    }
    let other_kat = Uuid::now_v7();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO dots (id,workspace_id,default_space_id,name,role,persona,instructions,tool_allowlist,model_tier,autonomy) SELECT $1,workspace_id,default_space_id,'Other',role,persona,instructions,tool_allowlist,model_tier,autonomy FROM dots WHERE id=$2")
        .bind(other_kat).bind(f.kat).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO dot_spaces (workspace_id,dot_id,space_id) VALUES ($1,$2,$3)")
        .bind(f.workspace)
        .bind(other_kat)
        .bind(f.space)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sqlstate(sqlx::query("INSERT INTO team_members (workspace_id,team_id,dot_id,is_lead,position) VALUES ($1,$2,$3,true,8)")
        .bind(f.workspace).bind(f.team).bind(other_kat).execute(&pool).await.unwrap_err(), "23505");
    sqlx::query("INSERT INTO team_members (workspace_id,team_id,dot_id,is_lead,position,active) VALUES ($1,$2,$3,true,8,false)")
        .bind(f.workspace).bind(f.team).bind(other_kat).execute(&pool).await.unwrap();
    sqlstate(
        sqlx::query("UPDATE team_members SET active=true WHERE dot_id=$1")
            .bind(other_kat)
            .execute(&pool)
            .await
            .unwrap_err(),
        "23505",
    );
    sqlx::query("UPDATE teams SET is_default=true WHERE id=$1")
        .bind(f.team)
        .execute(&pool)
        .await
        .unwrap();
    sqlstate(sqlx::query("INSERT INTO teams (id,workspace_id,space_id,name,is_default) VALUES ($1,$2,$3,'Second default',true)")
        .bind(Uuid::now_v7()).bind(f.workspace).bind(f.space).execute(&pool).await.unwrap_err(), "23505");
}

#[sqlx::test(migrations = false)]
async fn rename_preserves_populated_database(pool: PgPool) {
    let mut historical = sqlx::migrate!();
    historical.migrations = std::borrow::Cow::Owned(
        historical
            .iter()
            .filter(|m| m.version <= 4)
            .cloned()
            .collect(),
    );
    historical.run(&pool).await.unwrap();
    let f = Fixture::new_legacy(&pool).await;
    sqlx::query("INSERT INTO team_members (workspace_id,team_id,dot_id,is_lead,position) VALUES ($1,$2,$3,true,7)")
        .bind(f.workspace).bind(f.team).bind(f.kat).execute(&pool).await.unwrap();
    sqlx::query("UPDATE dots SET appearance = '{\"avatar\":\"dot\",\"accent\":\"blue\"}', instructions = 'Dot is a word in my instructions', name = 'Dot archive'")
        .execute(&pool).await.unwrap();
    sqlx::query("UPDATE messages SET author_kind='dot', author_user_id=NULL, author_dot_id=$1, body='Dots in user content stay verbatim'")
        .bind(f.kat).execute(&pool).await.unwrap();
    sqlx::query("UPDATE tasks SET hold_reason='dot_busy'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET reason='no_eligible_dot'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE runs SET stop_reason='dot_paused'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO model_calls (id,workspace_id,space_id,job_id,task_id,run_id,dot_id,provider_id,model,residency,attempt,usage_complete,charged_tokens,latency_ms,outcome) VALUES ($1,$2,$3,$4,$5,$6,$7,'fake','test','local',1,false,200,1,'error')")
        .bind(Uuid::now_v7()).bind(f.workspace).bind(f.space).bind(f.job).bind(f.task).bind(f.run).bind(f.kat).execute(&pool).await.unwrap();
    let mut expected = Vec::new();
    for (old_query, new_query, new_table, columns) in [
        (
            "SELECT to_jsonb(t) FROM dots t",
            "SELECT to_jsonb(t) FROM kats t",
            "kats",
            vec![],
        ),
        (
            "SELECT to_jsonb(t) FROM dot_spaces t",
            "SELECT to_jsonb(t) FROM kat_spaces t",
            "kat_spaces",
            vec![("dot_id", "kat_id")],
        ),
        (
            "SELECT to_jsonb(t) FROM team_members t",
            "SELECT to_jsonb(t) FROM team_members t",
            "team_members",
            vec![("dot_id", "kat_id")],
        ),
        (
            "SELECT to_jsonb(t) FROM messages t",
            "SELECT to_jsonb(t) FROM messages t",
            "messages",
            vec![("author_dot_id", "author_kat_id")],
        ),
        (
            "SELECT to_jsonb(t) FROM tasks t",
            "SELECT to_jsonb(t) FROM tasks t",
            "tasks",
            vec![
                ("dot_id", "kat_id"),
                ("dot_policy_version", "kat_policy_version"),
            ],
        ),
        (
            "SELECT to_jsonb(t) FROM runs t",
            "SELECT to_jsonb(t) FROM runs t",
            "runs",
            vec![("dot_id", "kat_id"), ("dot_config_hash", "kat_config_hash")],
        ),
        (
            "SELECT to_jsonb(t) FROM model_calls t",
            "SELECT to_jsonb(t) FROM model_calls t",
            "model_calls",
            vec![("dot_id", "kat_id")],
        ),
        (
            "SELECT to_jsonb(t) FROM jobs t",
            "SELECT to_jsonb(t) FROM jobs t",
            "jobs",
            vec![],
        ),
    ] {
        let mut row: serde_json::Value = sqlx::query_scalar(old_query)
            .fetch_one(&pool)
            .await
            .unwrap();
        let object = row.as_object_mut().unwrap();
        for (old, new) in columns {
            let value = object.remove(old).unwrap();
            object.insert(new.into(), value);
        }
        match new_table {
            "kats" => row["appearance"]["avatar"] = serde_json::json!("kat"),
            "messages" => {
                row["author_kind"] = serde_json::json!("kat");
                row["meta"] = serde_json::Value::Null;
                row["stopped"] = serde_json::json!(false);
            }
            "tasks" => {
                row["hold_reason"] = serde_json::json!("kat_busy");
                row["first_claimed_at"] = serde_json::Value::Null;
            }
            "jobs" => row["reason"] = serde_json::json!("no_eligible_kat"),
            "runs" => row["stop_reason"] = serde_json::json!("kat_paused"),
            _ => {}
        }
        expected.push((new_query, row));
    }
    katwork::db::migrate(&pool).await.unwrap();
    katwork::db::migrate(&pool).await.unwrap();
    for (query, expected) in expected {
        let actual: serde_json::Value = sqlx::query_scalar(query).fetch_one(&pool).await.unwrap();
        assert_eq!(actual, expected, "{query}");
    }
    for query in [
        "UPDATE messages SET author_kind='dot'",
        "UPDATE tasks SET hold_reason='dot_busy'",
        "UPDATE messages SET author_kat_id=NULL",
        "DELETE FROM kat_spaces",
    ] {
        assert!(sqlx::query(query).execute(&pool).await.is_err(), "{query}");
    }
}
