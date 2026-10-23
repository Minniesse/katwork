use super::*;
use sqlx::{Postgres, QueryBuilder};

#[sqlx::test]
async fn scheduler_migration_can_create_its_job_index(pool: PgPool) {
    sqlx::query("CREATE INDEX jobs_thread_id ON jobs (thread_id, id DESC)")
        .execute(&pool)
        .await
        .unwrap();
    let unused: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('tasks_kat_created')::text")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(unused, None);
}

#[sqlx::test]
async fn message_list_uses_thread_id_index(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL enable_seqscan=off")
        .execute(&mut *tx)
        .await
        .unwrap();
    for before in [None, Some(Uuid::now_v7())] {
        let plan = QueryBuilder::<Postgres>::new("EXPLAIN ")
            .push(include_str!("../../src/chat/message_select.sql"))
            .push(" AND ($3::uuid IS NULL OR m.id<$3) ORDER BY m.id DESC LIMIT $4")
            .build_query_scalar::<String>()
            .bind(f.workspace)
            .bind(f.thread)
            .bind(before)
            .bind(50i64)
            .fetch_all(&mut *tx)
            .await
            .unwrap()
            .join("\n");
        assert!(plan.contains("messages_thread_id"), "{plan}");
    }
}

#[sqlx::test]
async fn thread_list_uses_space_index(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL enable_seqscan=off")
        .execute(&mut *tx)
        .await
        .unwrap();
    let plan = QueryBuilder::<Postgres>::new("EXPLAIN ")
        .push(include_str!("../../src/chat/thread_list.sql"))
        .build_query_scalar::<String>()
        .bind(f.workspace)
        .bind(f.space)
        .bind(50i64)
        .fetch_all(&mut *tx)
        .await
        .unwrap()
        .join("\n");
    assert!(plan.contains("threads_space_id"), "{plan}");
}

#[sqlx::test]
async fn event_seq_cursor_uses_run_started_index(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL enable_seqscan=off")
        .execute(&mut *tx)
        .await
        .unwrap();
    let plan = QueryBuilder::<Postgres>::new("EXPLAIN ")
        .push(include_str!("../../src/chat/event_seq.sql"))
        .build_query_scalar::<String>()
        .bind(f.workspace)
        .bind(f.thread)
        .fetch_all(&mut *tx)
        .await
        .unwrap()
        .join("\n");
    assert!(plan.contains("events_run_started"), "{plan}");
}

#[sqlx::test]
async fn history_uses_thread_id_index(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL enable_seqscan=off")
        .execute(&mut *tx)
        .await
        .unwrap();
    let plan = sqlx::query_scalar::<_, String>("EXPLAIN SELECT id,body FROM messages WHERE workspace_id=$1 AND thread_id=$2 AND author_kind<>'system' AND id<=$3 ORDER BY id DESC")
        .bind(f.workspace).bind(f.thread).bind(f.message)
        .fetch_all(&mut *tx).await.unwrap().join("\n");
    assert!(plan.contains("messages_thread_id"), "{plan}");
}
