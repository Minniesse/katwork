#![allow(
    clippy::disallowed_methods,
    reason = "Test fixtures drain SQL; cancellation probes deliberately drop futures"
)]
#[allow(dead_code)]
mod support;

use axum::{Router, response::Response};
use katwork::{
    chat::{message, thread},
    config::Config,
    ids::*,
    jobs::{TaskInput, lease, worker},
    models::*,
    runtime::Core,
    team::{Kat, team},
    tenancy::{self, Tenant},
    types::{Locale, ModelTier},
};
use katwork_server::app::{AppState, build_router};
use serde_json::{Value, json};
use sqlx::{PgPool, types::Json};
use std::{sync::Arc, time::Duration};
use support::{body, request};
use tokio::time::timeout;

async fn wait_for_locks(pool: &PgPool, table: &str, count: i64) {
    timeout(Duration::from_secs(3), async {
        loop {
            let blocked: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE $1")
                .bind(format!("SELECT%FROM {table}%"))
                .fetch_one(pool).await.unwrap();
            if blocked >= count { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("expected database lock wait");
}

struct Fixture {
    core: Arc<Core>,
    app: Router,
    cookie: String,
    tenant: Tenant,
    thread: ThreadId,
    space: SpaceId,
    kats: Vec<Kat>,
}
impl Fixture {
    async fn new(pool: &PgPool) -> Self {
        Self::with_timeout(pool, 5000).await
    }
    async fn with_timeout(pool: &PgPool, lock_timeout: u64) -> Self {
        Self::with_locale(pool, lock_timeout, Locale::En).await
    }
    async fn with_locale(pool: &PgPool, lock_timeout: u64, locale: Locale) -> Self {
        let (user, password) = support::user(pool).await;
        let workspace = WorkspaceId::new();
        sqlx::query("INSERT INTO workspaces(id,name) VALUES($1,'Pipeline')")
            .bind(workspace)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO memberships(workspace_id,user_id,role) VALUES($1,$2,'owner')")
            .bind(workspace)
            .bind(user)
            .execute(pool)
            .await
            .unwrap();
        let tenant = tenancy::resolve(pool, UserId(user), workspace)
            .await
            .unwrap();
        let space = SpaceId::new();
        sqlx::query(
            "INSERT INTO spaces(id,workspace_id,name,allow_external) VALUES($1,$2,'Chat',false)",
        )
        .bind(space)
        .bind(workspace)
        .execute(pool)
        .await
        .unwrap();
        let team = team::create_default(
            pool,
            &tenant,
            space,
            locale,
            std::time::Duration::from_secs(5),
        )
        .await
        .unwrap();
        let kats = team::get(pool, &tenant, team).await.unwrap().members;
        let thread = thread::create(pool, &tenant, space, team, "")
            .await
            .unwrap()
            .id;
        let config = Config::from_lookup(|key| match key {
            "DATABASE_URL" => Some("postgres://unused".into()),
            "KATWORK_SECRET" => Some(uuid::Uuid::now_v7().to_string()),
            "KATWORK_DEMO" => Some("1".into()),
            "KATWORK_LOCK_TIMEOUT_MS" => Some(lock_timeout.to_string()),
            _ => None,
        })
        .unwrap();
        let core = Arc::new(Core::new(pool.clone(), Arc::new(config)));
        let app = build_router(AppState { core: core.clone() });
        let cookie = support::cookie(&support::login(&app, &password).await);
        Self {
            core,
            app,
            cookie,
            tenant,
            thread,
            space,
            kats,
        }
    }
    fn provider(&mut self, fake: FakeProvider) {
        let registry = Registry::from_entries(
            [ModelTier::Fast, ModelTier::Standard, ModelTier::Strong]
                .into_iter()
                .map(|tier| {
                    (
                        ModelEntry {
                            provider_id: "fake".into(),
                            model: "script".into(),
                            tier,
                            capabilities: fake.capabilities(),
                            max_output_tokens: 2048,
                            temperature: None,
                            price: None,
                            timeouts: ModelTimeouts::HOSTED,
                        },
                        Arc::new(fake.clone()) as Arc<dyn ModelProvider>,
                    )
                })
                .collect(),
        );
        self.core = Arc::new(
            Core::new(self.core.pool.clone(), self.core.config.clone()).with_registry(registry),
        );
        self.app = build_router(AppState {
            core: self.core.clone(),
        });
    }
    async fn call(&self, method: &str, suffix: &str, value: Option<Value>) -> Response {
        request(
            &self.app,
            method,
            &format!("/api/workspaces/{}/{suffix}", self.tenant.workspace_id()),
            value,
            Some(&self.cookie),
        )
        .await
    }
    async fn post(&self, text: &str, key: Option<&str>) -> Response {
        self.call(
            "POST",
            &format!("threads/{}/messages", self.thread),
            Some(json!({"body":text,"client_msg_id":key})),
        )
        .await
    }
    async fn submit(&self, text: &str) -> (MessageId, JobId) {
        let r = self.post(text, None).await;
        let status = r.status();
        let v = body(r).await;
        assert_eq!(status, 201, "{v}");
        assert!(
            v["job_id"].is_string(),
            "POST must atomically create a job: {v}"
        );
        (
            serde_json::from_value(v["message_id"].clone()).unwrap(),
            serde_json::from_value(v["job_id"].clone()).unwrap(),
        )
    }
    async fn task(&self, job: JobId) -> (KatId, TaskInput) {
        let (kat, Json(input)) = sqlx::query_as("SELECT kat_id,input FROM tasks WHERE job_id=$1")
            .bind(job)
            .fetch_one(&self.core.pool)
            .await
            .unwrap();
        (kat, input)
    }
    async fn status(&self, job: JobId) -> (Option<String>, Option<String>) {
        sqlx::query_as("SELECT status,reason FROM jobs WHERE id=$1")
            .bind(job)
            .fetch_one(&self.core.pool)
            .await
            .unwrap()
    }
    async fn events(&self) -> Vec<Value> {
        sqlx::query_scalar::<_, Json<Value>>(
            "SELECT payload FROM events WHERE thread_id=$1 ORDER BY seq",
        )
        .bind(self.thread)
        .fetch_all(&self.core.pool)
        .await
        .unwrap()
        .into_iter()
        .map(|Json(v)| v)
        .collect()
    }
    async fn run(&self, job: JobId) {
        let mut workers = worker::spawn_pool(self.core.clone(), 1, Duration::from_secs(60));
        timeout(Duration::from_secs(15), async {
            while self.status(job).await.0.is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("job must finish");
        self.core.shutdown.cancel();
        while let Some(r) = workers.join_next().await {
            r.unwrap();
        }
        support::assert_pool_clean(&self.core.pool).await;
    }
    async fn pause(&self, kat: KatId) {
        sqlx::query("UPDATE kats SET status='paused',policy_version=policy_version+1 WHERE id=$1")
            .bind(kat)
            .execute(&self.core.pool)
            .await
            .unwrap();
    }
    async fn notices(&self) -> Vec<Value> {
        sqlx::query_scalar::<_, Json<Value>>(
            "SELECT meta FROM messages WHERE thread_id=$1 AND author_kind='system' ORDER BY id",
        )
        .bind(self.thread)
        .fetch_all(&self.core.pool)
        .await
        .unwrap()
        .into_iter()
        .map(|Json(v)| v)
        .collect()
    }
    async fn awaiting(&self) -> JobId {
        self.awaiting_kat(&self.kats[2]).await
    }
    async fn awaiting_kat(&self, kat: &Kat) -> JobId {
        let (_, job) = self.submit(&format!("@{} question", kat.name)).await;
        sqlx::query("UPDATE jobs SET status='needs_input',reason='question_pending',decided_by='policy',finished_at=clock_timestamp() WHERE id=$1").bind(job).execute(&self.core.pool).await.unwrap();
        sqlx::query("UPDATE tasks SET state='done' WHERE job_id=$1")
            .bind(job)
            .execute(&self.core.pool)
            .await
            .unwrap();
        job
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn message_without_mention_goes_to_lead_and_job_answered(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (_, job) = f.submit("ช่วยสรุปงาน").await;
    assert_eq!(f.task(job).await.0, f.kats[0].id);
    f.run(job).await;
    assert_eq!(f.status(job).await, (Some("answered".into()), None));
    let page = message::list(&pool, &f.tenant, f.thread, None, 50)
        .await
        .unwrap();
    assert_eq!(page.messages.len(), 2);
    assert!(page.messages[1].body.contains("สวัสดี"));
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn mention_routes_to_named_kat_including_thai_name(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    sqlx::query("UPDATE kats SET name='นักวิจัย' WHERE id=$1")
        .bind(f.kats[1].id)
        .execute(&pool)
        .await
        .unwrap();
    for text in ["@นักวิจัย ช่วยค้นข้อมูล", "@นักวิจัยfind facts"]
    {
        let (_, job) = f.submit(text).await;
        assert_eq!(f.task(job).await.0, f.kats[1].id);
    }
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn mention_without_space_after_latin_name_routes(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (_, job) = f.submit("@Writerช่วยสรุปไฟล์นี้").await;
    assert_eq!(f.task(job).await.0, f.kats[2].id);
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn first_eligible_mention_wins_among_several(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (_, job) = f.submit("@Analyst then @Writer").await;
    assert_eq!(f.task(job).await.0, f.kats[3].id);
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn second_eligible_mention_gets_extra_mention_notice(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (_, job) = f.submit("@Researcher find X, then @Writer draft it").await;
    let (kat, input) = f.task(job).await;
    assert_eq!(kat, f.kats[1].id);
    assert_eq!(input.routing.ignored, vec![f.kats[2].id]);
    assert_eq!(
        f.notices().await,
        vec![json!({"code":"extra_mention_ignored","kats":[{"id":f.kats[2].id,"name":"Writer"}]})]
    );
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn two_paused_mentions_post_two_named_notices(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.pause(f.kats[2].id).await;
    f.pause(f.kats[3].id).await;
    let (_, job) = f.submit("@Writer @Analyst").await;
    assert_eq!(f.task(job).await.0, f.kats[0].id);
    let notices = f.notices().await;
    assert_eq!(notices.len(), 2);
    assert_eq!(notices[0]["kats"][0]["name"], "Writer");
    assert_eq!(notices[1]["kats"][0]["name"], "Analyst");
    assert!(notices.iter().all(|n| n["code"] == "kat_paused"));
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn paused_kat_leaves_routing_set(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.pause(f.kats[2].id).await;
    let (_, job) = f.submit("@Writer @Analyst").await;
    assert_eq!(f.task(job).await.0, f.kats[3].id);
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn paused_lead_routes_to_first_active_specialist(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.pause(f.kats[0].id).await;
    let (_, job) = f.submit("hello").await;
    assert_eq!(f.task(job).await.0, f.kats[1].id);
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn no_eligible_kat_ends_job_stopped(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for k in &f.kats {
        f.pause(k.id).await;
    }
    let (_, job) = f.submit("hello").await;
    assert_eq!(
        f.status(job).await,
        (Some("stopped".into()), Some("no_eligible_kat".into()))
    );
    assert_eq!(f.notices().await[0]["code"], "no_eligible_kat");
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn follow_up_without_mention_goes_to_the_asking_kat(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.awaiting().await;
    let (_, job) = f.submit("ใช่ เอาแบบ B").await;
    assert_eq!(f.task(job).await.0, f.kats[2].id);
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn follow_up_with_mention_goes_to_the_mentioned_kat(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.awaiting().await;
    let (_, job) = f.submit("@Analyst ...").await;
    assert_eq!(f.task(job).await.0, f.kats[3].id);
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn follow_up_when_asking_kat_paused_falls_back_to_lead(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.awaiting().await;
    f.pause(f.kats[2].id).await;
    let (_, job) = f.submit("yes").await;
    assert_eq!(f.task(job).await.0, f.kats[0].id);
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn reply_to_needs_input_job_starts_linked_follow_up(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let parent = f.awaiting().await;
    let (_, job) = f.submit("yes").await;
    let saved: JobId = sqlx::query_scalar("SELECT parent_job_id FROM jobs WHERE id=$1")
        .bind(job)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(saved, parent);
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn routing_rule_is_recorded_on_the_task(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for (text, rule) in [("hello", "lead"), ("@Writer hello", "mention")] {
        let (_, job) = f.submit(text).await;
        let (_, input) = f.task(job).await;
        assert_eq!(serde_json::to_value(input.routing.rule).unwrap(), rule);
        assert!(input.routing.ignored.is_empty());
    }
    f.awaiting().await;
    let (_, job) = f.submit("yes").await;
    assert_eq!(
        serde_json::to_value(f.task(job).await.1.routing.rule).unwrap(),
        "follow_up"
    );
    f.pause(f.kats[0].id).await;
    let (_, job) = f.submit("next").await;
    assert_eq!(
        serde_json::to_value(f.task(job).await.1.routing.rule).unwrap(),
        "first_eligible"
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn retry_with_same_client_msg_id_creates_one_job(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let a = f.post("hello", Some("retry")).await;
    assert_eq!(a.status(), 201);
    let a = body(a).await;
    let b = f.post("hello", Some("retry")).await;
    assert_eq!(b.status(), 200);
    let b = body(b).await;
    assert_eq!(a, b);
    assert!(a["job_id"].is_string());
    let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM messages),(SELECT count(*) FROM jobs),(SELECT count(*) FROM tasks)").fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (1, 1, 1));
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn concurrent_retries_with_same_client_msg_id_create_one_job(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (a, b) = tokio::join!(f.post("hello", Some("race")), f.post("hello", Some("race")));
    let mut statuses = vec![a.status().as_u16(), b.status().as_u16()];
    statuses.sort();
    assert_eq!(statuses, vec![200, 201]);
    let (a, b) = (body(a).await, body(b).await);
    assert_eq!(a, b);
    assert!(a["job_id"].is_string());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn client_msg_id_reuse_with_different_body_is_409_and_creates_no_job(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    f.post("hello", Some("same")).await;
    support::assert_code(
        f.post("different", Some("same")).await,
        409,
        "client_msg_id_conflict",
    )
    .await;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn seventh_message_in_a_minute_is_stopped_too_many_requests(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for _ in 0..6 {
        let (_, job) = f.submit("hello").await;
        assert_eq!(f.status(job).await.0, None);
    }
    let (_, job) = f.submit("seventh").await;
    assert_eq!(
        f.status(job).await,
        (Some("stopped".into()), Some("too_many_requests".into()))
    );
    assert_eq!(f.notices().await[0]["code"], "too_many_requests");
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn retried_post_does_not_consume_the_rate_limit(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for n in 0..5 {
        let r = f.post("hello", Some(&format!("key{n}"))).await;
        assert_eq!(r.status(), 201);
    }
    for _ in 0..5 {
        assert_eq!(f.post("hello", Some("key0")).await.status(), 200);
    }
    let (_, job) = f.submit("sixth").await;
    assert_eq!(f.status(job).await.0, None);
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn two_simultaneous_submissions_with_five_recent_jobs_admit_one(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for _ in 0..5 {
        f.submit("hello").await;
    }
    let current = thread::get(&pool, &f.tenant, f.thread).await.unwrap();
    let other = thread::create(&pool, &f.tenant, f.space, current.team_id, "Other")
        .await
        .unwrap();
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM kats WHERE id=$1 FOR NO KEY UPDATE")
        .bind(f.kats[0].id)
        .execute(&mut *holder)
        .await
        .unwrap();
    let submissions = async {
        tokio::join!(
            f.submit("six"),
            katwork::pipeline::submit(
                &f.core,
                &f.tenant,
                other.id,
                katwork::chat::UserPost {
                    body: "seven".into(),
                    client_msg_id: None,
                }
            )
        )
    };
    tokio::pin!(submissions);
    tokio::select! {
        _ = &mut submissions => panic!("submissions bypassed the Kat lock"),
        () = wait_for_locks(&pool, "kats", 2) => {}
    }
    holder.commit().await.unwrap();
    let (a, b) = submissions.await;
    let statuses = [f.status(a.1).await, f.status(b.unwrap().job_id).await];
    assert_eq!(statuses.iter().filter(|s| s.0.is_none()).count(), 1);
    assert_eq!(
        statuses
            .iter()
            .filter(|s| s.1.as_deref() == Some("too_many_requests"))
            .count(),
        1
    );
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn user_message_is_announced_with_client_msg_id_and_job_id(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let mut subscriber = f.core.events.subscribe(f.thread);
    let response = body(f.post("hello", Some("client1")).await).await;
    assert_eq!(
        timeout(Duration::from_secs(2), subscriber.recv())
            .await
            .unwrap()
            .unwrap(),
        EventSeq(2)
    );
    let events = f.events().await;
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["name"], "katwork.message_stored");
    assert_eq!(events[0]["value"]["clientMsgId"], "client1");
    assert_eq!(events[0]["value"]["jobId"], response["job_id"]);
    assert_eq!(events[0]["value"]["author"]["kind"], "user");
    assert_eq!(events[1]["name"], "katwork.job_status");
    assert!(events[1]["value"]["status"].is_null());
}

#[path = "pipeline/cancel.rs"]
mod cancel;
#[path = "pipeline/execution.rs"]
mod execution;
#[path = "pipeline/history.rs"]
mod history;
#[path = "pipeline/quotas.rs"]
mod quotas;
#[path = "pipeline/transactions.rs"]
mod transactions;

#[sqlx::test(migrations = "../katwork/migrations")]
async fn workspace_pause_enqueues_with_notice_and_waits(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    sqlx::query("UPDATE workspaces SET paused=true WHERE id=$1")
        .bind(f.tenant.workspace_id())
        .execute(&pool)
        .await
        .unwrap();
    let (_, job) = f.submit("hello").await;
    assert_eq!(f.task(job).await.0, f.kats[0].id);
    assert_eq!(f.notices().await[0]["code"], "workspace_paused");
    assert!(
        lease::claim(
            &f.core,
            "worker",
            Duration::from_secs(60),
            &katwork::agent::Limits::default()
        )
        .await
        .unwrap()
        .is_none()
    );
    assert!(f.status(job).await.0.is_none());
}
#[sqlx::test(migrations = "../katwork/migrations")]
async fn ungranted_mention_names_the_kat_and_routes_to_lead(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let space = SpaceId::new();
    sqlx::query(
        "INSERT INTO spaces(id,workspace_id,name,allow_external) VALUES($1,$2,'Other',false)",
    )
    .bind(space)
    .bind(f.tenant.workspace_id())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO kat_spaces(workspace_id,kat_id,space_id) VALUES($1,$2,$3)")
        .bind(f.tenant.workspace_id())
        .bind(f.kats[2].id)
        .bind(space)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE kats SET default_space_id=$2 WHERE id=$1")
        .bind(f.kats[2].id)
        .bind(space)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM kat_spaces WHERE kat_id=$1 AND space_id=$2")
        .bind(f.kats[2].id)
        .bind(f.space)
        .execute(&pool)
        .await
        .unwrap();
    let (_, job) = f.submit("@Writer hello").await;
    assert_eq!(f.task(job).await.0, f.kats[0].id);
    assert_eq!(
        f.notices().await[0],
        json!({"code":"kat_not_in_space","kats":[{"id":f.kats[2].id,"name":"Writer"}]})
    );
}
#[path = "pipeline/contracts.rs"]
mod contracts;

#[sqlx::test(migrations = "../katwork/migrations")]
async fn mention_needs_space_names_english_and_thai_kats(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for (name, text) in [
        ("Writer", "@Writerplease help"),
        ("เลขา", "@เลขาช่วยสรุปประชุม"),
    ] {
        sqlx::query("UPDATE kats SET name=$2 WHERE id=$1")
            .bind(f.kats[2].id)
            .bind(name)
            .execute(&pool)
            .await
            .unwrap();
        let (_, job) = f.submit(text).await;
        let (kat, input) = f.task(job).await;
        assert_eq!(kat, f.kats[0].id);
        assert_eq!(input.routing.rule, katwork::jobs::RouteRule::Lead);
        let notice = f.notices().await.pop().expect("missing delimiter notice");
        assert_eq!(notice["code"], "mention_needs_space");
        assert_eq!(notice["kats"], json!([{"id":f.kats[2].id,"name":name}]));
    }
}

#[path = "pipeline/safety.rs"]
mod safety;

#[sqlx::test(migrations = "../katwork/migrations")]
async fn mention_space_warning_respects_parser_and_follow_up_routing(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    for text in [
        "`@Writerplease`",
        "https://example.test/@Writerplease",
        "@Writerplease @Researcher yes",
    ] {
        f.submit(text).await;
        assert!(
            !f.notices()
                .await
                .iter()
                .any(|n| n["code"] == "mention_needs_space")
        );
    }
    f.awaiting().await;
    let (_, job) = f.submit("@Writerplease @Writerplease").await;
    let (kat, input) = f.task(job).await;
    assert_eq!(kat, f.kats[2].id);
    assert_eq!(input.routing.rule, katwork::jobs::RouteRule::FollowUp);
    assert_eq!(
        f.notices()
            .await
            .iter()
            .filter(|n| n["code"] == "mention_needs_space")
            .count(),
        1
    );
}

#[path = "pipeline/followups.rs"]
mod followups;
