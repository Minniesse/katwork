#![allow(
    clippy::disallowed_methods,
    reason = "Test fixtures drain SQL; cancellation probes deliberately drop futures"
)]
#[allow(dead_code)]
mod support;

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::ConnectInfo,
    http::Request,
    response::Response,
};
use katwork::{
    artifacts::{
        BlobRef, BlobStore, BlobStream, LocalBlobStore, StagedBlob, Staging,
        store::{self, test_hooks::Gate},
    },
    auth,
    config::{Config, TransportConfig},
    error::KatError,
    ids::{SpaceId, UserId, WorkspaceId},
    runtime::Core,
    tenancy::workspace,
};
use katwork_server::app::{AppState, build_router};
use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{Connection, PgConnection, PgPool, postgres::PgPoolOptions};
use support::{assert_code, assert_pool_clean, body, cancel_sql};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    sync::{Notify, Semaphore},
};
use tower::ServiceExt;
use uuid::Uuid;

const LOCK: Duration = Duration::from_secs(5);
const MIB: u64 = 1_048_576;
const DEFAULT_CSP: &str = "default-src 'self'; frame-ancestors 'none'";

/// A directory under the system temp dir, removed when dropped.
struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("katwork-files-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What the test store's `commit` does.
enum OnCommit {
    /// Commits, as the local store does.
    Commit,
    /// Signals the gate after the real rename and waits for its release, so a test can act
    /// while the blob is in place and its transaction has not committed.
    PauseAfterRename(Gate),
    /// Panics before any file I/O, as a bug in a store would.
    Panic,
}

/// The local store, with a fixed answer for free space so no test depends on the disk.
struct FreeBytes {
    inner: LocalBlobStore,
    free: u64,
    on_commit: OnCommit,
}

#[async_trait::async_trait]
impl BlobStore for FreeBytes {
    async fn stage(&self, workspace: WorkspaceId, max_bytes: u64) -> Result<Staging, KatError> {
        self.inner.stage(workspace, max_bytes).await
    }

    async fn commit(&self, staged: StagedBlob) -> Result<BlobRef, KatError> {
        match &self.on_commit {
            OnCommit::Commit => self.inner.commit(staged).await,
            OnCommit::PauseAfterRename(gate) => {
                let blob = self.inner.commit(staged).await?;
                gate.reached.notify_one();
                gate.release.notified().await;
                Ok(blob)
            }
            OnCommit::Panic => panic!("the test store's commit panics"),
        }
    }

    async fn read(&self, blob: &BlobRef, max_bytes: u64) -> Result<Vec<u8>, KatError> {
        self.inner.read(blob, max_bytes).await
    }

    async fn open(&self, blob: &BlobRef) -> Result<BlobStream, KatError> {
        self.inner.open(blob).await
    }

    async fn remove(&self, blob: &BlobRef) -> Result<(), KatError> {
        self.inner.remove(blob).await
    }

    async fn free_bytes(&self) -> Result<u64, KatError> {
        Ok(self.free)
    }
}

struct Env {
    core: Arc<Core>,
    app: Router,
    dir: TestDir,
    workspace: WorkspaceId,
    space: SpaceId,
    user: UserId,
    cookie: String,
}

impl Env {
    async fn new(pool: &PgPool, tweak: impl FnOnce(&mut Config)) -> Self {
        Self::with_free(pool, tweak, u64::MAX / 4).await
    }

    async fn with_free(pool: &PgPool, tweak: impl FnOnce(&mut Config), free: u64) -> Self {
        Self::build(pool, pool.clone(), tweak, free, OnCommit::Commit).await
    }

    /// The app whose store does `on_commit` in each commit.
    async fn committing(pool: &PgPool, on_commit: OnCommit) -> Self {
        Self::build(pool, pool.clone(), |_| {}, u64::MAX / 4, on_commit).await
    }

    /// The app on a pool of one connection, which `drop_during_guarded_statement` needs.
    async fn single(pool: &PgPool) -> Self {
        let single = PgPoolOptions::new()
            .max_connections(1)
            .connect_with((*pool.connect_options()).clone())
            .await
            .unwrap();
        Self::build(pool, single, |_| {}, u64::MAX / 4, OnCommit::Commit).await
    }

    async fn build(
        pool: &PgPool,
        app_pool: PgPool,
        tweak: impl FnOnce(&mut Config),
        free: u64,
        on_commit: OnCommit,
    ) -> Self {
        let (user, cookie) = person(pool).await;
        let workspace = workspace::create(pool, "Files", user, LOCK).await.unwrap();
        let space = SpaceId::new();
        sqlx::query(
            "INSERT INTO spaces (id, workspace_id, name, allow_external) VALUES ($1, $2, 'Docs', false)",
        )
        .bind(space)
        .bind(workspace)
        .execute(pool)
        .await
        .unwrap();
        let dir = TestDir::new();
        let mut config = support::config(None);
        tweak(&mut config);
        let store = FreeBytes {
            inner: LocalBlobStore::new(dir.0.join("artifacts")).unwrap(),
            free,
            on_commit,
        };
        let core = Arc::new(Core::new(app_pool, Arc::new(config)).with_blob_store(Arc::new(store)));
        let app = build_router(AppState { core: core.clone() });
        Self {
            core,
            app,
            dir,
            workspace,
            space,
            user,
            cookie,
        }
    }

    fn root(&self) -> PathBuf {
        self.dir.0.join("artifacts")
    }

    fn files_uri(&self) -> String {
        format!(
            "/api/workspaces/{}/spaces/{}/files",
            self.workspace, self.space
        )
    }

    fn upload_uri(&self, name: &str) -> String {
        format!("{}?name={}", self.files_uri(), encode(name))
    }

    fn content_uri(&self, file: &Value, version: &str) -> String {
        format!(
            "/api/workspaces/{}/files/{}/versions/{version}/content",
            self.workspace,
            file["id"].as_str().unwrap()
        )
    }

    fn tmp_files(&self) -> Vec<PathBuf> {
        files_under(&self.root().join(self.workspace.to_string()).join("tmp"))
    }

    fn blob_files(&self) -> Vec<PathBuf> {
        files_under(&self.root().join(self.workspace.to_string()).join("sha256"))
    }

    fn blob_path(&self, sha256: &str) -> PathBuf {
        self.root()
            .join(self.workspace.to_string())
            .join("sha256")
            .join(&sha256[0..2])
            .join(&sha256[2..4])
            .join(sha256)
    }

    async fn rows(&self) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM artifacts WHERE workspace_id = $1")
            .bind(self.workspace)
            .fetch_one(&self.core.pool)
            .await
            .unwrap()
    }

    /// Nothing of an upload was kept: no row, no blob, no temp file.
    async fn assert_nothing_stored(&self) {
        assert_eq!(self.rows().await, 0);
        assert!(self.blob_files().is_empty());
        wait_for(|| self.tmp_files().is_empty()).await;
    }

    /// Both of this person's upload slots are free.
    fn assert_slots_free(&self) {
        let first = self.core.uploads.try_acquire(self.user).unwrap();
        let second = self.core.uploads.try_acquire(self.user).unwrap();
        drop((first, second));
    }

    async fn upload(&self, name: &str, bytes: &[u8]) -> Response {
        send(
            &self.app,
            post(
                &self.upload_uri(name),
                &self.cookie,
                Some("application/octet-stream"),
                Some(bytes.len() as u64),
                Body::from(bytes.to_vec()),
            ),
        )
        .await
    }

    async fn uploaded(&self, name: &str, bytes: &[u8]) -> Value {
        let response = self.upload(name, bytes).await;
        assert_eq!(response.status(), 201, "{name}");
        body(response).await
    }

    async fn get(&self, uri: &str) -> Response {
        send(&self.app, get(uri, &self.cookie)).await
    }
}

fn encode(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else {
            found.push(path);
        }
    }
    found
}

/// Polls `ready` until it holds; the deadline only bounds a hang.
async fn wait_for(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("state never reached");
}

async fn person(pool: &PgPool) -> (UserId, String) {
    let user = UserId::new();
    sqlx::query(
        "INSERT INTO users (id, email, password_hash, display_name, locale) VALUES ($1, $2, $3, 'Person', 'en')",
    )
    .bind(user)
    .bind(format!("{user}@example.test"))
    .bind(Uuid::now_v7().to_string())
    .execute(pool)
    .await
    .unwrap();
    let session = auth::session::create(pool, user, LOCK).await.unwrap();
    (user, format!("kw_session={}", session.0))
}

/// A person who is a member of `workspace`.
async fn member(pool: &PgPool, workspace: WorkspaceId) -> (UserId, String) {
    let (user, cookie) = person(pool).await;
    sqlx::query("INSERT INTO memberships (workspace_id, user_id, role) VALUES ($1, $2, 'member')")
        .bind(workspace)
        .bind(user)
        .execute(pool)
        .await
        .unwrap();
    (user, cookie)
}

fn post(
    uri: &str,
    cookie: &str,
    content_type: Option<&str>,
    length: Option<u64>,
    body: Body,
) -> Request<Body> {
    let mut builder = Request::post(uri)
        .header("host", "localhost")
        .header("origin", "http://localhost:3000")
        .header("cookie", cookie);
    if let Some(content_type) = content_type {
        builder = builder.header("content-type", content_type);
    }
    if let Some(length) = length {
        builder = builder.header("content-length", length);
    }
    with_peer(builder.body(body).unwrap())
}

fn get(uri: &str, cookie: &str) -> Request<Body> {
    with_peer(
        Request::get(uri)
            .header("host", "localhost")
            .header("cookie", cookie)
            .body(Body::empty())
            .unwrap(),
    )
}

fn with_peer(mut request: Request<Body>) -> Request<Body> {
    request
        .extensions_mut()
        .insert(ConnectInfo("127.0.0.1:7000".parse::<SocketAddr>().unwrap()));
    request
}

async fn send(app: &Router, request: Request<Body>) -> Response {
    app.clone().oneshot(request).await.unwrap()
}

/// `send` for a body that stalls: the rate floor must end it, long before the upload timeout.
/// The deadline only bounds a hang.
async fn send_stalled(app: &Router, request: Request<Body>) -> Response {
    tokio::time::timeout(Duration::from_secs(30), send(app, request))
        .await
        .expect("a stalled upload outlived its rate window")
}

/// A body with no length that streams `chunks`.
fn chunked(chunks: Vec<Vec<u8>>) -> Body {
    Body::from_stream(futures::stream::iter(
        chunks
            .into_iter()
            .map(|chunk| Ok::<_, std::io::Error>(Bytes::from(chunk))),
    ))
}

/// A body that sends `first`, then neither sends nor ends; `started` gains a permit once the
/// server reads it.
fn held(first: Vec<u8>, started: Arc<Semaphore>) -> Body {
    Body::from_stream(async_stream::stream! {
        started.add_permits(1);
        yield Ok::<_, std::io::Error>(Bytes::from(first));
        std::future::pending::<()>().await;
    })
}

async fn bytes_of(response: Response) -> Bytes {
    axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn header<'a>(response: &'a Response, name: &str) -> &'a str {
    response
        .headers()
        .get(name)
        .unwrap_or_else(|| panic!("no {name} header"))
        .to_str()
        .unwrap()
}

#[test]
fn support_copies_are_identical() {
    assert_eq!(
        include_str!("support/cancel_sql.rs"),
        include_str!("../../katwork/tests/support/cancel_sql.rs")
    );
    assert_eq!(
        include_str!("support/locks.rs"),
        include_str!("../../katwork/tests/support/locks.rs")
    );
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn upload_needs_membership_of_the_workspace(pool: PgPool) {
    let env = Env::new(&pool, |_| {}).await;
    let other = Env::new(&pool, |_| {}).await;
    let anonymous = send(
        &env.app,
        post(
            &env.upload_uri("a.txt"),
            "",
            Some("application/octet-stream"),
            Some(1),
            Body::from("a"),
        ),
    )
    .await;
    assert_code(anonymous, 401, "unauthenticated").await;
    // A member of another workspace, at this workspace's path.
    let outsider = send(
        &env.app,
        post(
            &env.upload_uri("a.txt"),
            &other.cookie,
            Some("application/octet-stream"),
            Some(1),
            Body::from("a"),
        ),
    )
    .await;
    assert_code(outsider, 404, "not_found").await;
    env.assert_nothing_stored().await;
    let member = env.uploaded("a.txt", b"a").await;
    assert_eq!(member["name"], "a.txt");
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn upload_to_a_foreign_space_stages_nothing(pool: PgPool) {
    let env = Env::new(&pool, |_| {}).await;
    let other = Env::new(&pool, |_| {}).await;
    for space in [other.space, SpaceId::new()] {
        let uri = format!(
            "/api/workspaces/{}/spaces/{space}/files?name=a.txt",
            env.workspace
        );
        let response = send(
            &env.app,
            post(
                &uri,
                &env.cookie,
                Some("application/octet-stream"),
                Some(1),
                Body::from("a"),
            ),
        )
        .await;
        assert_code(response, 404, "not_found").await;
    }
    env.assert_nothing_stored().await;
    assert!(!env.root().join(env.workspace.to_string()).exists());
    assert_eq!(other.rows().await, 0);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn upload_over_cap_is_413_before_reading_the_body(pool: PgPool) {
    let env = Env::new(&pool, |config| config.upload_max_bytes = MIB).await;
    let polled = Arc::new(AtomicBool::new(false));
    let flag = polled.clone();
    let declared = Body::from_stream(async_stream::stream! {
        flag.store(true, Ordering::SeqCst);
        yield Ok::<_, std::io::Error>(Bytes::from(vec![b'a'; 2 * MIB as usize]));
    });
    let response = send(
        &env.app,
        post(
            &env.upload_uri("big.txt"),
            &env.cookie,
            Some("application/octet-stream"),
            Some(2 * MIB),
            declared,
        ),
    )
    .await;
    assert_code(response, 413, "body_too_large").await;
    assert!(
        !polled.load(Ordering::SeqCst),
        "the body was read before the cap was checked"
    );
    let response = send(
        &env.app,
        post(
            &env.upload_uri("big.txt"),
            &env.cookie,
            Some("application/octet-stream"),
            None,
            chunked(vec![vec![b'a'; 65_536]; 32]),
        ),
    )
    .await;
    assert_code(response, 413, "body_too_large").await;
    env.assert_nothing_stored().await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn upload_of_three_mib_streams_with_the_default_cap(pool: PgPool) {
    let env = Env::new(&pool, |_| {}).await;
    let bytes: Vec<u8> = (0..3 * MIB).map(|i| b'a' + (i % 26) as u8).collect();
    let streamed = send(
        &env.app,
        post(
            &env.upload_uri("three.txt"),
            &env.cookie,
            Some("application/octet-stream"),
            None,
            chunked(bytes.chunks(65_536).map(<[u8]>::to_vec).collect()),
        ),
    )
    .await;
    assert_eq!(streamed.status(), 201);
    let streamed = body(streamed).await;
    assert_eq!(streamed["latest"]["bytes"], 3 * MIB);
    assert_eq!(streamed["latest"]["sha256"], sha256(&bytes));
    let whole = env.uploaded("whole.txt", &bytes).await;
    let download = env.get(&env.content_uri(&whole, "1")).await;
    assert_eq!(download.status(), 200);
    assert_eq!(bytes_of(download).await, bytes);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn upload_of_exactly_the_cap_is_accepted_and_one_byte_more_is_413(pool: PgPool) {
    let env = Env::new(&pool, |config| config.upload_max_bytes = MIB).await;
    let exact = vec![b'x'; MIB as usize];
    let over = vec![b'x'; MIB as usize + 1];
    assert_eq!(env.upload("exact.txt", &exact).await.status(), 201);
    assert_code(env.upload("over.txt", &over).await, 413, "body_too_large").await;
    for (bytes, status) in [(&exact, 201), (&over, 413)] {
        let response = send(
            &env.app,
            post(
                &env.upload_uri("chunked.txt"),
                &env.cookie,
                Some("application/octet-stream"),
                None,
                chunked(bytes.chunks(100_000).map(<[u8]>::to_vec).collect()),
            ),
        )
        .await;
        assert_eq!(response.status(), status);
    }
    assert_eq!(env.rows().await, 2);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn uploads_over_the_concurrency_limit_are_429(pool: PgPool) {
    let env = Env::new(&pool, |config| config.upload_min_bytes_per_sec = 0).await;
    let started = Arc::new(Semaphore::new(0));
    let mut uploads = tokio::task::JoinSet::new();
    let mut hold = |cookie: &str| {
        uploads.spawn(env.app.clone().oneshot(post(
            &env.upload_uri("held.txt"),
            cookie,
            Some("application/octet-stream"),
            None,
            held(b"first".to_vec(), started.clone()),
        )));
    };
    hold(&env.cookie);
    hold(&env.cookie);
    started.acquire_many(2).await.unwrap().forget();
    let third = env.upload("third.txt", b"third").await;
    assert_code(third, 429, "too_many_uploads").await;
    let mut others = Vec::new();
    for _ in 0..3 {
        let (_, cookie) = member(&env.core.pool, env.workspace).await;
        hold(&cookie);
        hold(&cookie);
        others.push(cookie);
    }
    started.acquire_many(6).await.unwrap().forget();
    let (_, ninth) = member(&env.core.pool, env.workspace).await;
    let response = send(
        &env.app,
        post(
            &env.upload_uri("ninth.txt"),
            &ninth,
            Some("application/octet-stream"),
            Some(5),
            Body::from("ninth"),
        ),
    )
    .await;
    assert_code(response, 429, "too_many_uploads").await;
    uploads.abort_all();
    while uploads.join_next().await.is_some() {}
    let freed = send(
        &env.app,
        post(
            &env.upload_uri("ninth.txt"),
            &ninth,
            Some("application/octet-stream"),
            Some(5),
            Body::from("ninth"),
        ),
    )
    .await;
    assert_eq!(freed.status(), 201);
    wait_for(|| env.tmp_files().is_empty()).await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn slow_upload_is_408(pool: PgPool) {
    let env = Env::new(&pool, |config| {
        config.upload_min_bytes_per_sec = 1_048_576;
        config.upload_rate_window = Duration::from_millis(200);
    })
    .await;
    let response = send_stalled(
        &env.app,
        post(
            &env.upload_uri("slow.txt"),
            &env.cookie,
            Some("application/octet-stream"),
            Some(4096),
            held(vec![b's'; 1024], Arc::new(Semaphore::new(0))),
        ),
    )
    .await;
    assert_code(response, 408, "upload_too_slow").await;
    env.assert_nothing_stored().await;
    env.assert_slots_free();
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn storage_quota_and_free_space_floor_are_507(pool: PgPool) {
    let env = Env::new(&pool, |config| config.workspace_storage_bytes = MIB).await;
    let part = vec![b'q'; 600 * 1024];
    assert_eq!(env.upload("one.txt", &part).await.status(), 201);
    assert_code(
        env.upload("two.txt", &part).await,
        507,
        "storage_quota_exceeded",
    )
    .await;
    // A body with no length is counted at the cap, which is over this quota.
    let response = send(
        &env.app,
        post(
            &env.upload_uri("small.txt"),
            &env.cookie,
            Some("application/octet-stream"),
            None,
            chunked(vec![b"small".to_vec()]),
        ),
    )
    .await;
    assert_code(response, 507, "storage_quota_exceeded").await;
    assert_eq!(env.rows().await, 1);
    let full = Env::with_free(&pool, |_| {}, 100 * MIB).await;
    assert_code(full.upload("a.txt", b"a").await, 507, "storage_full").await;
    full.assert_nothing_stored().await;
}

/// A body that signals `started` when the server first reads it, then sends `bytes` once `go`
/// has a permit for it.
fn gated(bytes: Vec<u8>, started: Arc<Semaphore>, go: Arc<Semaphore>) -> Body {
    Body::from_stream(async_stream::stream! {
        started.add_permits(1);
        go.acquire().await.unwrap().forget();
        yield Ok::<_, std::io::Error>(Bytes::from(bytes));
    })
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn concurrent_uploads_never_pass_the_quota(pool: PgPool) {
    let env = Env::new(&pool, |config| {
        config.workspace_storage_bytes = MIB;
        config.upload_min_bytes_per_sec = 0;
    })
    .await;
    let (started, go) = (Arc::new(Semaphore::new(0)), Arc::new(Semaphore::new(0)));
    // Each upload fits the quota alone; the two together do not.
    let size = 600 * 1024;
    let uploads: Vec<_> = [("one.txt", b'q'), ("two.txt", b'r')]
        .into_iter()
        .map(|(name, byte)| {
            let request = post(
                &env.upload_uri(name),
                &env.cookie,
                Some("application/octet-stream"),
                Some(size as u64),
                gated(vec![byte; size], started.clone(), go.clone()),
            );
            tokio::spawn(env.app.clone().oneshot(request))
        })
        .collect();
    // Both bodies are being read, so both passed the check before the body, each against an
    // empty workspace.
    tokio::time::timeout(Duration::from_secs(10), started.acquire_many(2))
        .await
        .expect("both uploads reach their bodies")
        .unwrap()
        .forget();
    go.add_permits(2);
    let mut responses = Vec::new();
    for upload in uploads {
        responses.push(upload.await.unwrap().unwrap());
    }
    responses.sort_by_key(|response| response.status());
    let mut responses = responses.into_iter();
    assert_eq!(responses.next().unwrap().status(), 201);
    assert_code(responses.next().unwrap(), 507, "storage_quota_exceeded").await;
    assert_eq!(env.rows().await, 1);
    assert_eq!(env.blob_files().len(), 1);
    wait_for(|| env.tmp_files().is_empty()).await;
    env.assert_slots_free();
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn upload_from_another_origin_is_403(pool: PgPool) {
    let env = Env::new(&pool, |_| {}).await;
    let mut request = post(
        &env.upload_uri("a.txt"),
        &env.cookie,
        Some("application/octet-stream"),
        Some(1),
        Body::from("a"),
    );
    request
        .headers_mut()
        .insert("origin", "https://evil.example".parse().unwrap());
    assert_code(send(&env.app, request).await, 403, "cross_site_request").await;
    let mut request = post(
        &env.upload_uri("a.txt"),
        &env.cookie,
        Some("application/octet-stream"),
        Some(1),
        Body::from("a"),
    );
    request
        .headers_mut()
        .insert("sec-fetch-site", "cross-site".parse().unwrap());
    assert_code(send(&env.app, request).await, 403, "cross_site_request").await;
    env.assert_nothing_stored().await;
    assert!(!env.root().join(env.workspace.to_string()).exists());
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn upload_with_json_content_type_is_415(pool: PgPool) {
    let env = Env::new(&pool, |_| {}).await;
    let response = send(
        &env.app,
        post(
            &env.upload_uri("a.txt"),
            &env.cookie,
            Some("application/json"),
            Some(2),
            Body::from("{}"),
        ),
    )
    .await;
    assert_code(response, 415, "octet_stream_required").await;
    env.assert_nothing_stored().await;
    assert!(!env.root().join(env.workspace.to_string()).exists());
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn upload_content_type_is_matched_like_the_json_check(pool: PgPool) {
    let env = Env::new(&pool, |_| {}).await;
    for (index, content_type) in [
        "application/octet-stream",
        "APPLICATION/Octet-Stream",
        "application/octet-stream; charset=binary",
        "application/octet-stream ; x=y",
    ]
    .into_iter()
    .enumerate()
    {
        let response = send(
            &env.app,
            post(
                &env.upload_uri(&format!("{index}.txt")),
                &env.cookie,
                Some(content_type),
                Some(1),
                Body::from("a"),
            ),
        )
        .await;
        assert_eq!(response.status(), 201, "{content_type}");
    }
    for content_type in [
        "application/json",
        "text/plain",
        "application/octet-streamx",
        "multipart/form-data; boundary=x",
        "application/x-www-form-urlencoded",
    ] {
        let response = send(
            &env.app,
            post(
                &env.upload_uri("refused.txt"),
                &env.cookie,
                Some(content_type),
                Some(1),
                Body::from("a"),
            ),
        )
        .await;
        assert_code(response, 415, "octet_stream_required").await;
    }
    let mut doubled = post(
        &env.upload_uri("refused.txt"),
        &env.cookie,
        Some("application/octet-stream"),
        Some(1),
        Body::from("a"),
    );
    doubled
        .headers_mut()
        .append("content-type", "application/octet-stream".parse().unwrap());
    assert_code(send(&env.app, doubled).await, 415, "octet_stream_required").await;
    assert_eq!(env.rows().await, 4);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn empty_upload_without_content_type_is_415(pool: PgPool) {
    let env = Env::new(&pool, |_| {}).await;
    let response = send(
        &env.app,
        post(
            &env.upload_uri("empty.txt"),
            &env.cookie,
            None,
            None,
            Body::empty(),
        ),
    )
    .await;
    assert_code(response, 415, "octet_stream_required").await;
    env.assert_nothing_stored().await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn octet_stream_is_still_415_on_json_routes(pool: PgPool) {
    let env = Env::new(&pool, |_| {}).await;
    for uri in [
        "/api/auth/login".to_owned(),
        format!("/api/workspaces/{}/spaces", env.workspace),
    ] {
        let response = send(
            &env.app,
            post(
                &uri,
                &env.cookie,
                Some("application/octet-stream"),
                Some(2),
                Body::from("{}"),
            ),
        )
        .await;
        assert_code(response, 415, "json_required").await;
    }
    // The list route shares the upload's path, but only POST takes a file.
    let mut request = post(
        &env.files_uri(),
        &env.cookie,
        Some("application/octet-stream"),
        Some(2),
        Body::from("{}"),
    );
    *request.method_mut() = axum::http::Method::PUT;
    assert_code(send(&env.app, request).await, 415, "json_required").await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn upload_name_decoding_table(pool: PgPool) {
    let env = Env::new(&pool, |_| {}).await;
    let upload = |query: &'static str| {
        let request = post(
            &format!("{}{query}", env.files_uri()),
            &env.cookie,
            Some("application/octet-stream"),
            Some(4),
            Body::from("text"),
        );
        send(&env.app, request)
    };
    for (query, name) in [
        ("?name=a%2Bb.txt", "a+b.txt"),
        ("?name=a+b.txt", "a b.txt"),
        ("?name=a%20c.txt", "a c.txt"),
        ("?name=e%CC%81.txt", "\u{e9}.txt"),
        (
            "?name=%E0%B8%9B%E0%B8%A3%E0%B8%B0%E0%B8%8A%E0%B8%B8%E0%B8%A1%2F%E0%B8%A7%E0%B8%B2%E0%B8%A3%E0%B8%B0.txt",
            "ประชุม/วาระ.txt",
        ),
        ("?name=folder/x.txt", "folder/x.txt"),
        ("?other=1&name=d.txt&x", "d.txt"),
    ] {
        let response = upload(query).await;
        assert_eq!(response.status(), 201, "{query}");
        let view = body(response).await;
        assert_eq!(view["name"], name, "{query}");
    }
    for query in [
        "",
        "?",
        "?name=",
        "?name",
        "?nam=a.txt",
        "?name=%FF.txt",
        "?name=%C3.txt",
        "?name=a.txt&name=b.txt",
        "?name=a/b/c/d/e/f/g/h/i.txt",
        "?name=%2E%2E%2Fx.txt",
        "?name=%2Fx.txt",
        "?name=a%00b.txt",
        "?name=a%E2%80%AEb.txt",
    ] {
        assert_code(upload(query).await, 400, "file_name_invalid").await;
    }
    assert_eq!(env.rows().await, 7);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn permit_is_released_after_every_failure(pool: PgPool) {
    // 507 for the quota and for free space.
    let quota = Env::new(&pool, |config| config.workspace_storage_bytes = MIB).await;
    for _ in 0..3 {
        let big = vec![b'b'; MIB as usize + 1];
        assert_code(
            quota.upload("big.txt", &big).await,
            507,
            "storage_quota_exceeded",
        )
        .await;
        quota.assert_slots_free();
    }
    let full = Env::with_free(&pool, |_| {}, 100 * MIB).await;
    for _ in 0..3 {
        assert_code(full.upload("a.txt", b"a").await, 507, "storage_full").await;
        full.assert_slots_free();
    }
    // 413 for a body with no length, past the cap.
    let small = Env::new(&pool, |config| config.upload_max_bytes = 1024).await;
    for _ in 0..3 {
        let response = send(
            &small.app,
            post(
                &small.upload_uri("over.txt"),
                &small.cookie,
                Some("application/octet-stream"),
                None,
                chunked(vec![vec![b'o'; 1000], vec![b'o'; 1000]]),
            ),
        )
        .await;
        assert_code(response, 413, "body_too_large").await;
        small.assert_slots_free();
    }
    // 408 for a stalled body.
    let slow = Env::new(&pool, |config| {
        config.upload_min_bytes_per_sec = 1_048_576;
        config.upload_rate_window = Duration::from_millis(100);
    })
    .await;
    for _ in 0..3 {
        let response = send_stalled(
            &slow.app,
            post(
                &slow.upload_uri("slow.txt"),
                &slow.cookie,
                Some("application/octet-stream"),
                None,
                held(b"slow".to_vec(), Arc::new(Semaphore::new(0))),
            ),
        )
        .await;
        assert_code(response, 408, "upload_too_slow").await;
        slow.assert_slots_free();
    }
    // 400 for a body that breaks off, and 415 for a file that is not what its name says.
    let env = Env::new(&pool, |_| {}).await;
    for _ in 0..3 {
        let broken = Body::from_stream(futures::stream::iter([
            Ok(Bytes::from_static(b"half")),
            Err(std::io::Error::other("connection reset")),
        ]));
        let response = send(
            &env.app,
            post(
                &env.upload_uri("broken.txt"),
                &env.cookie,
                Some("application/octet-stream"),
                None,
                broken,
            ),
        )
        .await;
        assert_code(response, 400, "request_invalid").await;
        env.assert_slots_free();
        assert_code(
            env.upload("fake.pdf", b"not a pdf").await,
            415,
            "file_type_unsupported",
        )
        .await;
        env.assert_slots_free();
    }
    // 503 when the blob lock is held past the lock timeout.
    let busy = Env::new(&pool, |config| {
        config.lock_timeout = Duration::from_millis(100)
    })
    .await;
    let bytes = b"bytes someone else is committing";
    let mut holder = PgConnection::connect_with(&pool.connect_options())
        .await
        .unwrap();
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(store::blob_lock_key(busy.workspace, &sha256(bytes)))
        .execute(&mut holder)
        .await
        .unwrap();
    for _ in 0..3 {
        let response = busy.upload("busy.txt", bytes).await;
        assert_eq!(response.status(), 503);
        assert_eq!(header(&response, "retry-after"), "1");
        assert_code(response, 503, "busy").await;
        busy.assert_slots_free();
    }
    holder.close().await.unwrap();
    assert_eq!(busy.upload("busy.txt", bytes).await.status(), 201);
    busy.assert_slots_free();
    for env in [&quota, &full, &small, &slow, &env] {
        wait_for(|| env.tmp_files().is_empty()).await;
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn refused_upload_leaves_tmp_empty(pool: PgPool) {
    let env = Env::new(&pool, |config| config.upload_max_bytes = 1024).await;
    let response = send(
        &env.app,
        post(
            &env.upload_uri("over.txt"),
            &env.cookie,
            Some("application/octet-stream"),
            None,
            chunked(vec![vec![b'o'; 1000], vec![b'o'; 1000]]),
        ),
    )
    .await;
    assert_code(response, 413, "body_too_large").await;
    assert_code(
        env.upload("fake.docx", b"%PDF-1.7 not a package").await,
        415,
        "file_type_unsupported",
    )
    .await;
    assert_code(
        env.upload("legacy.txt", &[0xCA, 0xC7, 0xD1, 0xCA, 0xB4, 0xD5])
            .await,
        415,
        "file_encoding_unsupported",
    )
    .await;
    env.assert_nothing_stored().await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn empty_txt_upload_is_accepted_with_zero_bytes(pool: PgPool) {
    let env = Env::new(&pool, |_| {}).await;
    let view = env.uploaded("empty.txt", b"").await;
    assert_eq!(view["latest"]["bytes"], 0);
    assert_eq!(view["latest"]["sha256"], sha256(b""));
    let download = env.get(&env.content_uri(&view, "1")).await;
    assert_eq!(download.status(), 200);
    assert_eq!(header(&download, "content-length"), "0");
    assert!(bytes_of(download).await.is_empty());
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn empty_pdf_upload_is_415(pool: PgPool) {
    let env = Env::new(&pool, |_| {}).await;
    assert_code(
        env.upload("empty.pdf", b"").await,
        415,
        "file_type_unsupported",
    )
    .await;
    env.assert_nothing_stored().await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn client_disconnect_mid_upload_leaves_no_row_and_no_blob(pool: PgPool) {
    let env = Env::new(&pool, |config| config.upload_min_bytes_per_sec = 0).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(katwork_server::transport::serve_with_shutdown(
        listener,
        env.app.clone(),
        TransportConfig::default(),
        std::future::pending(),
    ));
    let mut client = TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "POST {} HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:3000\r\nCookie: {}\r\nContent-Type: application/octet-stream\r\nContent-Length: 65536\r\n\r\n",
        env.upload_uri("half.txt"),
        env.cookie
    );
    client.write_all(head.as_bytes()).await.unwrap();
    client.write_all(&[b'h'; 32_768]).await.unwrap();
    client.flush().await.unwrap();
    // The server has begun to stage the body.
    wait_for(|| !env.tmp_files().is_empty()).await;
    drop(client);
    wait_for(|| env.tmp_files().is_empty()).await;
    assert_eq!(env.rows().await, 0);
    assert!(env.blob_files().is_empty());
    assert_pool_clean(&env.core.pool).await;
    wait_for(|| {
        let first = env.core.uploads.try_acquire(env.user);
        let second = env.core.uploads.try_acquire(env.user);
        first.is_ok() && second.is_ok()
    })
    .await;
    server.abort();
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn upload_commits_when_its_client_leaves_after_the_rename(pool: PgPool) {
    let gate = Gate {
        reached: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    let env = Env::committing(&pool, OnCommit::PauseAfterRename(gate.clone())).await;
    let bytes = b"kept after the client left";
    let request = post(
        &env.upload_uri("kept.txt"),
        &env.cookie,
        Some("application/octet-stream"),
        Some(bytes.len() as u64),
        Body::from(bytes.to_vec()),
    );
    let client = tokio::spawn(env.app.clone().oneshot(request));
    tokio::time::timeout(Duration::from_secs(10), gate.reached.notified())
        .await
        .expect("the upload never reached its rename");
    // The blob is in place and its rows are not committed when the client goes away: hyper
    // drops the request's future, as this abort does.
    client.abort();
    assert!(client.await.unwrap_err().is_cancelled());
    gate.release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), async {
        while env.rows().await == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the upload's rows never committed after its client left");
    assert_eq!(env.blob_files(), vec![env.blob_path(&sha256(bytes))]);
    wait_for(|| {
        let first = env.core.uploads.try_acquire(env.user);
        let second = env.core.uploads.try_acquire(env.user);
        first.is_ok() && second.is_ok()
    })
    .await;
    wait_for(|| env.tmp_files().is_empty()).await;
    assert_pool_clean(&env.core.pool).await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn shutdown_waits_for_an_upload_commit_whose_client_left(pool: PgPool) {
    let gate = Gate {
        reached: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    let env = Env::committing(&pool, OnCommit::PauseAfterRename(gate.clone())).await;
    let bytes = b"committed before the process ends";
    let request = post(
        &env.upload_uri("shutdown.txt"),
        &env.cookie,
        Some("application/octet-stream"),
        Some(bytes.len() as u64),
        Body::from(bytes.to_vec()),
    );
    let client = tokio::spawn(env.app.clone().oneshot(request));
    tokio::time::timeout(Duration::from_secs(10), gate.reached.notified())
        .await
        .expect("the upload never reached its rename");
    client.abort();
    assert!(client.await.unwrap_err().is_cancelled());
    // The server has stopped taking requests, as `serve` does on a shutdown signal: it waits for
    // the commit, paused after its rename, and never past the grace it is given.
    assert!(
        !env.core.finish_commits(Duration::from_millis(100)).await,
        "the shutdown passed a commit that was still running"
    );
    let mut finishing = Box::pin(env.core.finish_commits(Duration::from_secs(30)));
    assert!(futures::poll!(finishing.as_mut()).is_pending());
    gate.release.notify_one();
    assert!(finishing.await, "the commit outlived the grace");
    assert_eq!(env.rows().await, 1);
    assert_eq!(env.blob_files(), vec![env.blob_path(&sha256(bytes))]);
    env.assert_slots_free();
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn a_panic_in_the_commit_answers_500_and_frees_the_slot(pool: PgPool) {
    let env = Env::committing(&pool, OnCommit::Panic).await;
    assert_code(
        env.upload("panics.txt", b"never stored").await,
        500,
        "io_error",
    )
    .await;
    env.assert_nothing_stored().await;
    env.assert_slots_free();
    assert_pool_clean(&env.core.pool).await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn download_is_an_attachment_with_nosniff_no_store_and_sandbox(pool: PgPool) {
    let env = Env::new(&pool, |_| {}).await;
    let bytes = "บันทึกการประชุม".as_bytes();
    let file = env.uploaded("ประชุม/วาระ.txt", bytes).await;
    let download = env.get(&env.content_uri(&file, "1")).await;
    assert_eq!(download.status(), 200);
    assert_eq!(header(&download, "content-type"), "text/plain");
    assert_eq!(
        header(&download, "content-disposition"),
        "attachment; filename*=UTF-8''%E0%B8%A7%E0%B8%B2%E0%B8%A3%E0%B8%B0.txt"
    );
    assert_eq!(header(&download, "x-content-type-options"), "nosniff");
    assert_eq!(header(&download, "cache-control"), "private, no-store");
    assert_eq!(
        header(&download, "content-security-policy"),
        "default-src 'none'; sandbox"
    );
    assert_eq!(header(&download, "content-length"), bytes.len().to_string());
    assert_eq!(bytes_of(download).await, bytes);
    // Every other response keeps the default policy, the download route's errors included.
    let missing = env.get(&env.content_uri(&file, "2")).await;
    assert_eq!(header(&missing, "content-security-policy"), DEFAULT_CSP);
    assert_code(missing, 404, "not_found").await;
    let listed = env.get(&env.files_uri()).await;
    assert_eq!(header(&listed, "content-security-policy"), DEFAULT_CSP);
    let uploaded = env.upload("another.txt", b"x").await;
    assert_eq!(header(&uploaded, "content-security-policy"), DEFAULT_CSP);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn download_content_disposition_is_rfc8187_encoded(pool: PgPool) {
    let env = Env::new(&pool, |_| {}).await;
    let zip = b"PK\x03\x04 a package".as_slice();
    let pdf = b"%PDF-1.7 a document".as_slice();
    let text = b"plain text".as_slice();
    for (name, bytes, encoded) in [
        (
            "วาระ.docx",
            zip,
            "%E0%B8%A7%E0%B8%B2%E0%B8%A3%E0%B8%B0.docx",
        ),
        (
            "รายงานการประชุม 2568-10.docx",
            zip,
            "%E0%B8%A3%E0%B8%B2%E0%B8%A2%E0%B8%87%E0%B8%B2%E0%B8%99%E0%B8%81%E0%B8%B2%E0%B8%A3%E0%B8%9B%E0%B8%A3%E0%B8%B0%E0%B8%8A%E0%B8%B8%E0%B8%A1%202568-10.docx",
        ),
        ("a'b*c%d;e,f\"g.txt", text, "a%27b%2Ac%25d%3Be%2Cf%22g.txt"),
        (
            "q1 (ร่าง).xlsx",
            zip,
            "q1%20%28%E0%B8%A3%E0%B9%88%E0%B8%B2%E0%B8%87%29.xlsx",
        ),
        (
            "รายได้ไตรมาส ๑.csv",
            text,
            "%E0%B8%A3%E0%B8%B2%E0%B8%A2%E0%B9%84%E0%B8%94%E0%B9%89%E0%B9%84%E0%B8%95%E0%B8%A3%E0%B8%A1%E0%B8%B2%E0%B8%AA%20%E0%B9%91.csv",
        ),
        ("a+b.txt", text, "a+b.txt"),
        ("\u{e9}.pdf", pdf, "%C3%A9.pdf"),
        ("folder/sub/report.pdf", pdf, "report.pdf"),
    ] {
        let file = env.uploaded(name, bytes).await;
        let download = env.get(&env.content_uri(&file, "1")).await;
        assert_eq!(download.status(), 200, "{name}");
        let value = download
            .headers()
            .get("content-disposition")
            .unwrap()
            .to_str()
            .expect("the header is visible ASCII");
        assert_eq!(value, format!("attachment; filename*=UTF-8''{encoded}"));
        let decoded =
            percent_decode_str(value.strip_prefix("attachment; filename*=UTF-8''").unwrap())
                .decode_utf8()
                .unwrap();
        assert_eq!(decoded, name.rsplit('/').next().unwrap());
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn download_content_type_follows_the_media_type(pool: PgPool) {
    let env = Env::new(&pool, |_| {}).await;
    for (name, bytes, mime) in [
        ("a.pdf", b"%PDF-1.7".as_slice(), "application/pdf"),
        (
            "a.docx",
            b"PK\x03\x04".as_slice(),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        ),
        (
            "a.xlsx",
            b"PK\x03\x04".as_slice(),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        ),
        ("a.txt", b"text".as_slice(), "text/plain"),
        ("a.md", b"# notes".as_slice(), "text/markdown"),
        ("a.csv", b"a,b\n1,2".as_slice(), "text/csv"),
        ("a.png", b"\x89PNG\r\n\x1a\n rest".as_slice(), "image/png"),
        ("a.jpg", b"\xff\xd8\xff\xe0 rest".as_slice(), "image/jpeg"),
    ] {
        let file = env.uploaded(name, bytes).await;
        let download = env.get(&env.content_uri(&file, "1")).await;
        assert_eq!(header(&download, "content-type"), mime, "{name}");
        assert_eq!(bytes_of(download).await, bytes);
    }
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn download_of_a_tampered_blob_is_500_blob_corrupt_with_no_partial_body(pool: PgPool) {
    let env = Env::new(&pool, |_| {}).await;
    let file = env.uploaded("a.txt", b"the stored bytes").await;
    let path = env.blob_path(file["latest"]["sha256"].as_str().unwrap());
    std::fs::write(&path, b"forged  bytes!!!").unwrap();
    assert_code(
        env.get(&env.content_uri(&file, "1")).await,
        500,
        "blob_corrupt",
    )
    .await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn download_of_a_missing_blob_is_404(pool: PgPool) {
    let env = Env::new(&pool, |_| {}).await;
    let file = env.uploaded("a.txt", b"short-lived").await;
    std::fs::remove_file(env.blob_path(file["latest"]["sha256"].as_str().unwrap())).unwrap();
    assert_code(
        env.get(&env.content_uri(&file, "1")).await,
        404,
        "not_found",
    )
    .await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn download_version_path_table(pool: PgPool) {
    let env = Env::new(&pool, |_| {}).await;
    let file = env.uploaded("a.txt", b"one version").await;
    assert_eq!(env.get(&env.content_uri(&file, "1")).await.status(), 200);
    for version in ["0", "-1", "2", "2147483647"] {
        assert_code(
            env.get(&env.content_uri(&file, version)).await,
            404,
            "not_found",
        )
        .await;
    }
    for version in ["abc", "1.0", "2147483648"] {
        assert_code(
            env.get(&env.content_uri(&file, version)).await,
            400,
            "request_invalid",
        )
        .await;
    }
    let malformed = format!(
        "/api/workspaces/{}/files/not-a-uuid/versions/1/content",
        env.workspace
    );
    assert_code(env.get(&malformed).await, 400, "request_invalid").await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn download_of_another_workspace_file_is_404(pool: PgPool) {
    let env = Env::new(&pool, |_| {}).await;
    let other = Env::new(&pool, |_| {}).await;
    // The requester holds the same bytes, so its own store has the blob: only the query's
    // tenant filter can answer 404, never a missing file.
    let bytes = b"identical bytes in both workspaces";
    env.uploaded("mine.txt", bytes).await;
    let theirs = other.uploaded("secret-name.txt", bytes).await;
    let at_our_path = format!(
        "/api/workspaces/{}/files/{}/versions/1/content",
        env.workspace,
        theirs["id"].as_str().unwrap()
    );
    assert_code(env.get(&at_our_path).await, 404, "not_found").await;
    let at_their_path = other.content_uri(&theirs, "1");
    assert_code(env.get(&at_their_path).await, 404, "not_found").await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn list_filters_by_folder_prefix(pool: PgPool) {
    let env = Env::new(&pool, |_| {}).await;
    for name in ["a/x.txt", "a/y.txt", "b/z.txt"] {
        env.uploaded(name, name.as_bytes()).await;
    }
    async fn names(env: &Env, query: &str) -> Vec<String> {
        let response = env.get(&format!("{}{query}", env.files_uri())).await;
        assert_eq!(response.status(), 200, "{query}");
        body(response).await["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|file| file["name"].as_str().unwrap().to_owned())
            .collect()
    }
    assert_eq!(names(&env, "?prefix=a%2F").await, ["a/x.txt", "a/y.txt"]);
    assert_eq!(names(&env, "?prefix=a/").await, ["a/x.txt", "a/y.txt"]);
    assert_eq!(names(&env, "?prefix=b").await, ["b/z.txt"]);
    assert_eq!(names(&env, "").await, ["a/x.txt", "a/y.txt", "b/z.txt"]);
    assert!(names(&env, "?prefix=c/").await.is_empty());
    let listed = body(env.get(&env.files_uri()).await).await;
    assert_eq!(listed["files"][0]["role"], "input");
    assert_eq!(listed["files"][0]["display"], "a/x.txt");
    assert_code(
        env.get(&format!("{}?prefix=%FF", env.files_uri())).await,
        400,
        "file_name_invalid",
    )
    .await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn list_route_for_a_foreign_space_is_404(pool: PgPool) {
    let env = Env::new(&pool, |_| {}).await;
    let other = Env::new(&pool, |_| {}).await;
    other.uploaded("theirs.txt", b"theirs").await;
    for space in [other.space, SpaceId::new()] {
        let uri = format!("/api/workspaces/{}/spaces/{space}/files", env.workspace);
        assert_code(env.get(&uri).await, 404, "not_found").await;
    }
    assert_code(env.get(&other.files_uri()).await, 404, "not_found").await;
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn list_and_download_cancelled_during_their_statements_leave_pool_clean(pool: PgPool) {
    let env = Env::single(&pool).await;
    let app_pool = &env.core.pool;
    let file = env.uploaded("a.txt", b"listed and downloaded").await;
    cancel_sql::drop_during_guarded_statement(app_pool, "artifacts", || {
        send(&env.app, get(&env.files_uri(), &env.cookie))
    })
    .await;
    assert_pool_clean(app_pool).await;
    cancel_sql::drop_during_guarded_statement(app_pool, "artifacts", || {
        send(&env.app, get(&env.content_uri(&file, "1"), &env.cookie))
    })
    .await;
    assert_pool_clean(app_pool).await;
    cancel_sql::probe_write(app_pool).await;
    assert_eq!(env.get(&env.files_uri()).await.status(), 200);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn reads_before_the_body_wait_at_most_the_lock_timeout(pool: PgPool) {
    let env = Env::new(&pool, |config| {
        config.lock_timeout = Duration::from_millis(200);
    })
    .await;
    for table in ["spaces", "artifact_versions"] {
        let mut blocker = PgConnection::connect_with(&pool.connect_options())
            .await
            .unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "BEGIN; LOCK TABLE {table} IN ACCESS EXCLUSIVE MODE"
        )))
        .execute(&mut blocker)
        .await
        .unwrap();
        // The deadline only bounds a hang: the lock timeout ends the wait long before it.
        let response = tokio::time::timeout(Duration::from_secs(10), env.upload("a.txt", b"bytes"))
            .await
            .unwrap_or_else(|_| panic!("an upload waited past its lock timeout on {table}"));
        assert_code(response, 503, "busy").await;
        sqlx::query("ROLLBACK").execute(&mut blocker).await.unwrap();
        blocker.close().await.unwrap();
        env.assert_nothing_stored().await;
        env.assert_slots_free();
    }
    assert_eq!(env.upload("a.txt", b"bytes").await.status(), 201);
}

#[sqlx::test(migrations = "../katwork/migrations")]
async fn upload_route_cancelled_during_space_and_quota_reads_leaves_pool_clean(pool: PgPool) {
    let env = Env::single(&pool).await;
    let app_pool = &env.core.pool;
    for table in ["spaces", "artifact_versions"] {
        cancel_sql::drop_during_guarded_statement(app_pool, table, || {
            send(
                &env.app,
                post(
                    &env.upload_uri("a.txt"),
                    &env.cookie,
                    Some("application/octet-stream"),
                    Some(5),
                    Body::from("bytes"),
                ),
            )
        })
        .await;
        assert_pool_clean(app_pool).await;
        env.assert_nothing_stored().await;
        assert!(!env.root().join(env.workspace.to_string()).exists());
        env.assert_slots_free();
    }
    cancel_sql::probe_write(app_pool).await;
    assert_eq!(env.upload("a.txt", b"bytes").await.status(), 201);
}
