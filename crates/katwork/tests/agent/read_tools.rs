#![allow(
    clippy::disallowed_methods,
    reason = "Test fixtures drain SQL; cancellation probes deliberately drop futures"
)]
//! The loop with the read tools (SP2a Task 6): effects, retries, document errors, the cache,
//! output trust, the repeat key, the run deadline and the shared budget.
use super::*;
use katwork::{
    agent::ledger,
    artifacts::{
        BlobRef, BlobStore, BlobStream, MediaType, StagedBlob, Staging,
        store::{self, OutputCtx},
        tools::{ListFiles, ReadFile},
    },
    db,
    docs::{DocError, DocLimits, Extracted, ExtractorId},
    policy::{self, NextStep, PolicyInputs, Thresholds},
};
use sqlx::Connection;
use support::files::{self, TestDir};
use uuid::Uuid;

/// The harness with `read_file` (over `extract`) and `list_files` registered and allowed.
struct Files {
    dir: TestDir,
    core: katwork::runtime::Core,
}

async fn with_files(h: &mut Harness, extract: Option<ExtractFn>, limits: DocLimits) -> Files {
    let dir = TestDir::new();
    let core = files::core(&h.pool, &dir, limits);
    let deps = match extract {
        Some(extract) => files::deps(&core, extract),
        None => ToolDeps::from_core(&core),
    };
    allow(h, &["read_file", "list_files"]).await;
    h.tools.register(Arc::new(ReadFile::new(deps))).unwrap();
    h.tools.register(Arc::new(ListFiles)).unwrap();
    Files { dir, core }
}

async fn allow(h: &mut Harness, names: &[&str]) {
    h.kat
        .tool_allowlist
        .extend(names.iter().map(|n| n.to_string()));
    sqlx::query("UPDATE kats SET tool_allowlist=$2 WHERE id=$1")
        .bind(h.kat.id)
        .bind(&h.kat.tool_allowlist)
        .execute(&h.pool)
        .await
        .unwrap();
}

impl Files {
    async fn upload(&self, h: &Harness, name: &str, bytes: &[u8]) -> ArtifactId {
        files::upload(
            &h.pool,
            &*self.core.blobs,
            &h.input.tenant,
            h.input.space_id,
            name,
            MediaType::Text,
            bytes,
        )
        .await
        .id
    }

    async fn output(&self, h: &Harness, name: &str, bytes: &[u8]) -> ArtifactId {
        let staged = self
            .core
            .blobs
            .stage_bytes(h.input.tenant.workspace_id(), bytes)
            .await
            .unwrap();
        let ctx = output_ctx(h);
        let blobs = self.core.blobs.clone();
        db::guarded(&h.pool, async |conn| {
            let mut tx = conn.begin().await?;
            let blob = store::commit_blob_on(&mut tx, &*blobs, staged).await?;
            let v = store::create_output_on(
                &mut tx,
                &ctx,
                name,
                MediaType::Text,
                &blob,
                None,
                &Uuid::now_v7().to_string(),
            )
            .await?;
            tx.commit().await?;
            Ok(v.artifact_id)
        })
        .await
        .unwrap()
    }
}

fn output_ctx(h: &Harness) -> OutputCtx {
    OutputCtx {
        tenant: h.input.tenant.clone(),
        space_id: h.input.space_id,
        kat_id: h.kat.id,
        job_id: h.input.job_id,
        run_id: RunId(h.f.run),
    }
}

async fn new_version(
    pool: &PgPool,
    blobs: Arc<dyn BlobStore>,
    ctx: OutputCtx,
    artifact: ArtifactId,
    bytes: &[u8],
) {
    let staged = blobs
        .stage_bytes(ctx.tenant.workspace_id(), bytes)
        .await
        .unwrap();
    db::guarded(pool, async |conn| {
        let mut tx = conn.begin().await?;
        store::lock_artifacts_on(&mut tx, &ctx.tenant, Some(artifact), &[]).await?;
        let blob = store::commit_blob_on(&mut tx, &*blobs, staged).await?;
        store::new_output_version_on(
            &mut tx,
            &ctx,
            artifact,
            MediaType::Text,
            &blob,
            None,
            &Uuid::now_v7().to_string(),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    })
    .await
    .unwrap();
}

/// Replaces the model's script, keeping the harness's entry.
fn script(h: &mut Harness, turns: Vec<FakeTurn>) {
    h.fake = FakeProvider::scripted(turns);
    h.models = Registry::from_entries(vec![(entry(&h.fake), Arc::new(h.fake.clone()))]);
}

fn read(id: &str, file: ArtifactId) -> ToolCall {
    call(id, "read_file", json!({"file_id": file.0}))
}

fn one_block() -> Extracted {
    files::extracted(ExtractorId::TEXT, vec![files::text_block(1, "ก")])
}

#[sqlx::test]
async fn workspace_read_does_not_mark_effects_started(pool: PgPool) {
    let mut h = Harness::new(pool, vec![]).await;
    let files = with_files(&mut h, None, DocLimits::default()).await;
    let file = files.upload(&h, "a.txt", "ข้อความ".as_bytes()).await;
    script(
        &mut h,
        vec![turn("", vec![read("a", file)]), turn("ok", vec![])],
    );
    let facts = h.run().await.unwrap();
    assert_eq!(facts.final_text.as_deref(), Some("ok"));
    assert_eq!(facts.tool_calls[0].state, ToolCallState::Succeeded);
    assert!(!h.effects().await);
}

/// A blob store whose first read never returns.
struct HangOnce {
    inner: Arc<dyn BlobStore>,
    reads: AtomicUsize,
}
#[async_trait]
impl BlobStore for HangOnce {
    async fn stage(&self, workspace: WorkspaceId, max_bytes: u64) -> Result<Staging, KatError> {
        self.inner.stage(workspace, max_bytes).await
    }
    async fn commit(&self, staged: StagedBlob) -> Result<BlobRef, KatError> {
        self.inner.commit(staged).await
    }
    async fn read(&self, blob: &BlobRef, max_bytes: u64) -> Result<Vec<u8>, KatError> {
        if self.reads.fetch_add(1, Ordering::SeqCst) == 0 {
            futures::future::pending::<()>().await;
        }
        self.inner.read(blob, max_bytes).await
    }
    async fn open(&self, blob: &BlobRef) -> Result<BlobStream, KatError> {
        self.inner.open(blob).await
    }
    async fn remove(&self, blob: &BlobRef) -> Result<(), KatError> {
        self.inner.remove(blob).await
    }
    async fn free_bytes(&self) -> Result<u64, KatError> {
        self.inner.free_bytes().await
    }
}

#[sqlx::test]
async fn workspace_read_timeout_retries_once_like_read(pool: PgPool) {
    let mut h = Harness::new(pool, vec![]).await;
    let dir = TestDir::new();
    let core = files::core(&h.pool, &dir, DocLimits::default());
    let file = files::upload(
        &h.pool,
        &*core.blobs,
        &h.input.tenant,
        h.input.space_id,
        "a.txt",
        MediaType::Text,
        "ก".as_bytes(),
    )
    .await
    .id;
    let hang = Arc::new(HangOnce {
        inner: core.blobs.clone(),
        reads: AtomicUsize::new(0),
    });
    let deps = ToolDeps {
        blobs: hang.clone(),
        ..ToolDeps::from_core(&core)
    };
    allow(&mut h, &["read_file"]).await;
    h.tools.register(Arc::new(ReadFile::new(deps))).unwrap();
    script(
        &mut h,
        vec![turn("", vec![read("a", file)]), turn("ok", vec![])],
    );
    // Real time: the pool's call timer runs on the std clock, which a paused tokio clock would
    // leave behind. The loop's own timeout fires on the hung blob read.
    h.limits.tool_timeout = Duration::from_millis(300);
    let facts = h.run().await.unwrap();
    assert_eq!(facts.final_text.as_deref(), Some("ok"));
    assert_eq!(hang.reads.load(Ordering::SeqCst), 2);
    let rows = h.rows().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<i32, _>("attempts"), 2);
    assert_eq!(
        rows[0].get::<String, _>("state"),
        "succeeded",
        "{:?}",
        rows[0].get::<Option<String>, _>("result")
    );
    assert!(!tool_results(&h.fake)[0].3);
}

async fn doc_error_case(pool: PgPool, error: DocError, code: &str) {
    let mut h = Harness::new(pool, vec![]).await;
    let (extract, calls) = files::counting(move || Err(error));
    let files = with_files(&mut h, Some(extract), DocLimits::default()).await;
    let file = files.upload(&h, "a.txt", b"x").await;
    script(
        &mut h,
        vec![
            turn("", vec![read("a", file)]),
            turn("", vec![read("b", file)]),
            turn("ok", vec![]),
        ],
    );
    let facts = h.run().await.unwrap();
    assert_eq!(facts.final_text.as_deref(), Some("ok"));
    assert_eq!(
        files::calls(&calls),
        1,
        "the second call came from the cache"
    );
    let results = tool_results(&h.fake);
    assert_eq!(results.len(), 2);
    for result in &results {
        assert_eq!((result.2.as_str(), result.3), (code, true));
    }
    for row in h.rows().await {
        assert_eq!(row.get::<i32, _>("attempts"), 1, "not retried");
        assert_eq!(row.get::<String, _>("state"), "failed");
        assert_eq!(row.get::<String, _>("result"), code);
    }
}

#[sqlx::test]
async fn doc_errors_are_not_retried_and_are_cached(pool: PgPool) {
    doc_error_case(pool.clone(), DocError::CallTimeout, "doc_timeout").await;
}

#[sqlx::test]
async fn file_too_complex_is_not_retried_and_is_cached(pool: PgPool) {
    doc_error_case(
        pool,
        DocError::TooComplex("zip_entry_ratio"),
        "file_too_complex",
    )
    .await;
}

#[sqlx::test]
async fn short_caller_deadline_timeout_is_not_cached(pool: PgPool) {
    let mut h = Harness::new(pool, vec![]).await;
    let (release, released) = std::sync::mpsc::channel::<()>();
    let released = Arc::new(Mutex::new(released));
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let extract: ExtractFn = Arc::new(move |_, _, _| {
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            let _ = released.lock().unwrap().recv();
        }
        Ok(one_block())
    });
    let limits = DocLimits {
        call_timeout: Duration::from_secs(30),
        ..DocLimits::default()
    };
    let files = with_files(&mut h, Some(extract), limits).await;
    let file = files.upload(&h, "a.txt", b"x").await;
    script(
        &mut h,
        vec![turn("", vec![read("a", file)]), turn("late", vec![])],
    );
    h.input.deadline = tokio::time::Instant::now() + Duration::from_millis(200);
    let facts = h.run().await.unwrap();
    assert_eq!(facts.tool_calls[0].state, ToolCallState::Failed);
    let row = &h.rows().await[0];
    assert_eq!(row.get::<String, _>("result"), "doc_timeout");
    release.send(()).unwrap();
    // A new run of the same file reads it again.
    h.input.run_id = RunId::new();
    h.input.deadline = tokio::time::Instant::now() + Duration::from_secs(600);
    script(
        &mut h,
        vec![turn("", vec![read("b", file)]), turn("ok", vec![])],
    );
    let facts = h.run().await.unwrap();
    assert_eq!(facts.final_text.as_deref(), Some("ok"));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let page = &tool_results(&h.fake)[0].2;
    assert!(page.contains("\"text\":\"ก\""), "{page}");
}

#[sqlx::test]
async fn list_files_pages_under_the_result_cap(pool: PgPool) {
    let mut h = Harness::new(pool.clone(), vec![]).await;
    let files = with_files(&mut h, None, DocLimits::default()).await;
    let ids: Vec<ArtifactId> = {
        let mut ids = vec![];
        for n in 0..200 {
            ids.push(
                files
                    .upload(&h, &format!("{n:03}-{}", "x".repeat(196)), b"x")
                    .await,
            );
        }
        ids
    };
    let ctx = files::tool_ctx(&pool, &h.f, &h.input.tenant);
    let list_all = || async {
        let mut pages = vec![];
        let mut after: Option<String> = None;
        loop {
            let args = after.as_ref().map_or(json!({}), |a| json!({"after": a}));
            let ToolOutput::Text(text) = ListFiles.call(&ctx, args).await.unwrap() else {
                unreachable!()
            };
            let next =
                serde_json::from_str::<Value>(text.split('\n').next().unwrap()).unwrap()["next"]
                    .as_str()
                    .map(str::to_owned);
            pages.push(text);
            match next {
                Some(n) => after = Some(n),
                None => return pages,
            }
        }
    };
    let first = list_all().await;
    let per_page = first[0].split('\n').count() - 1;
    // The first file of page 2 takes the name of the last of page 1.
    let last_name: String = sqlx::query_scalar("SELECT name FROM artifacts WHERE id=$1")
        .bind(ids[per_page - 1])
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE artifacts SET name=$2 WHERE id=$1")
        .bind(ids[per_page])
        .bind(&last_name)
        .execute(&pool)
        .await
        .unwrap();
    let pages = list_all().await;
    let mut seen = vec![];
    for page in &pages {
        assert!(page.chars().count() < 7000);
        let lines: Vec<Value> = page
            .split('\n')
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        if let Some(next) = lines[0]["next"].as_str() {
            assert!(next.is_ascii());
        }
        seen.extend(lines[1..].iter().map(|l| {
            (
                l["name"].as_str().unwrap().to_owned(),
                l["file_id"].as_str().unwrap().to_owned(),
            )
        }));
    }
    assert!(lines_of(&pages[pages.len() - 1])[0]["next"].is_null());
    assert_eq!(seen.len(), 200);
    assert_eq!(
        seen.iter()
            .map(|s| &s.1)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        200
    );
    let page_one = lines_of(&pages[0]);
    let page_two = lines_of(&pages[1]);
    assert_eq!(page_one.last().unwrap()["name"], json!(last_name));
    assert_eq!(
        page_two[1]["name"],
        json!(last_name),
        "the shared name falls on both sides"
    );
    // A cursor that does not decode is invalid_args, before any read: this context's pool
    // cannot reach a server.
    let offline = ToolCtx {
        pool: sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://127.0.0.1:1/none")
            .unwrap(),
        ..ctx.clone()
    };
    let x = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, b"\"x\"");
    for bad in ["%%%".to_owned(), x] {
        let error = ListFiles
            .call(&offline, json!({"after": bad}))
            .await
            .unwrap_err();
        assert_eq!(error.code(), "invalid_args", "{bad}");
    }
}

fn lines_of(text: &str) -> Vec<Value> {
    text.split('\n')
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[sqlx::test]
async fn policy_retries_after_only_workspace_reads(pool: PgPool) {
    let mut h = Harness::new(pool, vec![]).await;
    let files = with_files(
        &mut h,
        Some(files::counting(|| Ok(one_block())).0),
        DocLimits::default(),
    )
    .await;
    let file = files.upload(&h, "a.txt", b"x").await;
    script(
        &mut h,
        vec![
            turn("", vec![read("a", file)]),
            FakeTurn::Error(ModelError::ProviderUnavailable),
            FakeTurn::Error(ModelError::ProviderUnavailable),
        ],
    );
    let facts = h.run().await.unwrap();
    assert_eq!(facts.stop, Some(StopReason::ProviderUnavailable));
    let verdict = policy::decide_job(&facts, &Thresholds::SP1);
    assert_eq!(verdict.next, Some(NextStep::Retry));
}

/// A sink that writes a new version of `artifact` after the `after`-th tool result.
struct VersionSink<'a> {
    h: &'a Harness,
    files: &'a Files,
    artifact: ArtifactId,
    after: usize,
    results: AtomicUsize,
}
#[async_trait]
impl EventSink for VersionSink<'_> {
    async fn emit(&self, event: AgUiEvent) -> Result<EventSeq, KatError> {
        let result = matches!(event, AgUiEvent::ToolCallResult { .. });
        let seq = self.h.sink.emit(event).await?;
        if result && self.results.fetch_add(1, Ordering::SeqCst) + 1 == self.after {
            new_version(
                &self.h.pool,
                self.files.core.blobs.clone(),
                output_ctx(self.h),
                self.artifact,
                "v2".as_bytes(),
            )
            .await;
        }
        Ok(seq)
    }
    async fn close_run(&self, end: RunEnd, code: &str) -> Result<EventSeq, KatError> {
        self.h.sink.close_run(end, code).await
    }
}

async fn reread_case(pool: PgPool, after: usize) -> RunFacts {
    let mut h = Harness::new(pool, vec![]).await;
    let files = with_files(&mut h, None, DocLimits::default()).await;
    let file = files.output(&h, "out.txt", "v1".as_bytes()).await;
    let mut turns: Vec<FakeTurn> = ["a", "b", "c"]
        .iter()
        .map(|id| turn("", vec![read(id, file)]))
        .collect();
    turns.push(turn("ok", vec![]));
    script(&mut h, turns);
    run::execute(
        &RunEnv {
            lock_timeout: h.lock_timeout,
            pool: &h.pool,
            models: &h.models,
            tools: &h.tools,
            sink: &VersionSink {
                h: &h,
                files: &files,
                artifact: file,
                after,
                results: AtomicUsize::new(0),
            },
        },
        &h.kat,
        h.input.clone(),
        &h.limits,
    )
    .await
    .unwrap()
}

#[sqlx::test]
async fn reread_after_new_version_is_not_a_loop(pool: PgPool) {
    let facts = reread_case(pool.clone(), 2).await;
    assert_eq!(facts.stop, None);
    assert_eq!(facts.final_text.as_deref(), Some("ok"));
    // The control: with no new version, the third identical read is a loop.
    let facts = reread_case(pool, usize::MAX).await;
    assert_eq!(facts.stop, Some(StopReason::LoopDetected));
}

#[sqlx::test]
async fn tool_calls_record_output_trust(pool: PgPool) {
    let mut h = Harness::new(pool, vec![]).await;
    let files = with_files(
        &mut h,
        Some(files::counting(|| Ok(one_block())).0),
        DocLimits::default(),
    )
    .await;
    let file = files.upload(&h, "a.txt", b"x").await;
    script(
        &mut h,
        vec![
            turn("", vec![read("a", file)]),
            turn(
                "",
                vec![call(
                    "b",
                    "ask_user",
                    json!({"question":"ok?","options":["yes"]}),
                )],
            ),
        ],
    );
    h.run().await.unwrap();
    let trust: Vec<(String, String)> = h
        .rows()
        .await
        .iter()
        .map(|r| (r.get("tool_name"), r.get("output_trust")))
        .collect();
    assert_eq!(
        trust,
        [
            ("read_file".into(), "untrusted".into()),
            ("ask_user".into(), "trusted".into())
        ]
    );
}

/// The run's records rebuilt from its `tool_calls` rows.
async fn records(h: &Harness) -> Vec<ToolCallRecord> {
    h.rows()
        .await
        .iter()
        .map(|r| ToolCallRecord {
            id: r.get("id"),
            step: r.get::<i32, _>("step") as u32,
            tool_call_id: r.get("tool_call_id"),
            name: r.get("tool_name"),
            side_effect: r
                .get::<Option<String>, _>("side_effect")
                .and_then(|s| SideEffect::from_wire(&s)),
            state: serde_json::from_value(json!(r.get::<String, _>("state"))).unwrap(),
            output_trust: serde_json::from_value(json!(r.get::<String, _>("output_trust")))
                .unwrap(),
        })
        .collect()
}

#[sqlx::test]
async fn context_untrusted_replays_from_the_log(pool: PgPool) {
    for (read_a_file, untrusted) in [(true, true), (false, false)] {
        let mut h = Harness::new(pool.clone(), vec![]).await;
        let files = with_files(
            &mut h,
            Some(files::counting(|| Ok(one_block())).0),
            DocLimits::default(),
        )
        .await;
        let file = files.upload(&h, "a.txt", b"x").await;
        if read_a_file {
            script(
                &mut h,
                vec![turn("", vec![read("a", file)]), turn("done", vec![])],
            );
        } else {
            script(
                &mut h,
                vec![turn(
                    "",
                    vec![call(
                        "b",
                        "ask_user",
                        json!({"question":"ok?","options":["yes"]}),
                    )],
                )],
            );
        }
        let mut facts = h.run().await.unwrap();
        assert_eq!(
            PolicyInputs::from_facts(&facts).context_untrusted,
            untrusted
        );
        facts.tool_calls = records(&h).await;
        assert_eq!(
            PolicyInputs::from_facts(&facts).context_untrusted,
            untrusted,
            "replayed"
        );
    }
}

/// A probe that records when it ran and the deadline it was given.
fn deadline_probe(
    seen: Arc<Mutex<Vec<(tokio::time::Instant, tokio::time::Instant)>>>,
    hang_first: bool,
) -> Probe {
    Probe::new("probe", SideEffect::WorkspaceRead).action(move |ctx, _| {
        let seen = seen.clone();
        async move {
            let first = {
                let mut seen = seen.lock().unwrap();
                seen.push((tokio::time::Instant::now(), ctx.deadline));
                seen.len() == 1
            };
            if hang_first && first {
                futures::future::pending::<()>().await;
            }
            Ok(ToolOutput::Text("ok".into()))
        }
        .boxed()
    })
}

#[sqlx::test]
async fn tool_ctx_deadline_is_the_dispatch_deadline(pool: PgPool) {
    let seen = Arc::new(Mutex::new(vec![]));
    let mut h = Harness::new(
        pool.clone(),
        vec![
            turn("", vec![call("a", "probe", json!({}))]),
            turn("ok", vec![]),
        ],
    )
    .await;
    h.add(deadline_probe(seen.clone(), false));
    h.limits.tool_timeout = Duration::from_secs(60);
    h.input.deadline = tokio::time::Instant::now() + Duration::from_secs(600);
    h.run().await.unwrap();
    let (entry, deadline) = seen.lock().unwrap()[0];
    assert!(deadline > entry && deadline <= entry + Duration::from_secs(60));
    let seen = Arc::new(Mutex::new(vec![]));
    let mut h = Harness::new(
        pool,
        vec![
            turn("", vec![call("a", "probe", json!({}))]),
            turn("ok", vec![]),
        ],
    )
    .await;
    h.add(deadline_probe(seen.clone(), false));
    h.input.deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    h.run().await.unwrap();
    assert_eq!(seen.lock().unwrap()[0].1, h.input.deadline);
}

#[sqlx::test]
async fn ctx_deadline_is_recomputed_for_the_retry(pool: PgPool) {
    let seen = Arc::new(Mutex::new(vec![]));
    let mut h = Harness::new(
        pool,
        vec![
            turn("", vec![call("a", "probe", json!({}))]),
            turn("ok", vec![]),
        ],
    )
    .await;
    h.add(deadline_probe(seen.clone(), true));
    let mut e = entry(&h.fake);
    e.timeouts = ModelTimeouts::LOCAL;
    h.models = Registry::from_entries(vec![(e, Arc::new(h.fake.clone()))]);
    let _clock = Clock::pause();
    let progress = async {
        until(|| seen.lock().unwrap().len() == 1).await;
        tokio::time::advance(Duration::from_millis(60_001)).await;
    };
    let (facts, ()) = tokio::join!(h.run(), progress);
    assert_eq!(facts.unwrap().final_text.as_deref(), Some("ok"));
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert!(
        seen[1].1 > seen[0].1,
        "the retry has its own, later deadline"
    );
    assert!(seen[1].1 > seen[1].0);
}

#[sqlx::test]
async fn run_budget_is_shared_with_tools(pool: PgPool) {
    let charge = |tokens: u64| {
        Probe::new("probe", SideEffect::Read).action(move |ctx, _| {
            async move {
                let before = ctx.budget.remaining();
                ctx.budget.charge(tokens);
                Ok(ToolOutput::Text(format!(
                    "{}",
                    before - ctx.budget.remaining()
                )))
            }
            .boxed()
        })
    };
    let mut h = Harness::new(
        pool.clone(),
        vec![
            turn("", vec![call("a", "probe", json!({}))]),
            turn("ok", vec![]),
        ],
    )
    .await;
    h.add(charge(1_000));
    let facts = h.run().await.unwrap();
    assert_eq!(tool_results(&h.fake)[0].2, "1000");
    // Two model calls of 20 tokens each, and the tool's 1,000.
    assert_eq!(facts.budget.charged(), 1_040);
    let mut h = Harness::new(
        pool,
        vec![
            turn("", vec![call("a", "probe", json!({}))]),
            turn("never", vec![]),
        ],
    )
    .await;
    h.add(charge(200_000));
    let facts = h.run().await.unwrap();
    assert_eq!(facts.stop, Some(StopReason::TokenBudget));
    assert_eq!(h.fake.received().len(), 1);
}

#[sqlx::test]
async fn a_coded_tool_error_reaches_the_model_and_the_ledger(pool: PgPool) {
    let mut h = Harness::new(
        pool,
        vec![
            turn("", vec![call("a", "probe", json!({}))]),
            turn("ok", vec![]),
        ],
    )
    .await;
    let n = h.add(
        Probe::new("probe", SideEffect::WorkspaceRead)
            .action(|_, _| async { Err(ToolError::Coded("not_found")) }.boxed()),
    );
    h.run().await.unwrap();
    assert_eq!(n.load(Ordering::SeqCst), 1, "not retried");
    assert_eq!(tool_results(&h.fake)[0].2, "not_found");
    let row = &h.rows().await[0];
    assert_eq!(row.get::<String, _>("state"), "failed");
    assert_eq!(row.get::<String, _>("result"), "not_found");
}

/// A tool whose `target_revision` never answers.
struct SlowRevision {
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl Tool for SlowRevision {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "probe".into(),
            description: "Test tool".into(),
            parameters: json!({"type":"object"}),
        }
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::WorkspaceRead
    }
    async fn target_revision(&self, _: &ToolCtx, _: &Value) -> Option<String> {
        futures::future::pending().await
    }
    async fn call(&self, _: &ToolCtx, _: Value) -> Result<ToolOutput, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::Text("ran".into()))
    }
}

#[sqlx::test]
async fn a_slow_or_failing_target_revision_never_blocks_the_loop(pool: PgPool) {
    let mut h = Harness::new(
        pool,
        vec![
            turn("", vec![call("a", "probe", json!({}))]),
            turn("ok", vec![]),
        ],
    )
    .await;
    let calls = Arc::new(AtomicUsize::new(0));
    h.tools
        .register(Arc::new(SlowRevision {
            calls: calls.clone(),
        }))
        .unwrap();
    // The revision is bounded by the dispatch deadline, in real time.
    h.limits.tool_timeout = Duration::from_millis(300);
    let facts = h.run().await;
    assert_eq!(facts.unwrap().final_text.as_deref(), Some("ok"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[sqlx::test]
async fn changes_state_and_the_sql_lists_agree_for_all_13_effects(pool: PgPool) {
    let h = Harness::new(pool, vec![]).await;
    sqlx::query("INSERT INTO runs(id,workspace_id,job_id,task_id,kat_id,kind,lease_epoch,kat_config_hash,state) VALUES($1,$2,$3,$4,$5,'initial',1,'h','running')")
        .bind(h.input.run_id).bind(h.input.tenant.workspace_id()).bind(h.input.job_id).bind(h.input.task_id).bind(h.kat.id)
        .execute(&h.pool).await.unwrap();
    let fence = Fence {
        task_id: h.input.task_id,
        epoch: h.input.lease_epoch,
    };
    for (step, wire) in [
        "read",
        "navigate",
        "workspace_read",
        "draft",
        "workspace_write",
        "exec",
        "credential_use",
        "destructive",
        "outward_send",
        "outward_publish",
        "outward_pay",
        "outward_share",
        "outward_external_api",
    ]
    .into_iter()
    .enumerate()
    {
        sqlx::query("UPDATE tasks SET effects_started_at=NULL WHERE id=$1")
            .bind(h.input.task_id)
            .execute(&h.pool)
            .await
            .unwrap();
        let effect = SideEffect::from_wire(wire).unwrap();
        let mut ctx = files::tool_ctx(&h.pool, &h.f, &h.input.tenant);
        ctx.run_id = h.input.run_id;
        ctx.lease_epoch = h.input.lease_epoch;
        ctx.step = step as u32;
        ctx.policy_version = h.input.policy_version;
        let id =
            ledger::start_tool_call(&h.pool, &ctx, "probe", effect, OutputTrust::Untrusted, "{}")
                .await
                .unwrap();
        assert_eq!(h.effects().await, effect.changes_state(), "{wire}: start");
        ledger::end_tool_call(
            &h.pool,
            &fence,
            id,
            ToolCallState::TimedOut,
            Some("timeout"),
            h.lock_timeout,
        )
        .await
        .unwrap();
        let retried = ledger::retry_read(&h.pool, &fence, id, h.lock_timeout).await;
        assert_eq!(
            retried.is_ok(),
            !effect.changes_state(),
            "{wire}: retry_read"
        );
    }
}
