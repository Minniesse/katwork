use super::*;
use katwork::{
    chat::{NoticeKat, NoticeMeta},
    error::KatError,
    ids::{RunId, TaskId},
    types::{Question, ReasonCode},
};

async fn job(pool: &PgPool, f: &Fixture, message: MessageId) -> JobId {
    let id = JobId::new();
    sqlx::query("INSERT INTO jobs (id,workspace_id,thread_id,request_message_id,requested_by) VALUES ($1,$2,$3,$4,$5)").bind(id).bind(f.tenant.workspace_id()).bind(f.thread).bind(message).bind(f.tenant.user_id()).execute(pool).await.unwrap();
    id
}

async fn run(pool: &PgPool, f: &Fixture, job: JobId) -> RunId {
    let kat: KatId =
        sqlx::query_scalar("SELECT id FROM kats WHERE workspace_id=$1 ORDER BY id LIMIT 1")
            .bind(f.tenant.workspace_id())
            .fetch_one(pool)
            .await
            .unwrap();
    let task = TaskId::new();
    let run = RunId::new();
    sqlx::query("INSERT INTO tasks (id,workspace_id,job_id,kat_id,kat_policy_version,input,state) VALUES ($1,$2,$3,$4,1,'{}','leased')").bind(task).bind(f.tenant.workspace_id()).bind(job).bind(kat).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO runs (id,workspace_id,job_id,task_id,kat_id,kind,lease_epoch,kat_config_hash,state) VALUES ($1,$2,$3,$4,$5,'initial',1,'hash','running')").bind(run).bind(f.tenant.workspace_id()).bind(job).bind(task).bind(kat).execute(pool).await.unwrap();
    run
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn messages_list_pages_without_gaps_or_duplicates(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut ids = Vec::new();
    for batch in 0..3 {
        let mut tx = pool.begin().await.unwrap();
        for i in 0..40 {
            ids.push(
                message::append(
                    &mut tx,
                    &f.tenant,
                    f.thread,
                    f.new_message(&format!("{batch}-{i}")),
                )
                .await
                .unwrap()
                .message
                .id,
            );
        }
        tx.commit().await.unwrap();
    }
    let mut pages = Vec::new();
    let mut before = None;
    loop {
        let query = before.map_or("?limit=50".into(), |id| format!("?limit=50&before={id}"));
        let page = f.page(&query).await;
        let rows = page["messages"].as_array().unwrap();
        if rows.is_empty() {
            break;
        }
        before = Some(rows[0]["id"].as_str().unwrap().to_owned());
        pages.push(
            rows.iter()
                .map(|v| serde_json::from_value::<MessageId>(v["id"].clone()).unwrap())
                .collect::<Vec<_>>(),
        );
    }
    assert_eq!(
        pages.iter().map(Vec::len).collect::<Vec<_>>(),
        vec![50, 50, 20]
    );
    assert_eq!(pages.into_iter().rev().flatten().collect::<Vec<_>>(), ids);
    assert_eq!(
        f.page("?limit=65535").await["messages"]
            .as_array()
            .unwrap()
            .len(),
        120
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn list_event_seq_is_read_before_messages(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let writer_pool = pool.clone();
    let tenant = f.tenant.clone();
    let thread = f.thread;
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let writer_barrier = barrier.clone();
    let writer = tokio::spawn(async move {
        writer_barrier.wait().await;
        for seq in 1..=40i64 {
            let mut tx = writer_pool.begin().await.unwrap();
            sqlx::query("SELECT id FROM threads WHERE id=$1 AND workspace_id=$2 FOR NO KEY UPDATE")
                .bind(thread)
                .bind(tenant.workspace_id())
                .execute(&mut *tx)
                .await
                .unwrap();
            sqlx::query("INSERT INTO messages (id,workspace_id,thread_id,author_kind,body) VALUES ($1,$2,$3,'system',$4)").bind(MessageId::new()).bind(tenant.workspace_id()).bind(thread).bind(seq.to_string()).execute(&mut *tx).await.unwrap();
            sqlx::query("UPDATE threads SET last_event_seq=$3 WHERE id=$1 AND workspace_id=$2")
                .bind(thread)
                .bind(tenant.workspace_id())
                .bind(seq)
                .execute(&mut *tx)
                .await
                .unwrap();
            sqlx::query("INSERT INTO events (workspace_id,thread_id,seq,kind,payload) VALUES ($1,$2,$3,'CUSTOM','{}')").bind(tenant.workspace_id()).bind(thread).bind(seq).execute(&mut *tx).await.unwrap();
            tx.commit().await.unwrap();
            tokio::task::yield_now().await;
        }
    });
    barrier.wait().await;
    for _ in 0..60 {
        let page = message::list(&pool, &f.tenant, f.thread, None, 200)
            .await
            .unwrap();
        for seq in 1..=page.event_seq.0 {
            assert!(
                page.messages.iter().any(|m| m.body == seq.to_string()),
                "lost seq {seq}"
            );
        }
        tokio::task::yield_now().await;
    }
    writer.await.unwrap();
    assert_eq!(f.page("").await["messages"].as_array().unwrap().len(), 40);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn list_event_seq_rewinds_to_the_oldest_running_run(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut runs = Vec::new();
    for seq in [4i64, 7] {
        let message = message::append(
            &mut pool.acquire().await.unwrap(),
            &f.tenant,
            f.thread,
            f.new_message("go"),
        )
        .await
        .unwrap()
        .message
        .id;
        let run = run(&pool, &f, job(&pool, &f, message).await).await;
        sqlx::query("INSERT INTO events (workspace_id,thread_id,seq,run_id,kind,payload) VALUES ($1,$2,$3,$4,'RUN_STARTED','{}')").bind(f.tenant.workspace_id()).bind(f.thread).bind(seq).bind(run).execute(&pool).await.unwrap();
        runs.push(run);
    }
    sqlx::query("UPDATE threads SET last_event_seq=9 WHERE id=$1")
        .bind(f.thread)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(f.page("").await["event_seq"], 3);
    sqlx::query("UPDATE runs SET state='finished' WHERE id=$1")
        .bind(runs[0])
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(f.page("").await["event_seq"], 6);
    sqlx::query("UPDATE runs SET state='finished' WHERE id=$1")
        .bind(runs[1])
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(f.page("").await["event_seq"], 9);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn message_json_carries_author_name_and_job_status(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let msg = message::append(
        &mut pool.acquire().await.unwrap(),
        &f.tenant,
        f.thread,
        f.new_message("go"),
    )
    .await
    .unwrap()
    .message;
    let job = job(&pool, &f, msg.id).await;
    message::set_job(&mut pool.acquire().await.unwrap(), &f.tenant, msg.id, job)
        .await
        .unwrap();
    let page = f.page("").await;
    assert_eq!(page["messages"][0]["author_name"], "Person");
    assert_eq!(page["messages"][0]["job_id"], json!(job));
    assert_eq!(page["messages"][0]["job_status"], Value::Null);
    sqlx::query("UPDATE jobs SET status='stopped',reason='provider_quota' WHERE id=$1")
        .bind(job)
        .execute(&pool)
        .await
        .unwrap();
    let page = f.page("").await;
    assert_eq!(page["messages"][0]["job_status"], "stopped");
    assert_eq!(page["messages"][0]["job_reason"], "provider_quota");
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn kat_reply_and_system_notice_round_trip(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (kat, name): (KatId, String) =
        sqlx::query_as("SELECT id,name FROM kats WHERE workspace_id=$1 ORDER BY id LIMIT 1")
            .bind(f.tenant.workspace_id())
            .fetch_one(&pool)
            .await
            .unwrap();
    let mut reply = f.new_message("partial 👩‍💻");
    reply.author = Author::Kat(kat);
    reply.synthetic = true;
    reply.stopped = true;
    reply.question = Some(Question {
        question: "Which?".into(),
        options: vec!["One".into()],
    });
    let stored = message::append(
        &mut pool.acquire().await.unwrap(),
        &f.tenant,
        f.thread,
        reply,
    )
    .await
    .unwrap()
    .message;
    assert_eq!(stored.author_name, name);
    assert!(stored.synthetic && stored.stopped);
    assert_eq!(stored.question.unwrap().question, "Which?");
    let mut notice = f.new_message("kat_paused");
    notice.author = Author::System;
    notice.meta = Some(NoticeMeta {
        code: ReasonCode::KatPaused,
        kats: vec![NoticeKat { id: kat, name }],
    });
    let stored = message::append(
        &mut pool.acquire().await.unwrap(),
        &f.tenant,
        f.thread,
        notice,
    )
    .await
    .unwrap()
    .message;
    assert_eq!(
        serde_json::to_value(stored.author).unwrap(),
        json!({"kind":"system"})
    );
    assert_eq!(stored.author_name, "");
    assert_eq!(stored.meta.unwrap().kats[0].id, kat);
    assert_eq!(f.page("").await["messages"][0]["author"]["kind"], "kat");
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn append_concurrent_duplicates_return_one_row(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(20));
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..20 {
        let pool = pool.clone();
        let tenant = f.tenant.clone();
        let thread = f.thread;
        let barrier = barrier.clone();
        let mut msg = f.new_message("same");
        msg.client_msg_id = Some("same".into());
        tasks.spawn(async move {
            barrier.wait().await;
            message::append(&mut pool.acquire().await.unwrap(), &tenant, thread, msg)
                .await
                .unwrap()
        });
    }
    let mut created = 0;
    let mut ids = std::collections::HashSet::new();
    while let Some(result) = tasks.join_next().await {
        let row = result.unwrap();
        created += usize::from(row.created);
        ids.insert(row.message.id);
    }
    assert_eq!(created, 1);
    assert_eq!(ids.len(), 1);
    assert_eq!(f.page("").await["messages"].as_array().unwrap().len(), 1);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn append_primary_key_collision_is_an_error(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut conn = pool.acquire().await.unwrap();
    let msg = message::append(&mut conn, &f.tenant, f.thread, f.new_message("one"))
        .await
        .unwrap()
        .message;
    let other = thread::create(&pool, &f.tenant, f.space, f.team, "")
        .await
        .unwrap();
    let mut collision = f.new_message("two");
    collision.id = Some(msg.id);
    assert!(
        message::append(&mut conn, &f.tenant, other.id, collision)
            .await
            .is_err()
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn append_idempotency_key_returns_original_and_never_crosses_tenant(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut conn = pool.acquire().await.unwrap();
    let mut first = f.new_message("original");
    first.idempotency_key = Some("reply:one".into());
    let first = message::append(&mut conn, &f.tenant, f.thread, first)
        .await
        .unwrap();
    let mut retry = f.new_message("original");
    retry.idempotency_key = Some("reply:one".into());
    let retry = message::append(&mut conn, &f.tenant, f.thread, retry)
        .await
        .unwrap();
    assert!(!retry.created);
    assert_eq!(retry.message.id, first.message.id);
    let (_, _, other) = foreign_thread(&pool).await;
    assert!(matches!(
        message::append(&mut conn, &f.tenant, other, f.new_message("x")).await,
        Err(KatError::NotFound)
    ));
    assert!(matches!(
        message::set_job(&mut conn, &f.tenant, MessageId::new(), JobId::new()).await,
        Err(KatError::NotFound)
    ));
}
