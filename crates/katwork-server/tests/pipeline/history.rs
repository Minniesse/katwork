use super::*;
use katwork::{
    chat::{Author, NewMessage},
    pipeline,
};

async fn append(
    f: &Fixture,
    author: Author,
    text: String,
    job: Option<JobId>,
) -> katwork::chat::Message {
    message::append(
        &mut f.core.pool.acquire().await.unwrap(),
        &f.tenant,
        f.thread,
        NewMessage {
            id: None,
            author,
            body: text,
            client_msg_id: None,
            idempotency_key: None,
            synthetic: false,
            stopped: false,
            question: None,
            meta: None,
            job_id: job,
        },
    )
    .await
    .unwrap()
    .message
}
fn text(m: &ChatMsg) -> &str {
    match m {
        ChatMsg::User { content, .. }
        | ChatMsg::Assistant { content, .. }
        | ChatMsg::Tool { content, .. } => content,
    }
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn history_budget_keeps_latest_user_message_whole(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let a = append(
        &f,
        katwork::chat::Author::User(f.tenant.user_id()),
        "old".into(),
        None,
    )
    .await;
    let b = append(
        &f,
        katwork::chat::Author::User(f.tenant.user_id()),
        "ไทย".repeat(100),
        None,
    )
    .await;
    let h = pipeline::build_history(&[a, b.clone()], f.kats[0].id, 4);
    assert_eq!(h.cut, 1);
    assert_eq!(h.messages.len(), 1);
    assert_eq!(text(&h.messages[0]), b.body);
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn other_kats_messages_are_user_role_with_name(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let a = append(&f, Author::Kat(f.kats[0].id), "Mine".into(), None).await;
    let b = append(&f, Author::Kat(f.kats[2].id), "Other".into(), None).await;
    let c = append(&f, Author::User(f.tenant.user_id()), "Request".into(), None).await;
    let h = pipeline::build_history(&[a, b, c], f.kats[0].id, 24000);
    assert!(matches!(&h.messages[0],ChatMsg::Assistant{content,..} if content=="Mine"));
    assert_eq!(
        h.messages[1],
        ChatMsg::User {
            content: "Other".into(),
            name: Some("Writer".into())
        }
    );
    assert_eq!(
        h.messages[2],
        ChatMsg::User {
            content: "Request".into(),
            name: Some("Person".into())
        }
    );
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn queued_job_history_includes_earlier_reply_and_ends_with_request(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    sqlx::query("UPDATE kats SET max_concurrency=1 WHERE id=$1")
        .bind(f.kats[0].id)
        .execute(&pool)
        .await
        .unwrap();
    let (_, a) = f.submit("A").await;
    let c = super::cancel::claim(&f).await;
    assert_eq!(c.job_id, a);
    let (b, _) = f.submit("B").await;
    let (run, reply, _) = super::cancel::start_run(&f, &c).await;
    assert!(reply.0 > b.0);
    let mut facts = katwork::agent::RunFacts::not_run(katwork::agent::StopReason::NoEligibleKat);
    facts.run_id = Some(run);
    facts.reply_message_id = reply;
    facts.stop = None;
    facts.final_text = Some("R1".into());
    lease::complete(&f.core, c.task_id, "worker", c.lease_epoch, facts)
        .await
        .unwrap();
    let h = pipeline::load_history(&f.core, &f.tenant, f.thread, b, f.kats[0].id, 24000)
        .await
        .unwrap();
    assert_eq!(
        h.messages.iter().map(text).collect::<Vec<_>>(),
        vec!["A", "R1", "B"]
    );
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn history_excludes_system_notices_and_later_jobs(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    append(&f, Author::System, "quota_exceeded".into(), None).await;
    let (a, _) = f.submit("A").await;
    let (_, b) = f.submit("B").await;
    append(&f, Author::Kat(f.kats[0].id), "later reply".into(), Some(b)).await;
    let h = pipeline::load_history(&f.core, &f.tenant, f.thread, a, f.kats[0].id, 24000)
        .await
        .unwrap();
    assert_eq!(h.messages.iter().map(text).collect::<Vec<_>>(), vec!["A"]);
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn history_query_stops_at_budget(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for _ in 0..100 {
        append(
            &f,
            Author::User(f.tenant.user_id()),
            "x".repeat(30000),
            None,
        )
        .await;
    }
    let (request, _) = f.submit(&"y".repeat(30000)).await;
    let h = pipeline::load_history(&f.core, &f.tenant, f.thread, request, f.kats[0].id, 24000)
        .await
        .unwrap();
    let rows = sqlx::query(include_str!("../../../katwork/src/pipeline_history.sql"))
        .bind(f.tenant.workspace_id())
        .bind(f.thread)
        .bind(request)
        .bind(24000i64)
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "cut bodies must stay in PostgreSQL");
    assert_eq!(h.messages.len(), 1);
    assert_eq!(h.cut, 100);
    assert_eq!(text(&h.messages[0]), "y".repeat(30000));
}
