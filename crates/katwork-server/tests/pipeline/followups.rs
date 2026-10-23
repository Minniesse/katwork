use super::*;
use katwork::{
    agent::{Limits, RunFacts, StopReason},
    jobs::{RouteRule, job},
};

async fn parent_of(pool: &PgPool, job: JobId) -> Option<JobId> {
    sqlx::query_scalar("SELECT parent_job_id FROM jobs WHERE id=$1")
        .bind(job)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn claim_stopped_answer_can_be_resent(pool: PgPool, reason: &str, change: &'static str) {
    let f = Fixture::new(&pool).await;
    let parent = f.awaiting().await;
    let (_, stopped) = f.submit("my answer").await;
    assert_eq!(f.task(stopped).await.0, f.kats[2].id);
    assert_eq!(parent_of(&pool, stopped).await, Some(parent));
    sqlx::query(change)
        .bind(f.kats[2].id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        lease::claim(
            &f.core,
            "worker",
            Duration::from_secs(60),
            &Limits::default()
        )
        .await
        .unwrap()
        .is_none()
    );
    assert_eq!(
        f.status(stopped).await,
        (Some("stopped".into()), Some(reason.into()))
    );
    let runs: i64 = sqlx::query_scalar("SELECT count(*) FROM runs WHERE job_id=$1")
        .bind(stopped)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(runs, 0);
    sqlx::query("UPDATE kats SET status='active',daily_token_budget=NULL WHERE id=$1")
        .bind(f.kats[2].id)
        .execute(&pool)
        .await
        .unwrap();
    let (_, retry) = f.submit("my answer again").await;
    assert_eq!(parent_of(&pool, retry).await, Some(parent));
    let (kat, input) = f.task(retry).await;
    assert_eq!(kat, f.kats[2].id);
    assert_eq!(input.routing.rule, RouteRule::FollowUp);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn quota_stopped_at_claim_keeps_question(pool: PgPool) {
    claim_stopped_answer_can_be_resent(
        pool,
        "quota_exceeded",
        "UPDATE kats SET daily_token_budget=0 WHERE id=$1",
    )
    .await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn kat_paused_at_claim_keeps_question(pool: PgPool) {
    claim_stopped_answer_can_be_resent(
        pool,
        "kat_paused",
        "UPDATE kats SET status='paused',policy_version=policy_version+1 WHERE id=$1",
    )
    .await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn policy_changed_at_claim_keeps_question(pool: PgPool) {
    claim_stopped_answer_can_be_resent(
        pool,
        "policy_changed",
        "UPDATE kats SET policy_version=policy_version+1 WHERE id=$1",
    )
    .await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn stopped_answer_that_started_run_remains_latest(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.awaiting().await;
    let (_, stopped) = f.submit("my answer").await;
    let c = super::cancel::claim(&f).await;
    assert_eq!(c.job_id, stopped);
    let (run, reply, _) = super::cancel::start_run(&f, &c).await;
    let mut facts = RunFacts::not_run(StopReason::TimeLimit);
    facts.run_id = Some(run);
    facts.reply_message_id = reply;
    lease::complete(&f.core, c.task_id, "worker", c.lease_epoch, facts)
        .await
        .unwrap();
    assert_eq!(
        f.status(stopped).await,
        (Some("stopped".into()), Some("time_limit".into()))
    );
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        job::latest_in_thread(&mut conn, &f.tenant, f.thread)
            .await
            .unwrap()
            .unwrap()
            .id,
        stopped
    );
    let (_, next) = f.submit("next request").await;
    assert_eq!(parent_of(&pool, next).await, None);
    let (kat, input) = f.task(next).await;
    assert_eq!(kat, f.kats[0].id);
    assert_eq!(input.routing.rule, RouteRule::Lead);
}

async fn thai_missing_space_reply(
    pool: PgPool,
    text: &str,
    missing: &[usize],
    paused: bool,
    expected: usize,
    rule: RouteRule,
) {
    let f = Fixture::with_locale(&pool, 5000, Locale::Th).await;
    assert_eq!(f.kats[4].name, "เลขา");
    assert_eq!(f.kats[0].name, "หัวหน้าทีม");
    let parent = f.awaiting_kat(&f.kats[4]).await;
    if paused {
        f.pause(f.kats[4].id).await;
    }
    let (_, reply) = f.submit(text).await;
    assert_eq!(parent_of(&pool, reply).await, Some(parent));
    let (kat, input) = f.task(reply).await;
    assert_eq!(kat, f.kats[expected].id);
    assert_eq!(input.routing.rule, rule);
    let notices = f.notices().await;
    let expected: Vec<_> = missing
        .iter()
        .map(|&i| {
            json!({"code":"mention_needs_space","kats":[{"id":f.kats[i].id,"name":f.kats[i].name}]})
        })
        .collect();
    assert_eq!(notices, expected);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn thai_missing_space_asking_kat_keeps_follow_up(pool: PgPool) {
    thai_missing_space_reply(
        pool,
        "@เลขาวันพุธบ่ายสองโมง",
        &[4],
        false,
        4,
        RouteRule::FollowUp,
    )
    .await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn thai_missing_space_other_kat_routes_to_lead(pool: PgPool) {
    thai_missing_space_reply(pool, "@นักวิจัยช่วยหาข้อมูล", &[1], false, 0, RouteRule::Lead).await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn thai_missing_space_mixed_kats_routes_to_lead(pool: PgPool) {
    thai_missing_space_reply(
        pool,
        "@เลขาวันพุธบ่ายสองโมง @นักวิจัยช่วยหาข้อมูล",
        &[4, 1],
        false,
        0,
        RouteRule::Lead,
    )
    .await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn thai_missing_space_paused_asking_kat_routes_to_lead(pool: PgPool) {
    thai_missing_space_reply(pool, "@เลขาวันพุธบ่ายสองโมง", &[4], true, 0, RouteRule::Lead).await;
}
