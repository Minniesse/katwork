use super::*;
use sqlx::{Postgres, QueryBuilder};

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
async fn history_latest_job_and_rate_queries_have_indexes(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL enable_seqscan=off")
        .execute(&mut *tx)
        .await
        .unwrap();
    for (query, id, index) in [
        (
            "EXPLAIN SELECT id,body FROM messages WHERE workspace_id=$1 AND thread_id=$2 AND author_kind<>'system' AND id<=$3 ORDER BY id DESC",
            f.thread,
            "messages_thread_id",
        ),
        (
            "EXPLAIN SELECT id FROM jobs WHERE workspace_id=$1 AND thread_id=$2 ORDER BY id DESC LIMIT 1",
            f.thread,
            "jobs_thread_id",
        ),
        (
            "EXPLAIN SELECT count(*) FROM tasks WHERE workspace_id=$1 AND kat_id=$2 AND created_at>statement_timestamp()-interval '60 seconds'",
            f.kat,
            "tasks_kat_created",
        ),
    ] {
        let mut query_builder = sqlx::query_scalar::<_, String>(query)
            .bind(f.workspace)
            .bind(id);
        if index == "messages_thread_id" {
            query_builder = query_builder.bind(f.message);
        }
        let plan = query_builder.fetch_all(&mut *tx).await.unwrap().join("\n");
        assert!(plan.contains(index), "{plan}");
    }
}
