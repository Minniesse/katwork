#![allow(
    clippy::disallowed_methods,
    reason = "Test fixtures drain SQL; lock probes hold their own transactions"
)]
#[allow(dead_code)]
mod support;

use std::{
    collections::BTreeSet,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use bytes::Bytes;
use futures::StreamExt;
use katwork::{
    artifacts::{
        ArtifactRole, BlobRef, BlobStore, GridcraftCheck, LocalBlobStore, MediaType,
        media::{self, TextProbe},
        names,
        store::{
            self, ArtifactView, OutputCtx, VersionView,
            test_hooks::{Gate, pause_after_first_lock},
        },
        uploads::{self, RateCheck},
    },
    config::Config,
    db,
    error::KatError,
    ids::*,
    runtime::Core,
    tenancy::{self, Tenant},
};
use sha2::{Digest, Sha256};
use sqlx::{Connection, PgConnection, PgPool};
use support::{Fixture, assert_pool_clean, cancel_sql, locks};
use tokio::time::Instant;
use uuid::Uuid;

const LOCK: Duration = Duration::from_secs(5);

/// A directory under the system temp dir, removed when dropped.
struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("katwork-artifacts-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn root(&self) -> PathBuf {
        self.0.join("artifacts")
    }

    fn store(&self) -> LocalBlobStore {
        LocalBlobStore::new(self.root()).unwrap()
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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

fn blob_files(root: &Path, workspace: WorkspaceId) -> Vec<PathBuf> {
    files_under(&root.join(workspace.to_string()).join("sha256"))
}

fn tmp_files(root: &Path, workspace: WorkspaceId) -> Vec<PathBuf> {
    files_under(&root.join(workspace.to_string()).join("tmp"))
}

fn blob_path(root: &Path, blob: &BlobRef) -> PathBuf {
    root.join(blob.workspace_id.to_string())
        .join("sha256")
        .join(&blob.sha256[0..2])
        .join(&blob.sha256[2..4])
        .join(&blob.sha256)
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

async fn tenant(pool: &PgPool, f: &Fixture) -> Tenant {
    tenancy::resolve(pool, UserId(f.user), WorkspaceId(f.workspace))
        .await
        .unwrap()
}

fn ctx(f: &Fixture, tenant: &Tenant) -> OutputCtx {
    OutputCtx {
        tenant: tenant.clone(),
        space_id: SpaceId(f.space),
        kat_id: KatId(f.kat),
        job_id: JobId(f.job),
        run_id: RunId(f.run),
    }
}

async fn upload(
    pool: &PgPool,
    blobs: &dyn BlobStore,
    tenant: &Tenant,
    space: SpaceId,
    name: &str,
    bytes: &[u8],
) -> ArtifactView {
    let staged = blobs
        .stage_bytes(tenant.workspace_id(), bytes)
        .await
        .unwrap();
    store::create_input(
        pool,
        blobs,
        tenant,
        space,
        name,
        MediaType::Text,
        staged,
        LOCK,
    )
    .await
    .unwrap()
}

/// Commits `bytes` and creates an output with them as version 1, in one transaction.
async fn output(
    pool: &PgPool,
    blobs: &dyn BlobStore,
    ctx: &OutputCtx,
    name: &str,
    bytes: &[u8],
    key: &str,
) -> Result<VersionView, KatError> {
    let staged = blobs
        .stage_bytes(ctx.tenant.workspace_id(), bytes)
        .await
        .unwrap();
    db::guarded(pool, async |conn| {
        let mut tx = conn.begin().await?;
        db::set_lock_timeout(&mut tx, LOCK).await?;
        let blob = store::commit_blob_on(&mut tx, blobs, staged).await?;
        let version =
            store::create_output_on(&mut tx, ctx, name, MediaType::Docx, &blob, None, key).await?;
        tx.commit().await?;
        Ok(version)
    })
    .await
}

/// `lock_artifacts_on` with `output` and no sources, then a new version of it, in one
/// transaction. The backend's pid is sent before the lock.
async fn new_version(
    pool: &PgPool,
    blobs: &dyn BlobStore,
    ctx: &OutputCtx,
    artifact: ArtifactId,
    bytes: &[u8],
    key: &str,
    pid: Option<tokio::sync::oneshot::Sender<i32>>,
) -> Result<VersionView, KatError> {
    let staged = blobs
        .stage_bytes(ctx.tenant.workspace_id(), bytes)
        .await
        .unwrap();
    db::guarded(pool, async |conn| {
        let mut tx = conn.begin().await?;
        db::set_lock_timeout(&mut tx, Duration::from_secs(30)).await?;
        if let Some(pid) = pid {
            let own: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(&mut *tx)
                .await?;
            pid.send(own).unwrap();
        }
        store::lock_artifacts_on(&mut tx, &ctx.tenant, Some(artifact), &[]).await?;
        let blob = store::commit_blob_on(&mut tx, blobs, staged).await?;
        let version =
            store::new_output_version_on(&mut tx, ctx, artifact, MediaType::Docx, &blob, None, key)
                .await?;
        tx.commit().await?;
        Ok(version)
    })
    .await
}

async fn mark_deleted(pool: &PgPool, artifact: ArtifactId, by: Uuid) {
    sqlx::query("UPDATE artifacts SET deleted_at = now(), deleted_by = $2 WHERE id = $1")
        .bind(artifact)
        .bind(by)
        .execute(pool)
        .await
        .unwrap();
}

/// Tries `SELECT … <mode>` on one artifact row in a transaction of its own that rolls back.
async fn probe(pool: &PgPool, artifact: ArtifactId, mode: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect_with(&pool.connect_options())
        .await
        .unwrap();
    let mut tx = conn.begin().await.unwrap();
    let result = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT 1 FROM artifacts WHERE id = $1 {mode}"
    )))
    .bind(artifact)
    .execute(&mut *tx)
    .await
    .map(|_| ());
    drop(tx);
    conn.close().await.unwrap();
    result
}

fn lock_not_available(result: Result<(), sqlx::Error>) {
    support::sqlstate(result.unwrap_err(), "55P03");
}

fn gate() -> Gate {
    Gate {
        reached: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    }
}

/// A pool of one connection on the test database, without the test pool's idle reaper: after a
/// dropped operation, its next connection shows whether the old one was closed or kept.
async fn single(pool: &PgPool) -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap()
}

async fn holder(pool: &PgPool) -> (PgConnection, i32) {
    let mut conn = PgConnection::connect_with(&pool.connect_options())
        .await
        .unwrap();
    let pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    (conn, pid)
}

#[sqlx::test(migrations = "./migrations")]
async fn upload_creates_input_version_one_with_hash(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let tenant = tenant(&pool, &f).await;
    let dir = TestDir::new();
    let blobs = dir.store();
    let bytes = "รายงานการประชุม 2568".as_bytes();
    let view = upload(
        &pool,
        &blobs,
        &tenant,
        SpaceId(f.space),
        "ประชุม/วาระ.txt",
        bytes,
    )
    .await;
    assert_eq!(view.latest.sha256, sha256(bytes));
    assert_eq!(view.latest.version, 1);
    assert_eq!(view.latest.bytes, bytes.len() as i64);
    assert_eq!(view.latest.artifact_id, view.id);
    assert_eq!(view.latest.media_type, MediaType::Text);
    assert_eq!(view.latest.gridcraft_check, None);
    assert_eq!(view.role, ArtifactRole::Input);
    assert_eq!(view.space_id, SpaceId(f.space));
    assert_eq!(view.name, "ประชุม/วาระ.txt");
    assert_eq!(view.display, view.name);
    let (stored, version, user, kat, job): (String, i32, Option<Uuid>, Option<Uuid>, Option<Uuid>) =
        sqlx::query_as(
            "SELECT v.sha256, v.version, a.created_by_user, a.created_by_kat, a.job_id
             FROM artifacts a JOIN artifact_versions v ON v.artifact_id = a.id WHERE a.id = $1",
        )
        .bind(view.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, sha256(bytes));
    assert_eq!(version, 1);
    assert_eq!((user, kat, job), (Some(f.user), None, None));
    let blob = BlobRef {
        workspace_id: WorkspaceId(f.workspace),
        sha256: sha256(bytes),
        bytes: bytes.len() as u64,
    };
    assert_eq!(std::fs::read(blob_path(&dir.root(), &blob)).unwrap(), bytes);
    assert!(tmp_files(&dir.root(), WorkspaceId(f.workspace)).is_empty());
    let json = serde_json::to_value(&view).unwrap();
    for key in ["id", "space_id", "name", "role", "display", "latest"] {
        assert!(json.get(key).is_some(), "ArtifactView lacks {key}");
    }
    assert_eq!(json["role"], "input");
    assert_eq!(json["latest"]["media_type"], "text");
    assert!(json["latest"]["gridcraft_check"].is_null());
}

#[sqlx::test(migrations = "./migrations")]
async fn same_bytes_share_one_blob_within_a_workspace_only(pool: PgPool) {
    let a = Fixture::new(&pool).await;
    let b = Fixture::new(&pool).await;
    let (ta, tb) = (tenant(&pool, &a).await, tenant(&pool, &b).await);
    let dir = TestDir::new();
    let blobs = dir.store();
    let bytes = b"same bytes, three uploads";
    let one = upload(&pool, &blobs, &ta, SpaceId(a.space), "one.txt", bytes).await;
    let two = upload(&pool, &blobs, &ta, SpaceId(a.space), "two.txt", bytes).await;
    let other = upload(&pool, &blobs, &tb, SpaceId(b.space), "one.txt", bytes).await;
    assert_ne!(one.id, two.id);
    assert_eq!(one.latest.sha256, two.latest.sha256);
    assert_eq!(one.latest.sha256, other.latest.sha256);
    let in_a = blob_files(&dir.root(), WorkspaceId(a.workspace));
    let in_b = blob_files(&dir.root(), WorkspaceId(b.workspace));
    assert_eq!(in_a.len(), 1);
    assert_eq!(in_b.len(), 1);
    assert_ne!(in_a, in_b);
    assert_eq!(std::fs::read(&in_a[0]).unwrap(), bytes);
    assert_eq!(std::fs::read(&in_b[0]).unwrap(), bytes);
}

#[tokio::test]
async fn dropped_staging_leaves_no_blob() {
    let dir = TestDir::new();
    let blobs = dir.store();
    let workspace = WorkspaceId::new();
    let mut staging = blobs.stage(workspace, 1024).await.unwrap();
    staging.write(b"half of a file").await.unwrap();
    assert_eq!(tmp_files(&dir.root(), workspace).len(), 1);
    drop(staging);
    assert!(tmp_files(&dir.root(), workspace).is_empty());
    let staged = blobs.stage_bytes(workspace, b"a whole file").await.unwrap();
    assert_eq!(tmp_files(&dir.root(), workspace).len(), 1);
    drop(staged);
    assert!(tmp_files(&dir.root(), workspace).is_empty());
    assert!(blob_files(&dir.root(), workspace).is_empty());
}

#[tokio::test]
async fn read_detects_tampered_blob() {
    let dir = TestDir::new();
    let blobs = dir.store();
    let workspace = WorkspaceId::new();
    let blob = blobs
        .commit(blobs.stage_bytes(workspace, b"original").await.unwrap())
        .await
        .unwrap();
    assert_eq!(blobs.read(&blob, 1024).await.unwrap(), b"original");
    std::fs::write(blob_path(&dir.root(), &blob), b"tampered").unwrap();
    assert!(matches!(
        blobs.read(&blob, 1024).await,
        Err(KatError::BlobCorrupt)
    ));
}

#[tokio::test]
async fn read_refuses_more_than_max_bytes() {
    let dir = TestDir::new();
    let blobs = dir.store();
    let blob = blobs
        .commit(
            blobs
                .stage_bytes(WorkspaceId::new(), b"ten bytes!")
                .await
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(blobs.read(&blob, 10).await.unwrap(), b"ten bytes!");
    assert!(matches!(
        blobs.read(&blob, 9).await,
        Err(KatError::BodyTooLarge)
    ));
}

#[tokio::test]
async fn read_detects_a_truncated_or_grown_blob() {
    let dir = TestDir::new();
    let blobs = dir.store();
    let blob = blobs
        .commit(
            blobs
                .stage_bytes(WorkspaceId::new(), b"twelve bytes")
                .await
                .unwrap(),
        )
        .await
        .unwrap();
    let path = blob_path(&dir.root(), &blob);
    std::fs::write(&path, b"twelve").unwrap();
    assert!(matches!(
        blobs.read(&blob, 1024).await,
        Err(KatError::BlobCorrupt)
    ));
    std::fs::write(&path, b"twelve bytes and more").unwrap();
    assert!(matches!(
        blobs.read(&blob, 1024).await,
        Err(KatError::BlobCorrupt)
    ));
}

#[tokio::test]
async fn read_of_a_missing_blob_is_not_found() {
    let dir = TestDir::new();
    let blobs = dir.store();
    let blob = blobs
        .commit(
            blobs
                .stage_bytes(WorkspaceId::new(), b"short-lived")
                .await
                .unwrap(),
        )
        .await
        .unwrap();
    std::fs::remove_file(blob_path(&dir.root(), &blob)).unwrap();
    assert!(matches!(
        blobs.read(&blob, 1024).await,
        Err(KatError::NotFound)
    ));
    assert!(matches!(blobs.open(&blob).await, Err(KatError::NotFound)));
    // A missing file is no error for a removal.
    blobs.remove(&blob).await.unwrap();
}

#[tokio::test]
async fn open_streams_the_checked_bytes_and_refuses_a_tampered_blob() {
    let dir = TestDir::new();
    let blobs = dir.store();
    let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let blob = blobs
        .commit(blobs.stage_bytes(WorkspaceId::new(), &bytes).await.unwrap())
        .await
        .unwrap();
    let mut stream = blobs.open(&blob).await.unwrap();
    let mut read = Vec::new();
    while let Some(chunk) = stream.next().await {
        read.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(read, bytes);
    let mut tampered = bytes.clone();
    tampered[150_000] ^= 1;
    std::fs::write(blob_path(&dir.root(), &blob), &tampered).unwrap();
    assert!(matches!(
        blobs.open(&blob).await,
        Err(KatError::BlobCorrupt)
    ));
}

#[tokio::test]
async fn reupload_of_the_same_bytes_replaces_a_tampered_blob() {
    let dir = TestDir::new();
    let blobs = dir.store();
    let workspace = WorkspaceId::new();
    let blob = blobs
        .commit(
            blobs
                .stage_bytes(workspace, b"the real bytes")
                .await
                .unwrap(),
        )
        .await
        .unwrap();
    std::fs::write(blob_path(&dir.root(), &blob), b"a forged copy!").unwrap();
    assert!(matches!(
        blobs.read(&blob, 1024).await,
        Err(KatError::BlobCorrupt)
    ));
    let again = blobs
        .commit(
            blobs
                .stage_bytes(workspace, b"the real bytes")
                .await
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(again, blob);
    assert_eq!(blobs.read(&blob, 1024).await.unwrap(), b"the real bytes");
    assert_eq!(blob_files(&dir.root(), workspace).len(), 1);
    assert!(tmp_files(&dir.root(), workspace).is_empty());
}

#[tokio::test]
async fn blob_paths_reject_non_hex_digests() {
    let dir = TestDir::new();
    let blobs = dir.store();
    let workspace = WorkspaceId::new();
    let good = sha256(b"x");
    let mut bad = vec![
        "..".to_owned(),
        "../../../../etc/passwd".to_owned(),
        good.to_uppercase(),
        good[..63].to_owned(),
        format!("{good}0"),
        format!("{}\0", &good[..63]),
        format!("{}/", &good[..63]),
        format!("{}g", &good[..63]),
        String::new(),
    ];
    bad.push(format!("../{}", &good[..61]));
    for digest in &bad {
        let blob = BlobRef {
            workspace_id: workspace,
            sha256: digest.clone(),
            bytes: 1,
        };
        assert!(
            matches!(blobs.read(&blob, 1024).await, Err(KatError::RequestInvalid)),
            "read accepted {digest:?}"
        );
        assert!(
            matches!(blobs.open(&blob).await, Err(KatError::RequestInvalid)),
            "open accepted {digest:?}"
        );
        assert!(
            matches!(blobs.remove(&blob).await, Err(KatError::RequestInvalid)),
            "remove accepted {digest:?}"
        );
        let mut staged = blobs.stage_bytes(workspace, b"x").await.unwrap();
        staged.sha256 = digest.clone();
        assert!(
            matches!(blobs.commit(staged).await, Err(KatError::RequestInvalid)),
            "commit accepted {digest:?}"
        );
    }
    // A well-formed digest that is not the staged file's own is refused as well.
    let mut staged = blobs.stage_bytes(workspace, b"x").await.unwrap();
    staged.sha256 = sha256(b"y");
    assert!(matches!(
        blobs.commit(staged).await,
        Err(KatError::BlobCorrupt)
    ));
    assert!(blob_files(&dir.root(), workspace).is_empty());
    assert!(tmp_files(&dir.root(), workspace).is_empty());
}

#[tokio::test]
async fn staged_and_committed_files_are_mode_0600() {
    let dir = TestDir::new();
    let blobs = dir.store();
    let workspace = WorkspaceId::new();
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let mut staging = blobs.stage(workspace, 64).await.unwrap();
    staging.write(b"private").await.unwrap();
    let staged = staging.finish().await.unwrap();
    let temp = tmp_files(&dir.root(), workspace);
    assert_eq!(temp.len(), 1);
    assert_eq!(mode(&temp[0]), 0o600);
    let blob = blobs.commit(staged).await.unwrap();
    let path = blob_path(&dir.root(), &blob);
    assert_eq!(mode(&path), 0o600);
    for directory in path.ancestors().skip(1).take(4) {
        assert_eq!(mode(directory), 0o700, "{}", directory.display());
    }
}

#[test]
fn local_store_new_refuses_an_unusable_root() {
    let dir = TestDir::new();
    let file = dir.0.join("a-file");
    std::fs::write(&file, b"not a directory").unwrap();
    for root in [
        file.join("artifacts"),
        PathBuf::from("relative/artifacts"),
        // Exists, but nobody may create a file in it.
        PathBuf::from("/proc"),
    ] {
        assert!(
            matches!(
                LocalBlobStore::new(root.clone()),
                Err(KatError::ConfigInvalid {
                    var: "KATWORK_ARTIFACT_DIR"
                })
            ),
            "{}",
            root.display()
        );
    }
}

#[test]
fn local_store_new_creates_the_root_and_leaves_no_probe() {
    let dir = TestDir::new();
    let root = dir.0.join("nested").join("artifacts");
    LocalBlobStore::new(root.clone()).unwrap();
    assert!(root.is_dir());
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    assert_eq!(
        std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    // An existing, writable root is accepted again.
    LocalBlobStore::new(root.clone()).unwrap();
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
}

#[test]
fn blob_lock_key_vectors() {
    let first = WorkspaceId(Uuid::parse_str("01010101-0101-0101-0101-010101010101").unwrap());
    assert_eq!(
        store::blob_lock_key(first, &"ab".repeat(32)),
        -8_432_138_188_340_477_958
    );
    let second = WorkspaceId(Uuid::parse_str("018f0000-0000-7000-8000-000000000001").unwrap());
    assert_eq!(
        store::blob_lock_key(second, &sha256(b"abc")),
        -2_095_407_870_157_117_111
    );
}

#[tokio::test]
async fn free_bytes_is_positive_for_a_temp_dir() {
    let dir = TestDir::new();
    assert!(dir.store().free_bytes().await.unwrap() > 0);
}

#[tokio::test]
async fn staging_accepts_the_cap_and_fails_one_byte_more() {
    let dir = TestDir::new();
    let blobs = dir.store();
    let workspace = WorkspaceId::new();
    let mut staging = blobs.stage(workspace, 10).await.unwrap();
    staging.write(b"01234").await.unwrap();
    staging.write(b"56789").await.unwrap();
    assert!(matches!(
        staging.write(b"!").await,
        Err(KatError::BodyTooLarge)
    ));
    drop(staging);
    let mut staging = blobs.stage(workspace, 10).await.unwrap();
    staging.write(b"0123456789").await.unwrap();
    let staged = staging.finish().await.unwrap();
    assert_eq!(staged.bytes, 10);
    assert_eq!(staged.sha256, sha256(b"0123456789"));
    let mut staging = blobs.stage(workspace, 10).await.unwrap();
    assert!(matches!(
        staging.write(b"0123456789!").await,
        Err(KatError::BodyTooLarge)
    ));
    let mut empty = blobs.stage(workspace, 0).await.unwrap();
    assert!(matches!(
        empty.write(b"x").await,
        Err(KatError::BodyTooLarge)
    ));
    assert_eq!(empty.finish().await.unwrap().bytes, 0);
}

#[tokio::test]
async fn head_collects_the_first_eight_bytes_across_chunks() {
    let dir = TestDir::new();
    let blobs = dir.store();
    let workspace = WorkspaceId::new();
    let bytes = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3 more of the file";
    let mut staging = blobs.stage(workspace, 1024).await.unwrap();
    for byte in bytes {
        staging.write(&[*byte]).await.unwrap();
    }
    let staged = staging.finish().await.unwrap();
    assert_eq!(staged.head, b"%PDF-1.7");
    assert_eq!(staged.bytes, bytes.len() as u64);
    assert_eq!(staged.sha256, sha256(bytes));
    let mut staging = blobs.stage(workspace, 1024).await.unwrap();
    staging.write(b"%PD").await.unwrap();
    staging.write(b"").await.unwrap();
    staging.write(b"F-1.7 and the rest").await.unwrap();
    assert_eq!(staging.finish().await.unwrap().head, b"%PDF-1.7");
    let short = blobs.stage_bytes(workspace, b"PK").await.unwrap();
    assert_eq!(short.head, b"PK");
}

#[tokio::test]
async fn utf8_probe_survives_chunk_boundaries() {
    let dir = TestDir::new();
    let blobs = dir.store();
    let workspace = WorkspaceId::new();
    let thai = "รายได้".as_bytes().to_vec();
    assert_eq!(thai.len(), 18);
    let utf16: Vec<u8> = [0xFF, 0xFE]
        .into_iter()
        .chain("รายได้".encode_utf16().flat_map(u16::to_le_bytes))
        .collect();
    let utf16_be: Vec<u8> = [0xFE, 0xFF]
        .into_iter()
        .chain("รายได้".encode_utf16().flat_map(u16::to_be_bytes))
        .collect();
    let mut with_nul = thai.clone();
    with_nul.insert(9, 0);
    let mut bom_utf8 = vec![0xEF, 0xBB, 0xBF];
    bom_utf8.extend_from_slice(&thai);
    let cases: Vec<(Vec<u8>, TextProbe)> = vec![
        (thai.clone(), TextProbe::Utf8),
        (with_nul, TextProbe::Other),
        (utf16, TextProbe::Utf16Bom),
        (utf16_be, TextProbe::Utf16Bom),
        (vec![0xFF, 0xFE], TextProbe::Utf16Bom),
        (bom_utf8, TextProbe::Utf8),
        ([&thai[..], &[0xE0, 0xB8]].concat(), TextProbe::Other),
        (vec![0xE0, 0xB8], TextProbe::Other),
        (vec![0xC0, 0x80], TextProbe::Other),
        (vec![0xED, 0xA0, 0x80], TextProbe::Other),
        (vec![0xF4, 0x90, 0x80, 0x80], TextProbe::Other),
        (vec![0xF0, 0x9F, 0x98, 0x80], TextProbe::Utf8),
        (vec![0xFF], TextProbe::Other),
        (vec![0xCA, 0xC7, 0xD1, 0xCA, 0xB4, 0xD5], TextProbe::Other),
        (Vec::new(), TextProbe::Utf8),
    ];
    for (bytes, expected) in cases {
        let mut staging = blobs.stage(workspace, 1024).await.unwrap();
        for byte in &bytes {
            staging.write(&[*byte]).await.unwrap();
        }
        assert_eq!(
            staging.finish().await.unwrap().text,
            expected,
            "byte by byte: {bytes:02X?}"
        );
        assert_eq!(
            blobs.stage_bytes(workspace, &bytes).await.unwrap().text,
            expected,
            "one chunk: {bytes:02X?}"
        );
        for split in 1..bytes.len() {
            let mut staging = blobs.stage(workspace, 1024).await.unwrap();
            staging.write(&bytes[..split]).await.unwrap();
            staging.write(&bytes[split..]).await.unwrap();
            assert_eq!(
                staging.finish().await.unwrap().text,
                expected,
                "split at {split}: {bytes:02X?}"
            );
        }
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn create_input_refused_by_the_schema_leaves_no_blob(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let tenant = tenant(&pool, &f).await;
    let dir = TestDir::new();
    let blobs = dir.store();
    let staged = blobs
        .stage_bytes(tenant.workspace_id(), b"refused")
        .await
        .unwrap();
    // A Space that does not exist: the artifact's foreign key refuses the insert.
    let result = store::create_input(
        &pool,
        &blobs,
        &tenant,
        SpaceId::new(),
        "refused.txt",
        MediaType::Text,
        staged,
        LOCK,
    )
    .await;
    let Err(KatError::Database(error)) = result else {
        panic!("{result:?}");
    };
    support::sqlstate(error, "23503");
    assert!(blob_files(&dir.root(), tenant.workspace_id()).is_empty());
    assert!(tmp_files(&dir.root(), tenant.workspace_id()).is_empty());
}

#[sqlx::test(migrations = "./migrations")]
async fn input_artifact_never_gets_second_version(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let tenant = tenant(&pool, &f).await;
    let dir = TestDir::new();
    let blobs = dir.store();
    let input = upload(&pool, &blobs, &tenant, SpaceId(f.space), "in.txt", b"one").await;
    let result = new_version(
        &pool,
        &blobs,
        &ctx(&f, &tenant),
        input.id,
        b"two",
        "input-version",
        None,
    )
    .await;
    assert!(matches!(result, Err(KatError::ArtifactImmutable)));
    assert_eq!(versions_of(&pool, input.id).await, 1);
}

async fn versions_of(pool: &PgPool, artifact: ArtifactId) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM artifact_versions WHERE artifact_id = $1")
        .bind(artifact)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Another Space of the fixture's workspace.
async fn another_space(pool: &PgPool, f: &Fixture) -> SpaceId {
    let space = SpaceId::new();
    sqlx::query(
        "INSERT INTO spaces (id, workspace_id, name, allow_external) VALUES ($1, $2, 'Other', false)",
    )
    .bind(space)
    .bind(f.workspace)
    .execute(pool)
    .await
    .unwrap();
    space
}

/// A second job on the fixture's thread, so in the fixture's Space.
async fn another_job(pool: &PgPool, f: &Fixture) -> JobId {
    let message = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO messages (id, workspace_id, thread_id, author_kind, author_user_id, body) VALUES ($1, $2, $3, 'user', $4, 'Again')",
    )
    .bind(message)
    .bind(f.workspace)
    .bind(f.thread)
    .bind(f.user)
    .execute(pool)
    .await
    .unwrap();
    let job = JobId::new();
    sqlx::query(
        "INSERT INTO jobs (id, workspace_id, thread_id, request_message_id, requested_by) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(job)
    .bind(f.workspace)
    .bind(f.thread)
    .bind(message)
    .bind(f.user)
    .execute(pool)
    .await
    .unwrap();
    job
}

#[sqlx::test(migrations = "./migrations")]
async fn new_output_version_of_another_jobs_or_spaces_file_is_not_found(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let tenant = tenant(&pool, &f).await;
    let dir = TestDir::new();
    let blobs = dir.store();
    let writer = ctx(&f, &tenant);
    let other_space = another_space(&pool, &f).await;
    // Another job's output, in the writer's Space.
    let other_job = OutputCtx {
        job_id: another_job(&pool, &f).await,
        ..ctx(&f, &tenant)
    };
    let theirs = output(
        &pool,
        &blobs,
        &other_job,
        "theirs.docx",
        b"theirs",
        "theirs-1",
    )
    .await
    .unwrap();
    // The writer's own job's output in another Space: only the Space check refuses it.
    let elsewhere = OutputCtx {
        space_id: other_space,
        ..ctx(&f, &tenant)
    };
    let away = output(&pool, &blobs, &elsewhere, "away.docx", b"away", "away-1")
        .await
        .unwrap();
    // An input of another Space: not_found, never artifact_immutable, which would confirm it.
    let foreign_input = upload(&pool, &blobs, &tenant, other_space, "in.txt", b"in").await;
    for (artifact, key) in [
        (theirs.artifact_id, "theirs-2"),
        (away.artifact_id, "away-2"),
        (foreign_input.id, "foreign-input-2"),
    ] {
        let result = new_version(&pool, &blobs, &writer, artifact, b"v2", key, None).await;
        assert!(
            matches!(result, Err(KatError::NotFound)),
            "{key}: {result:?}"
        );
        assert_eq!(versions_of(&pool, artifact).await, 1, "{key}");
    }
    // In the writer's own Space an input stays artifact_immutable, whatever its job, and the
    // writer's own output takes version 2.
    let own_input = upload(&pool, &blobs, &tenant, SpaceId(f.space), "in.txt", b"own").await;
    let result = new_version(
        &pool,
        &blobs,
        &writer,
        own_input.id,
        b"v2",
        "own-in-2",
        None,
    )
    .await;
    assert!(
        matches!(result, Err(KatError::ArtifactImmutable)),
        "{result:?}"
    );
    let mine = output(&pool, &blobs, &writer, "mine.docx", b"mine", "mine-1")
        .await
        .unwrap();
    let second = new_version(
        &pool,
        &blobs,
        &writer,
        mine.artifact_id,
        b"v2",
        "mine-2",
        None,
    )
    .await
    .unwrap();
    assert_eq!((second.artifact_id, second.version), (mine.artifact_id, 2));
}

#[sqlx::test(migrations = "./migrations")]
async fn output_versions_are_dense_under_concurrency(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let tenant = tenant(&pool, &f).await;
    let dir = TestDir::new();
    let blobs = dir.store();
    let ctx = ctx(&f, &tenant);
    let first = output(&pool, &blobs, &ctx, "minutes.docx", b"v1", "out-1")
        .await
        .unwrap();
    assert_eq!(first.version, 1);
    let paused = gate();
    let (first_pid, first_pid_rx) = tokio::sync::oneshot::channel();
    let (second_pid, _second_pid_rx) = tokio::sync::oneshot::channel();
    let mut one = Box::pin(pause_after_first_lock(
        paused.clone(),
        new_version(
            &pool,
            &blobs,
            &ctx,
            first.artifact_id,
            b"v2-a",
            "out-2a",
            Some(first_pid),
        ),
    ));
    let mut two = Box::pin(new_version(
        &pool,
        &blobs,
        &ctx,
        first.artifact_id,
        b"v2-b",
        "out-2b",
        Some(second_pid),
    ));
    tokio::select! {
        result = &mut one => panic!("the first writer did not pause: {result:?}"),
        () = paused.reached.notified() => {}
    }
    let holder = first_pid_rx.await.unwrap();
    tokio::select! {
        result = &mut two => panic!("the second writer did not wait for the row lock: {result:?}"),
        _ = locks::wait_until_blocked_on(&pool, holder, "artifacts") => {}
    }
    paused.release.notify_one();
    let (one, two) = tokio::join!(one, two);
    let mut versions = vec![one.unwrap().version, two.unwrap().version];
    versions.sort_unstable();
    assert_eq!(versions, [2, 3]);
}

#[sqlx::test(migrations = "./migrations")]
async fn versioning_a_deleted_output_is_not_found(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let tenant = tenant(&pool, &f).await;
    let dir = TestDir::new();
    let blobs = dir.store();
    let ctx = ctx(&f, &tenant);
    let first = output(&pool, &blobs, &ctx, "minutes.docx", b"v1", "out-1")
        .await
        .unwrap();
    mark_deleted(&pool, first.artifact_id, f.user).await;
    let locked = new_version(&pool, &blobs, &ctx, first.artifact_id, b"v2", "out-2", None).await;
    assert!(matches!(locked, Err(KatError::NotFound)));
    let blob = blobs
        .commit(
            blobs
                .stage_bytes(tenant.workspace_id(), b"v2")
                .await
                .unwrap(),
        )
        .await
        .unwrap();
    let unlocked = db::guarded(&pool, async |conn| {
        store::new_output_version_on(
            conn,
            &ctx,
            first.artifact_id,
            MediaType::Docx,
            &blob,
            None,
            "out-2",
        )
        .await
    })
    .await;
    assert!(matches!(unlocked, Err(KatError::NotFound)));
}

#[sqlx::test(migrations = "./migrations")]
async fn output_version_idempotency_key_returns_stored_version(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let tenant = tenant(&pool, &f).await;
    let dir = TestDir::new();
    let blobs = dir.store();
    let ctx = ctx(&f, &tenant);
    let first = output(&pool, &blobs, &ctx, "minutes.docx", b"v1", "out-1")
        .await
        .unwrap();
    let replay = output(&pool, &blobs, &ctx, "minutes.docx", b"v1", "out-1")
        .await
        .unwrap();
    assert_eq!(replay, first);
    let second = new_version(&pool, &blobs, &ctx, first.artifact_id, b"v2", "out-2", None)
        .await
        .unwrap();
    assert_eq!(second.version, 2);
    let again = new_version(&pool, &blobs, &ctx, first.artifact_id, b"v2", "out-2", None)
        .await
        .unwrap();
    assert_eq!(again, second);
    let counts: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM artifacts), (SELECT count(*) FROM artifact_versions)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(counts, (1, 2));
    // A stored key of another artifact is a conflict, never that artifact's version.
    let other = output(&pool, &blobs, &ctx, "other.docx", b"o1", "other-1")
        .await
        .unwrap();
    let crossed = new_version(&pool, &blobs, &ctx, other.artifact_id, b"v2", "out-2", None).await;
    assert!(matches!(crossed, Err(KatError::RequestInvalid)));
}

#[sqlx::test(migrations = "./migrations")]
async fn idempotency_key_of_another_workspace_is_never_returned(pool: PgPool) {
    let a = Fixture::new(&pool).await;
    let b = Fixture::new(&pool).await;
    let (ta, tb) = (tenant(&pool, &a).await, tenant(&pool, &b).await);
    let dir = TestDir::new();
    let blobs = dir.store();
    let theirs = output(&pool, &blobs, &ctx(&b, &tb), "b.docx", b"b", "shared-key")
        .await
        .unwrap();
    let ours = output(&pool, &blobs, &ctx(&a, &ta), "a.docx", b"a", "shared-key").await;
    assert!(ours.is_err(), "another workspace's version was returned");
    let crossed = new_version(
        &pool,
        &blobs,
        &ctx(&a, &ta),
        theirs.artifact_id,
        b"x",
        "shared-key",
        None,
    )
    .await;
    assert!(matches!(crossed, Err(KatError::NotFound)));
    // Called directly, without the lock, the key still finds nothing outside the workspace.
    let ours = ctx(&a, &ta);
    let blob = blobs
        .commit(blobs.stage_bytes(ta.workspace_id(), b"x").await.unwrap())
        .await
        .unwrap();
    let direct = db::guarded(&pool, async |conn| {
        store::new_output_version_on(
            conn,
            &ours,
            theirs.artifact_id,
            MediaType::Docx,
            &blob,
            None,
            "shared-key",
        )
        .await
    })
    .await;
    assert!(matches!(direct, Err(KatError::NotFound)), "{direct:?}");
    let in_a: i64 = sqlx::query_scalar("SELECT count(*) FROM artifacts WHERE workspace_id = $1")
        .bind(a.workspace)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(in_a, 0);
}

#[sqlx::test(migrations = "./migrations")]
async fn create_output_rejects_an_invalid_name(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let tenant = tenant(&pool, &f).await;
    let dir = TestDir::new();
    let blobs = dir.store();
    let ctx = ctx(&f, &tenant);
    for (index, name) in ["../x.docx", "a\u{202E}b.docx", "", "a//b.docx"]
        .into_iter()
        .enumerate()
    {
        let result = output(&pool, &blobs, &ctx, name, b"x", &format!("bad-{index}")).await;
        assert!(
            matches!(result, Err(KatError::FileNameInvalid)),
            "{name:?}: {result:?}"
        );
    }
    let stored = output(&pool, &blobs, &ctx, "e\u{0301}.docx", b"x", "good")
        .await
        .unwrap();
    let name: String = sqlx::query_scalar("SELECT name FROM artifacts WHERE id = $1")
        .bind(stored.artifact_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "\u{e9}.docx");
}

#[test]
fn file_names_table() {
    let segment = |n: usize| "a".repeat(n);
    let thai = |n: usize| "ก".repeat(n);
    let invalid = [
        "../a".to_owned(),
        "/a".to_owned(),
        "a/".to_owned(),
        "a//b".to_owned(),
        "a\\b".to_owned(),
        "a\u{0}b".to_owned(),
        "a\u{7}b".to_owned(),
        "a\tb".to_owned(),
        "a\u{202E}b".to_owned(),
        "a\u{200B}b".to_owned(),
        "a\u{FEFF}b".to_owned(),
        "a/b/c/d/e/f/g/h/i".to_owned(),
        segment(256),
        thai(86),
        ".".to_owned(),
        "..".to_owned(),
        "a/./b".to_owned(),
        "a/../b".to_owned(),
        String::new(),
        " a.txt".to_owned(),
        "a.txt ".to_owned(),
        "a/ b.txt".to_owned(),
        "a\u{00A0}".to_owned(),
        "\u{3000}a".to_owned(),
        vec![segment(64); 8].join("/"),
    ];
    for name in &invalid {
        assert!(
            matches!(names::validate_name(name), Err(KatError::FileNameInvalid)),
            "{name:?} was accepted"
        );
    }
    // Beyond Cc and Cf: line and paragraph separators, U+FFFD and Unicode's other
    // default-ignorable code points, which most fonts draw as nothing. A listing prefix refuses
    // them too.
    for hidden in [
        '\u{2028}',
        '\u{2029}',
        '\u{FFFD}',
        '\u{034F}',
        '\u{115F}',
        '\u{1160}',
        '\u{17B4}',
        '\u{180B}',
        '\u{180F}',
        '\u{2065}',
        '\u{3164}',
        '\u{FE00}',
        '\u{FE0F}',
        '\u{FFA0}',
        '\u{FFF0}',
        '\u{E0000}',
        '\u{E0100}',
        '\u{E0FFF}',
    ] {
        for name in [format!("a{hidden}b.txt"), format!("{hidden}.txt")] {
            assert!(
                matches!(names::validate_name(&name), Err(KatError::FileNameInvalid)),
                "{name:?} was accepted"
            );
            assert!(
                matches!(
                    names::validate_prefix(&name),
                    Err(KatError::FileNameInvalid)
                ),
                "prefix {name:?} was accepted"
            );
        }
    }
    let unchanged = [
        "ประชุม 2568-10/วาระ.docx".to_owned(),
        "a/b/c/d/e/f/g/h.txt".to_owned(),
        segment(255),
        thai(85),
        vec![segment(63); 8].join("/"),
        "น้ำ.txt".to_owned(),
        ".pdf".to_owned(),
        "a b.txt".to_owned(),
        "q1 (ร่าง).xlsx".to_owned(),
        // Next to the hidden characters: visible ones that stay allowed.
        "\u{1F4C4} notes.txt".to_owned(),
        "\u{3131}\u{314F}.txt".to_owned(),
    ];
    for name in &unchanged {
        assert_eq!(names::validate_name(name).unwrap(), *name);
    }
    for (raw, normalized) in [
        ("e\u{0301}.txt", "\u{e9}.txt"),
        ("ก\u{0E48}\u{0E38}.txt", "ก\u{0E38}\u{0E48}.txt"),
    ] {
        assert_eq!(names::validate_name(raw).unwrap(), normalized);
    }
    // Lengths count the normalized form: 300 bytes decomposed, 200 composed.
    let decomposed = "e\u{0301}".repeat(100);
    assert_eq!(decomposed.len(), 300);
    assert_eq!(names::validate_name(&decomposed).unwrap().len(), 200);
}

fn staged_text(bytes: &[u8]) -> TextProbe {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let dir = TestDir::new();
            dir.store()
                .stage_bytes(WorkspaceId::new(), bytes)
                .await
                .unwrap()
                .text
        })
}

#[test]
fn media_sniff_table() {
    let pdf = b"%PDF-1.7".as_slice();
    let zip = b"PK\x03\x04\x14\x00\x06\x00".as_slice();
    let png = b"\x89PNG\r\n\x1a\n".as_slice();
    let jpeg = b"\xff\xd8\xff\xe0\x00\x10JF".as_slice();
    let mz = b"MZ\x90\x00\x03\x00\x00\x00".as_slice();
    let utf16: Vec<u8> = [0xFF, 0xFE]
        .into_iter()
        .chain("วาระการประชุม".encode_utf16().flat_map(u16::to_le_bytes))
        .collect();
    // "สวัสดี" in TIS-620.
    let tis620 = [0xCA, 0xC7, 0xD1, 0xCA, 0xB4, 0xD5];
    use TextProbe::*;
    let supported: [(&str, &[u8], TextProbe, MediaType); 16] = [
        ("a.pdf", pdf, Other, MediaType::Pdf),
        ("A.PDF", pdf, Utf8, MediaType::Pdf),
        ("folder/.pdf", pdf, Other, MediaType::Pdf),
        ("a.tar.pdf", pdf, Other, MediaType::Pdf),
        ("a.docx", zip, Other, MediaType::Docx),
        ("a.xlsx", zip, Other, MediaType::Xlsx),
        ("a.png", png, Other, MediaType::Png),
        ("a.JPG", jpeg, Other, MediaType::Jpeg),
        ("a.jpeg", jpeg, Other, MediaType::Jpeg),
        ("a.txt", b"", Utf8, MediaType::Text),
        ("a.txt", b"notes", staged_text(b"notes"), MediaType::Text),
        ("a.md", b"# head", Utf8, MediaType::Markdown),
        ("a.CSV", b"a,b", Utf8, MediaType::Csv),
        ("a.txt", &utf16[..8], staged_text(&utf16), MediaType::Text),
        ("a.csv", &utf16[..8], Utf16Bom, MediaType::Csv),
        ("a.md", &[0xEF, 0xBB, 0xBF, b'#'], Utf8, MediaType::Markdown),
    ];
    for (name, head, text, expected) in supported {
        assert_eq!(
            media::sniff(name, head, text).unwrap(),
            expected,
            "{name} {head:02X?}"
        );
    }
    let unsupported: [(&str, &[u8], TextProbe); 13] = [
        ("a.docx", pdf, Other),
        ("a.pdf", mz, Other),
        ("a.exe", mz, Other),
        ("a.exe", pdf, Other),
        ("a.pdf.exe", pdf, Other),
        ("a.png", &png[..7], Other),
        ("a.png", jpeg, Other),
        ("a.jpg", png, Other),
        ("a", pdf, Utf8),
        ("a.", pdf, Utf8),
        ("a.pdf", b"", Utf8),
        ("a.html", b"<html>", Utf8),
        ("a.zip", zip, Other),
    ];
    for (name, head, text) in unsupported {
        assert!(
            matches!(
                media::sniff(name, head, text),
                Err(KatError::FileTypeUnsupported)
            ),
            "{name} {head:02X?}"
        );
    }
    for (name, head, text) in [
        ("a.txt", &tis620[..], staged_text(&tis620)),
        ("a.txt", zip, Other),
        ("a.csv", b"a\0b", staged_text(b"a\0b")),
    ] {
        assert!(
            matches!(
                media::sniff(name, head, text),
                Err(KatError::FileEncodingUnsupported)
            ),
            "{name} {head:02X?}"
        );
    }
}

#[test]
fn media_type_wire_and_mime() {
    let all = [
        (MediaType::Pdf, "pdf", "application/pdf"),
        (
            MediaType::Docx,
            "docx",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        ),
        (
            MediaType::Xlsx,
            "xlsx",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        ),
        (MediaType::Text, "text", "text/plain"),
        (MediaType::Markdown, "markdown", "text/markdown"),
        (MediaType::Csv, "csv", "text/csv"),
        (MediaType::Png, "png", "image/png"),
        (MediaType::Jpeg, "jpeg", "image/jpeg"),
    ];
    for (media, wire, mime) in all {
        assert_eq!(media.wire(), wire);
        assert_eq!(MediaType::from_wire(wire), Some(media));
        assert_eq!(media.mime(), mime);
        assert_eq!(serde_json::to_value(media).unwrap(), wire);
    }
    assert_eq!(MediaType::from_wire("exe"), None);
    assert_eq!(ArtifactRole::Input.wire(), "input");
    assert_eq!(ArtifactRole::Output.wire(), "output");
    for (check, wire) in [
        (GridcraftCheck::Passed, "passed"),
        (GridcraftCheck::Unavailable, "unavailable"),
        (GridcraftCheck::Off, "off"),
    ] {
        assert_eq!(check.wire(), wire);
        assert_eq!(serde_json::to_value(check).unwrap(), wire);
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn version_lookup_across_workspaces_or_spaces_is_not_found(pool: PgPool) {
    let a = Fixture::new(&pool).await;
    let b = Fixture::new(&pool).await;
    let (ta, tb) = (tenant(&pool, &a).await, tenant(&pool, &b).await);
    let dir = TestDir::new();
    let blobs = dir.store();
    let file = upload(&pool, &blobs, &ta, SpaceId(a.space), "a.txt", b"a").await;
    let (found, version) = store::version(&pool, &ta, file.id, 1).await.unwrap();
    assert_eq!(found, file);
    assert_eq!(version, file.latest);
    assert!(matches!(
        store::version(&pool, &tb, file.id, 1).await,
        Err(KatError::NotFound)
    ));
    let other_space = SpaceId::new();
    sqlx::query(
        "INSERT INTO spaces (id, workspace_id, name, allow_external) VALUES ($1, $2, 'Other', false)",
    )
    .bind(other_space)
    .bind(a.workspace)
    .execute(&pool)
    .await
    .unwrap();
    let file_id = file.id;
    let in_space = |space: SpaceId, tenant: Tenant, version: Option<i32>| {
        let pool = pool.clone();
        async move {
            db::guarded(&pool, async |conn| {
                store::version_in_space_on(conn, &tenant, space, file_id, version).await
            })
            .await
        }
    };
    assert_eq!(
        in_space(SpaceId(a.space), ta.clone(), None)
            .await
            .unwrap()
            .1,
        file.latest
    );
    assert_eq!(
        in_space(SpaceId(a.space), ta.clone(), Some(1))
            .await
            .unwrap()
            .1,
        file.latest
    );
    for (space, tenant, version) in [
        (other_space, ta.clone(), None),
        (other_space, ta.clone(), Some(1)),
        (SpaceId(a.space), ta.clone(), Some(2)),
        (SpaceId(b.space), tb.clone(), None),
        (SpaceId(a.space), tb.clone(), None),
    ] {
        assert!(matches!(
            in_space(space, tenant, version).await,
            Err(KatError::NotFound)
        ));
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn deleted_artifacts_are_not_found_and_not_listed(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let tenant = tenant(&pool, &f).await;
    let dir = TestDir::new();
    let blobs = dir.store();
    let space = SpaceId(f.space);
    let kept = upload(&pool, &blobs, &tenant, space, "kept.txt", b"kept").await;
    let gone = upload(&pool, &blobs, &tenant, space, "gone.txt", b"gone").await;
    mark_deleted(&pool, gone.id, f.user).await;
    let listed = store::list(&pool, &tenant, space, None, 500).await.unwrap();
    assert_eq!(listed, vec![kept.clone()]);
    let paged = db::guarded(&pool, async |conn| {
        store::list_page(conn, &tenant, space, None, None, 500).await
    })
    .await
    .unwrap();
    assert_eq!(paged, vec![kept]);
    assert!(matches!(
        store::version(&pool, &tenant, gone.id, 1).await,
        Err(KatError::NotFound)
    ));
    let in_space = db::guarded(&pool, async |conn| {
        store::version_in_space_on(conn, &tenant, space, gone.id, None).await
    })
    .await;
    assert!(matches!(in_space, Err(KatError::NotFound)));
    let locked = db::guarded(&pool, async |conn| {
        let mut tx = conn.begin().await?;
        store::lock_source_on(&mut tx, &tenant, &[gone.id]).await
    })
    .await;
    assert!(matches!(locked, Err(KatError::NotFound)));
}

#[sqlx::test(migrations = "./migrations")]
async fn list_page_resumes_after_name_and_id(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let tenant = tenant(&pool, &f).await;
    let dir = TestDir::new();
    let blobs = dir.store();
    let space = SpaceId(f.space);
    let first = upload(&pool, &blobs, &tenant, space, "a.txt", b"first").await;
    let second = upload(&pool, &blobs, &tenant, space, "a.txt", b"second").await;
    let unique = upload(&pool, &blobs, &tenant, space, "b.txt", b"unique").await;
    assert!(first.id < second.id);
    // The earlier id gets the later upload time: created_at order is the reverse of id order.
    for (id, at) in [
        (first.id, "2026-03-01 01:00:00+07"),
        (second.id, "2026-01-15 12:00:00+00"),
    ] {
        sqlx::query("UPDATE artifacts SET created_at = $2::timestamptz WHERE id = $1")
            .bind(id)
            .bind(at)
            .execute(&pool)
            .await
            .unwrap();
    }
    let mut seen = Vec::new();
    let mut after: Option<(String, ArtifactId)> = None;
    loop {
        let page = db::guarded(&pool, async |conn| {
            store::list_page(conn, &tenant, space, None, after.clone(), 1).await
        })
        .await
        .unwrap();
        assert!(page.len() <= 1);
        let Some(last) = page.last() else {
            break;
        };
        after = Some((last.name.clone(), last.id));
        seen.extend(page);
    }
    let ids: Vec<ArtifactId> = seen.iter().map(|view| view.id).collect();
    assert_eq!(ids, [first.id, second.id, unique.id]);
    let displays: Vec<&str> = seen.iter().map(|view| view.display.as_str()).collect();
    // Upload dates are UTC: 2026-03-01 01:00 at +07 is 2026-02-28.
    assert_eq!(
        displays,
        ["a.txt · 2026-02-28", "a.txt · 2026-01-15", "b.txt"]
    );
    let listed = store::list(&pool, &tenant, space, None, 500).await.unwrap();
    assert_eq!(listed, seen);
    // The display looks at the whole Space, not the page: each page of one row still knows.
    let after_first = db::guarded(&pool, async |conn| {
        store::list_page(
            conn,
            &tenant,
            space,
            Some("a"),
            Some(("a.txt".to_owned(), first.id)),
            10,
        )
        .await
    })
    .await
    .unwrap();
    assert_eq!(after_first.len(), 1);
    assert_eq!(after_first[0].display, "a.txt · 2026-01-15");
    // Once one of the two is deleted, the other is shown by its bare name.
    mark_deleted(&pool, second.id, f.user).await;
    let listed = store::list(&pool, &tenant, space, None, 500).await.unwrap();
    let displays: Vec<&str> = listed.iter().map(|view| view.display.as_str()).collect();
    assert_eq!(displays, ["a.txt", "b.txt"]);
}

#[sqlx::test(migrations = "./migrations")]
async fn list_filters_by_name_prefix(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let tenant = tenant(&pool, &f).await;
    let dir = TestDir::new();
    let blobs = dir.store();
    let space = SpaceId(f.space);
    for name in [
        "a/x.txt", "a/y.txt", "ab.txt", "b/z.txt", "a%b.txt", "a_b.txt",
    ] {
        upload(&pool, &blobs, &tenant, space, name, name.as_bytes()).await;
    }
    let names = |prefix: &'static str| {
        let pool = pool.clone();
        let tenant = tenant.clone();
        async move {
            store::list(&pool, &tenant, space, Some(prefix), 500)
                .await
                .unwrap()
                .into_iter()
                .map(|view| view.name)
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(names("a/").await, ["a/x.txt", "a/y.txt"]);
    // `%` and `_` are plain characters of a prefix, never patterns.
    assert_eq!(names("a%").await, ["a%b.txt"]);
    assert_eq!(names("a_").await, ["a_b.txt"]);
    assert!(names("c/").await.is_empty());
    assert!(matches!(
        store::list(&pool, &tenant, space, Some(&"a".repeat(513)), 500).await,
        Err(KatError::FileNameInvalid)
    ));
    assert!(matches!(
        store::list(&pool, &tenant, space, Some("a\u{0}"), 500).await,
        Err(KatError::FileNameInvalid)
    ));
    let limited = store::list(&pool, &tenant, space, None, 2).await.unwrap();
    assert_eq!(limited.len(), 2);
    assert!(matches!(
        store::list(&pool, &tenant, SpaceId::new(), None, 500).await,
        Err(KatError::NotFound)
    ));
}

#[sqlx::test(migrations = "./migrations")]
async fn create_input_waits_for_the_blob_lock(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let tenant = tenant(&pool, &f).await;
    let dir = TestDir::new();
    let blobs = dir.store();
    let staged = blobs
        .stage_bytes(tenant.workspace_id(), b"locked bytes")
        .await
        .unwrap();
    let key = store::blob_lock_key(tenant.workspace_id(), &staged.sha256);
    let (mut holder, holder_pid) = holder(&pool).await;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(key)
        .execute(&mut holder)
        .await
        .unwrap();
    let mut create = Box::pin(store::create_input(
        &pool,
        &blobs,
        &tenant,
        SpaceId(f.space),
        "locked.txt",
        MediaType::Text,
        staged,
        Duration::from_secs(30),
    ));
    tokio::select! {
        result = &mut create => panic!("create_input passed the held blob lock: {result:?}"),
        _ = locks::wait_until_blocked_on(&pool, holder_pid, "artifacts") => {}
    }
    assert!(futures::poll!(create.as_mut()).is_pending());
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM artifacts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(key)
        .execute(&mut holder)
        .await
        .unwrap();
    let view = create.await.unwrap();
    assert_eq!(view.latest.sha256, sha256(b"locked bytes"));
    holder.close().await.unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn writer_locks_follow_one_ascending_pass(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let tenant = tenant(&pool, &f).await;
    let dir = TestDir::new();
    let blobs = dir.store();
    let space = SpaceId(f.space);
    let a = upload(&pool, &blobs, &tenant, space, "a.txt", b"a")
        .await
        .id;
    let b = output(&pool, &blobs, &ctx(&f, &tenant), "b.docx", b"b", "b-1")
        .await
        .unwrap()
        .artifact_id;
    let c = upload(&pool, &blobs, &tenant, space, "c.txt", b"c")
        .await
        .id;
    assert!(a < b && b < c);
    let (mut holder, holder_pid) = holder(&pool).await;
    let mut held = holder.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM artifacts WHERE id = $1 FOR NO KEY UPDATE")
        .bind(c)
        .execute(&mut *held)
        .await
        .unwrap();
    let mut writer = Box::pin(db::guarded(&pool, async |conn| {
        let mut tx = conn.begin().await?;
        db::set_lock_timeout(&mut tx, Duration::from_secs(30)).await?;
        store::lock_artifacts_on(&mut tx, &tenant, Some(b), &[c, a, c]).await?;
        tx.rollback().await?;
        Ok(())
    }));
    tokio::select! {
        result = &mut writer => panic!("the writer passed the held row: {result:?}"),
        _ = locks::wait_until_blocked_on(&pool, holder_pid, "artifacts") => {}
    }
    // Row locks live in the rows, so pg_locks cannot show them: probe with NOWAIT.
    probe(&pool, a, "FOR SHARE NOWAIT").await.unwrap();
    lock_not_available(probe(&pool, a, "FOR NO KEY UPDATE NOWAIT").await);
    lock_not_available(probe(&pool, b, "FOR SHARE NOWAIT").await);
    probe(&pool, b, "FOR KEY SHARE NOWAIT").await.unwrap();
    held.rollback().await.unwrap();
    writer.await.unwrap();
    // The output named as a source too is still held FOR NO KEY UPDATE.
    db::guarded(&pool, async |conn| {
        let mut tx = conn.begin().await?;
        store::lock_artifacts_on(&mut tx, &tenant, Some(b), &[b]).await?;
        lock_not_available(probe(&pool, b, "FOR SHARE NOWAIT").await);
        probe(&pool, b, "FOR KEY SHARE NOWAIT").await.unwrap();
        tx.rollback().await?;
        Ok(())
    })
    .await
    .unwrap();
    mark_deleted(&pool, c, f.user).await;
    let deleted = db::guarded(&pool, async |conn| {
        let mut tx = conn.begin().await?;
        store::lock_artifacts_on(&mut tx, &tenant, Some(b), &[a, c]).await
    })
    .await;
    assert!(matches!(deleted, Err(KatError::CitationUnknown)));
    holder.close().await.unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn own_output_as_source_never_upgrades_a_lock(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let tenant = tenant(&pool, &f).await;
    let dir = TestDir::new();
    let blobs = dir.store();
    let b = output(&pool, &blobs, &ctx(&f, &tenant), "b.docx", b"b", "b-1")
        .await
        .unwrap()
        .artifact_id;
    let writer = |pid: tokio::sync::oneshot::Sender<i32>| {
        let pool = pool.clone();
        let tenant = tenant.clone();
        async move {
            db::guarded(&pool, async |conn| {
                let mut tx = conn.begin().await?;
                db::set_lock_timeout(&mut tx, Duration::from_secs(30)).await?;
                let own: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                    .fetch_one(&mut *tx)
                    .await?;
                pid.send(own).unwrap();
                store::lock_artifacts_on(&mut tx, &tenant, Some(b), &[b]).await?;
                tx.commit().await?;
                Ok(())
            })
            .await
        }
    };
    let (first_gate, second_gate) = (gate(), gate());
    let (first_pid, first_pid_rx) = tokio::sync::oneshot::channel();
    let (second_pid, _second_pid_rx) = tokio::sync::oneshot::channel();
    let mut one = Box::pin(pause_after_first_lock(
        first_gate.clone(),
        writer(first_pid),
    ));
    let mut two = Box::pin(pause_after_first_lock(
        second_gate.clone(),
        writer(second_pid),
    ));
    tokio::select! {
        result = &mut one => panic!("the first writer did not pause: {result:?}"),
        () = first_gate.reached.notified() => {}
    }
    let first_pid = first_pid_rx.await.unwrap();
    tokio::select! {
        result = &mut two => panic!("the second writer ended early: {result:?}"),
        () = second_gate.reached.notified() => {}
        _ = locks::wait_until_blocked_on(&pool, first_pid, "artifacts") => {}
    }
    first_gate.release.notify_one();
    second_gate.release.notify_one();
    let (one, two) = tokio::join!(one, two);
    for result in [one, two] {
        if let Err(KatError::Database(error)) = &result {
            assert_ne!(
                error.as_database_error().and_then(|e| e.code()).as_deref(),
                Some("40P01"),
                "a lock was upgraded into a deadlock"
            );
        }
        result.unwrap();
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn lock_waits_for_a_concurrent_delete_then_sees_it(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let tenant = tenant(&pool, &f).await;
    let dir = TestDir::new();
    let blobs = dir.store();
    let file = upload(&pool, &blobs, &tenant, SpaceId(f.space), "a.txt", b"a").await;
    let (mut deleter, deleter_pid) = holder(&pool).await;
    let mut deleting = deleter.begin().await.unwrap();
    sqlx::query("UPDATE artifacts SET deleted_at = now(), deleted_by = $2 WHERE id = $1")
        .bind(file.id)
        .bind(f.user)
        .execute(&mut *deleting)
        .await
        .unwrap();
    let mut reader = Box::pin(db::guarded(&pool, async |conn| {
        let mut tx = conn.begin().await?;
        db::set_lock_timeout(&mut tx, Duration::from_secs(30)).await?;
        store::lock_source_on(&mut tx, &tenant, &[file.id]).await
    }));
    tokio::select! {
        result = &mut reader => panic!("the reader did not wait for the delete: {result:?}"),
        _ = locks::wait_until_blocked_on(&pool, deleter_pid, "artifacts") => {}
    }
    deleting.commit().await.unwrap();
    assert!(matches!(reader.await, Err(KatError::NotFound)));
    deleter.close().await.unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn foreign_or_missing_source_is_citation_unknown_and_output_is_not_found(pool: PgPool) {
    let a = Fixture::new(&pool).await;
    let b = Fixture::new(&pool).await;
    let (ta, tb) = (tenant(&pool, &a).await, tenant(&pool, &b).await);
    let dir = TestDir::new();
    let blobs = dir.store();
    let own = output(&pool, &blobs, &ctx(&a, &ta), "own.docx", b"o", "own-1")
        .await
        .unwrap()
        .artifact_id;
    let input = upload(&pool, &blobs, &ta, SpaceId(a.space), "in.txt", b"i")
        .await
        .id;
    let foreign = upload(&pool, &blobs, &tb, SpaceId(b.space), "theirs.txt", b"t")
        .await
        .id;
    let missing = ArtifactId::new();
    let lock = |output: Option<ArtifactId>, sources: Vec<ArtifactId>| {
        let pool = pool.clone();
        let tenant = ta.clone();
        async move {
            db::guarded(&pool, async |conn| {
                let mut tx = conn.begin().await?;
                store::lock_artifacts_on(&mut tx, &tenant, output, &sources).await
            })
            .await
        }
    };
    lock(Some(own), vec![input]).await.unwrap();
    lock(None, vec![input, own]).await.unwrap();
    for output in [missing, foreign] {
        assert!(matches!(
            lock(Some(output), vec![input]).await,
            Err(KatError::NotFound)
        ));
    }
    for source in [missing, foreign] {
        assert!(matches!(
            lock(Some(own), vec![input, source]).await,
            Err(KatError::CitationUnknown)
        ));
        assert!(matches!(
            lock(None, vec![source]).await,
            Err(KatError::CitationUnknown)
        ));
        let read = db::guarded(&pool, async |conn| {
            let mut tx = conn.begin().await?;
            store::lock_source_on(&mut tx, &ta, &[input, source]).await
        })
        .await;
        assert!(matches!(read, Err(KatError::NotFound)));
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn lock_sources_for_pass_returns_deleted_ids_and_holds_for_share(pool: PgPool) {
    let a = Fixture::new(&pool).await;
    let b = Fixture::new(&pool).await;
    let (ta, tb) = (tenant(&pool, &a).await, tenant(&pool, &b).await);
    let dir = TestDir::new();
    let blobs = dir.store();
    let space = SpaceId(a.space);
    let live = upload(&pool, &blobs, &ta, space, "live.txt", b"l").await.id;
    let deleted = upload(&pool, &blobs, &ta, space, "deleted.txt", b"d")
        .await
        .id;
    let foreign = upload(&pool, &blobs, &tb, SpaceId(b.space), "f.txt", b"f")
        .await
        .id;
    mark_deleted(&pool, deleted, a.user).await;
    db::guarded(&pool, async |conn| {
        let mut tx = conn.begin().await?;
        let found =
            store::lock_sources_for_pass_on(&mut tx, &ta, &[deleted, live, deleted]).await?;
        assert_eq!(found, BTreeSet::from([deleted]));
        for id in [live, deleted] {
            probe(&pool, id, "FOR SHARE NOWAIT").await.unwrap();
            lock_not_available(probe(&pool, id, "FOR NO KEY UPDATE NOWAIT").await);
        }
        tx.rollback().await?;
        Ok(())
    })
    .await
    .unwrap();
    for id in [foreign, ArtifactId::new()] {
        let result = db::guarded(&pool, async |conn| {
            let mut tx = conn.begin().await?;
            store::lock_sources_for_pass_on(&mut tx, &ta, &[live, id]).await
        })
        .await;
        assert!(matches!(result, Err(KatError::NotFound)));
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn workspace_bytes_sums_versions_is_zero_when_empty_and_skips_deleted_files(pool: PgPool) {
    let a = Fixture::new(&pool).await;
    let b = Fixture::new(&pool).await;
    let (ta, tb) = (tenant(&pool, &a).await, tenant(&pool, &b).await);
    let dir = TestDir::new();
    let blobs = dir.store();
    let used = |workspace: Uuid| {
        let pool = pool.clone();
        async move {
            db::guarded(&pool, async |conn| {
                store::workspace_bytes_on(conn, WorkspaceId(workspace)).await
            })
            .await
            .unwrap()
        }
    };
    assert_eq!(used(a.workspace).await, 0);
    upload(&pool, &blobs, &ta, SpaceId(a.space), "three.txt", b"abc").await;
    let five = upload(&pool, &blobs, &ta, SpaceId(a.space), "five.txt", b"abcde").await;
    let ctx = ctx(&a, &ta);
    let out = output(&pool, &blobs, &ctx, "out.docx", b"seven!!", "out-1")
        .await
        .unwrap();
    new_version(
        &pool,
        &blobs,
        &ctx,
        out.artifact_id,
        b"eleven bytes",
        "out-2",
        None,
    )
    .await
    .unwrap();
    // The same bytes again are counted again: the quota counts versions, not blobs.
    upload(
        &pool,
        &blobs,
        &ta,
        SpaceId(a.space),
        "three-again.txt",
        b"abc",
    )
    .await;
    upload(
        &pool,
        &blobs,
        &tb,
        SpaceId(b.space),
        "theirs.txt",
        b"other workspace",
    )
    .await;
    assert_eq!(used(a.workspace).await, 3 + 5 + 7 + 12 + 3);
    mark_deleted(&pool, five.id, a.user).await;
    assert_eq!(used(a.workspace).await, 3 + 7 + 12 + 3);
    assert_eq!(used(b.workspace).await, 15);
}

#[sqlx::test(migrations = "./migrations")]
async fn gridcraft_check_is_null_for_inputs_and_rejected_on_non_xlsx(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let tenant = tenant(&pool, &f).await;
    let dir = TestDir::new();
    let blobs = dir.store();
    let input = upload(&pool, &blobs, &tenant, SpaceId(f.space), "in.txt", b"x").await;
    let check: Option<String> =
        sqlx::query_scalar("SELECT gridcraft_check FROM artifact_versions WHERE artifact_id = $1")
            .bind(input.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(check, None);
    let ctx = ctx(&f, &tenant);
    let staged = blobs
        .stage_bytes(tenant.workspace_id(), b"PK\x03\x04")
        .await
        .unwrap();
    let xlsx = db::guarded(&pool, async |conn| {
        let mut tx = conn.begin().await?;
        let blob = store::commit_blob_on(&mut tx, &blobs, staged).await?;
        let version = store::create_output_on(
            &mut tx,
            &ctx,
            "budget.xlsx",
            MediaType::Xlsx,
            &blob,
            Some(GridcraftCheck::Off),
            "xlsx-1",
        )
        .await?;
        tx.commit().await?;
        Ok(version)
    })
    .await
    .unwrap();
    assert_eq!(xlsx.gridcraft_check, Some(GridcraftCheck::Off));
    let docx = sqlx::query(
        "INSERT INTO artifact_versions
         (id, workspace_id, artifact_id, version, sha256, bytes, media_type, run_id, gridcraft_check)
         VALUES ($1, $2, $3, 2, $4, 1, 'docx', $5, 'off')",
    )
    .bind(Uuid::now_v7())
    .bind(f.workspace)
    .bind(xlsx.artifact_id)
    .bind(sha256(b"x"))
    .bind(f.run)
    .execute(&pool)
    .await
    .unwrap_err();
    support::sqlstate(docx, "23514");
    let staged = blobs
        .stage_bytes(tenant.workspace_id(), b"PK\x03\x04 docx")
        .await
        .unwrap();
    let refused = db::guarded(&pool, async |conn| {
        let mut tx = conn.begin().await?;
        let blob = store::commit_blob_on(&mut tx, &blobs, staged).await?;
        store::create_output_on(
            &mut tx,
            &ctx,
            "minutes.docx",
            MediaType::Docx,
            &blob,
            Some(GridcraftCheck::Off),
            "docx-1",
        )
        .await
    })
    .await;
    assert!(refused.is_err());
}

#[sqlx::test(migrations = "./migrations")]
async fn artifact_author_check_rejects_mixed_authors(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let insert = |role: &'static str, user: Option<Uuid>, kat: Option<Uuid>, job: Option<Uuid>| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "INSERT INTO artifacts (id, workspace_id, space_id, name, role, created_by_user, created_by_kat, job_id)
                 VALUES ($1, $2, $3, 'x.txt', $4, $5, $6, $7)",
            )
            .bind(Uuid::now_v7())
            .bind(f.workspace)
            .bind(f.space)
            .bind(role)
            .bind(user)
            .bind(kat)
            .bind(job)
            .execute(&pool)
            .await
            .map(|_| ())
        }
    };
    insert("input", Some(f.user), None, None).await.unwrap();
    insert("output", None, Some(f.kat), Some(f.job))
        .await
        .unwrap();
    for (role, user, kat, job) in [
        ("input", Some(f.user), Some(f.kat), None),
        ("input", None, None, None),
        ("output", None, Some(f.kat), None),
        ("output", None, None, Some(f.job)),
        ("other", Some(f.user), None, None),
    ] {
        support::sqlstate(insert(role, user, kat, job).await.unwrap_err(), "23514");
    }
    for name in ["", &"a".repeat(513)] {
        let error = sqlx::query(
            "INSERT INTO artifacts (id, workspace_id, space_id, name, role, created_by_user)
             VALUES ($1, $2, $3, $4, 'input', $5)",
        )
        .bind(Uuid::now_v7())
        .bind(f.workspace)
        .bind(f.space)
        .bind(name)
        .bind(f.user)
        .execute(&pool)
        .await
        .unwrap_err();
        support::sqlstate(error, "23514");
    }
}

async fn insert_artifact(
    pool: &PgPool,
    workspace: Uuid,
    space: Uuid,
    user: Option<Uuid>,
    kat: Option<Uuid>,
    job: Option<Uuid>,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    let role = if kat.is_some() { "output" } else { "input" };
    sqlx::query(
        "INSERT INTO artifacts (id, workspace_id, space_id, name, role, created_by_user, created_by_kat, job_id)
         VALUES ($1, $2, $3, 'f.txt', $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(workspace)
    .bind(space)
    .bind(role)
    .bind(user)
    .bind(kat)
    .bind(job)
    .execute(pool)
    .await?;
    Ok(id)
}

async fn insert_version(
    pool: &PgPool,
    workspace: Uuid,
    artifact: Uuid,
    version: i32,
    run: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO artifact_versions (id, workspace_id, artifact_id, version, sha256, bytes, media_type, run_id)
         VALUES ($1, $2, $3, $4, $5, 1, 'text', $6)",
    )
    .bind(Uuid::now_v7())
    .bind(workspace)
    .bind(artifact)
    .bind(version)
    .bind("0".repeat(64))
    .bind(run)
    .execute(pool)
    .await
    .map(|_| ())
}

/// Tenant isolation lives in the schema: every reference 0011 adds is a composite key with the
/// row's workspace, so no row can point into another workspace.
#[sqlx::test(migrations = "./migrations")]
async fn composite_keys_keep_every_reference_in_its_workspace(pool: PgPool) {
    let a = Fixture::new(&pool).await;
    let b = Fixture::new(&pool).await;
    // Each reference inside workspace a is accepted...
    let input = insert_artifact(&pool, a.workspace, a.space, Some(a.user), None, None)
        .await
        .unwrap();
    let output = insert_artifact(&pool, a.workspace, a.space, None, Some(a.kat), Some(a.job))
        .await
        .unwrap();
    insert_version(&pool, a.workspace, input, 1, None)
        .await
        .unwrap();
    insert_version(&pool, a.workspace, output, 1, Some(a.run))
        .await
        .unwrap();
    // ...and the same reference into workspace b is a foreign-key violation.
    let theirs = insert_artifact(&pool, b.workspace, b.space, Some(b.user), None, None)
        .await
        .unwrap();
    let refused = [
        (
            "space",
            insert_artifact(&pool, a.workspace, b.space, Some(a.user), None, None)
                .await
                .map(|_| ()),
        ),
        (
            "kat",
            insert_artifact(&pool, a.workspace, a.space, None, Some(b.kat), Some(a.job))
                .await
                .map(|_| ()),
        ),
        (
            "job",
            insert_artifact(&pool, a.workspace, a.space, None, Some(a.kat), Some(b.job))
                .await
                .map(|_| ()),
        ),
        (
            "artifact",
            insert_version(&pool, a.workspace, theirs, 1, None).await,
        ),
        (
            "run",
            insert_version(&pool, a.workspace, output, 2, Some(b.run)).await,
        ),
    ];
    for (reference, result) in refused {
        let error = result.expect_err(reference);
        support::sqlstate(error, "23503");
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn deleted_columns_are_set_together(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let tenant = tenant(&pool, &f).await;
    let dir = TestDir::new();
    let blobs = dir.store();
    let file = upload(&pool, &blobs, &tenant, SpaceId(f.space), "a.txt", b"a").await;
    for set in ["deleted_at = now()", "deleted_by = $2"] {
        let error = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE artifacts SET {set} WHERE id = $1 AND $2::uuid IS NOT NULL"
        )))
        .bind(file.id)
        .bind(f.user)
        .execute(&pool)
        .await
        .unwrap_err();
        support::sqlstate(error, "23514");
    }
    mark_deleted(&pool, file.id, f.user).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn artifact_id_order_matches_postgres_uuid_order(pool: PgPool) {
    let mut ids: Vec<ArtifactId> = Vec::new();
    for first in [0x00u8, 0x01, 0x7f, 0x80, 0xfe, 0xff] {
        for last in [0x00u8, 0x80, 0xff] {
            let mut bytes = [0x5au8; 16];
            bytes[0] = first;
            bytes[15] = last;
            ids.push(ArtifactId(Uuid::from_bytes(bytes)));
            bytes[8] = last;
            ids.push(ArtifactId(Uuid::from_bytes(bytes)));
        }
    }
    ids.extend((0..20).map(|_| ArtifactId::new()));
    ids.reverse();
    let raw: Vec<Uuid> = ids.iter().map(|id| id.0).collect();
    let ordered: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM unnest($1::uuid[]) AS ids(id) ORDER BY id")
            .bind(&raw)
            .fetch_all(&pool)
            .await
            .unwrap();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(
        sorted.into_iter().map(|id| id.0).collect::<Vec<_>>(),
        ordered
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn create_input_cancelled_during_its_statement_leaves_pool_clean(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let tenant = tenant(&pool, &f).await;
    let dir = TestDir::new();
    let blobs = dir.store();
    let staged = blobs
        .stage_bytes(tenant.workspace_id(), b"cancelled upload")
        .await
        .unwrap();
    let single = single(&pool).await;
    cancel_sql::drop_during_guarded_statement(&single, "artifact_versions", || {
        store::create_input(
            &single,
            &blobs,
            &tenant,
            SpaceId(f.space),
            "cancelled.txt",
            MediaType::Text,
            staged,
            LOCK,
        )
    })
    .await;
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM artifacts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    assert_pool_clean(&single).await;
    cancel_sql::probe_write(&single).await;
    single.close().await;
    // The rename happened under the lock before the dropped insert: at most that blob is left.
    assert!(tmp_files(&dir.root(), tenant.workspace_id()).is_empty());
    assert!(blob_files(&dir.root(), tenant.workspace_id()).len() <= 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn drop_during_statement_accepts_a_backend_that_ends_idle(pool: PgPool) {
    // The plan's helper accepts a backend that ends idle: a worker's reviewed drain exception
    // returns its connection to the pool after the statement.
    cancel_sql::drop_during_statement(&pool, "artifacts", || {
        sqlx::query("SELECT count(*) FROM artifacts").fetch_one(&pool)
    })
    .await;
    assert_pool_clean(&pool).await;
    cancel_sql::probe_write(&pool).await;
}

#[sqlx::test(migrations = "./migrations")]
#[should_panic(expected = "stayed connected")]
async fn drop_during_guarded_statement_rejects_an_unguarded_query(pool: PgPool) {
    let single = single(&pool).await;
    cancel_sql::drop_during_guarded_statement(&single, "artifacts", || {
        sqlx::query("SELECT count(*) FROM artifacts").fetch_one(&single)
    })
    .await;
}

#[sqlx::test(migrations = "./migrations")]
#[should_panic(expected = "did not run on the pool it was given")]
async fn drop_during_guarded_statement_rejects_an_op_on_another_pool(pool: PgPool) {
    let single = single(&pool).await;
    cancel_sql::drop_during_guarded_statement(&single, "artifacts", || {
        sqlx::query("SELECT count(*) FROM artifacts").fetch_one(&pool)
    })
    .await;
}

#[sqlx::test(migrations = "./migrations")]
async fn drop_during_guarded_statement_accepts_a_guarded_query(pool: PgPool) {
    let single = single(&pool).await;
    cancel_sql::drop_during_guarded_statement(&single, "artifacts", || {
        db::guarded(&single, async |conn| {
            Ok(sqlx::query("SELECT count(*) FROM artifacts")
                .fetch_one(conn)
                .await?)
        })
    })
    .await;
    assert_pool_clean(&single).await;
    single.close().await;
}

#[sqlx::test(migrations = "./migrations")]
#[should_panic(expected = "never reached artifacts")]
async fn drop_during_statement_fails_fast_when_op_never_reaches_the_table(pool: PgPool) {
    cancel_sql::drop_during_statement(&pool, "artifacts", || async {}).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn wait_until_blocked_on_sees_row_advisory_and_relation_waiters(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (mut holder, holder_pid) = holder(&pool).await;
    let (mut waiter, waiter_pid) = self::holder(&pool).await;
    // A row lock.
    let mut held = holder.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM spaces WHERE id = $1 FOR NO KEY UPDATE")
        .bind(f.space)
        .execute(&mut *held)
        .await
        .unwrap();
    {
        let mut row = Box::pin(
            sqlx::query("SELECT 1 FROM spaces WHERE id = $1 FOR NO KEY UPDATE")
                .bind(f.space)
                .execute(&mut waiter),
        );
        tokio::select! {
            result = &mut row => panic!("the row wait ended: {result:?}"),
            pid = locks::wait_until_blocked_on(&pool, holder_pid, "spaces") => assert_eq!(pid, waiter_pid),
        }
        held.rollback().await.unwrap();
        row.await.unwrap();
    }
    // An advisory lock.
    sqlx::query("SELECT pg_advisory_lock(91)")
        .execute(&mut holder)
        .await
        .unwrap();
    {
        let mut advisory =
            Box::pin(sqlx::query("SELECT pg_advisory_lock(91)").execute(&mut waiter));
        tokio::select! {
            result = &mut advisory => panic!("the advisory wait ended: {result:?}"),
            pid = locks::wait_until_blocked_on(&pool, holder_pid, "spaces") => assert_eq!(pid, waiter_pid),
        }
        sqlx::query("SELECT pg_advisory_unlock(91)")
            .execute(&mut holder)
            .await
            .unwrap();
        advisory.await.unwrap();
    }
    sqlx::query("SELECT pg_advisory_unlock(91)")
        .execute(&mut waiter)
        .await
        .unwrap();
    // A relation lock.
    let mut table = holder.begin().await.unwrap();
    sqlx::query("LOCK TABLE kats IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *table)
        .await
        .unwrap();
    {
        let mut relation =
            Box::pin(sqlx::query("SELECT count(*) FROM kats").fetch_one(&mut waiter));
        tokio::select! {
            result = &mut relation => panic!("the relation wait ended: {result:?}"),
            pid = locks::wait_until_blocked_on(&pool, holder_pid, "kats") => assert_eq!(pid, waiter_pid),
        }
        table.rollback().await.unwrap();
        relation.await.unwrap();
    }
    holder.close().await.unwrap();
    waiter.close().await.unwrap();
}

#[test]
fn rate_check_table() {
    let base = Instant::now();
    let at = |millis: u64| base + Duration::from_millis(millis);
    let mut rate = RateCheck::new(1_000, Duration::from_secs(1));
    rate.observe(at(0), 0).unwrap();
    rate.observe(at(400), 50).unwrap();
    rate.observe(at(1_000), 1_000).unwrap();
    assert!(matches!(
        rate.observe(at(2_000), 1_999),
        Err(KatError::UploadTooSlow)
    ));
    for (later, bytes, ok) in [
        (1_000, 0, false),
        (1_000, 999, false),
        (1_000, 1_000, true),
        (2_500, 2_500, true),
        (2_500, 2_499, false),
        (999, 0, true),
    ] {
        let mut rate = RateCheck::new(1_000, Duration::from_secs(1));
        rate.observe(at(0), 0).unwrap();
        assert_eq!(
            rate.observe(at(later), bytes).is_ok(),
            ok,
            "{bytes} bytes at {later} ms"
        );
    }
    // Each window is checked on its own: a fast first window does not carry a stall.
    let mut rate = RateCheck::new(1_000, Duration::from_secs(1));
    rate.observe(at(0), 0).unwrap();
    rate.observe(at(1_000), 10_000).unwrap();
    assert!(rate.observe(at(2_000), 10_000).is_err());
    // A floor of 0 is off.
    let mut off = RateCheck::new(0, Duration::from_secs(1));
    off.observe(at(0), 0).unwrap();
    off.observe(at(60_000), 0).unwrap();
    // The deadline is the end of the current window.
    let mut rate = RateCheck::new(1_000, Duration::from_secs(1));
    assert_eq!(rate.deadline(), None);
    rate.observe(at(0), 0).unwrap();
    assert_eq!(rate.deadline(), Some(at(1_000)));
    rate.observe(at(1_200), 1_500).unwrap();
    assert_eq!(rate.deadline(), Some(at(2_200)));
}

#[test]
fn check_space_table() {
    const GIB: u64 = 1_073_741_824;
    assert!(uploads::check_space(GIB + 10, 0, 10, 100).is_ok());
    assert!(matches!(
        uploads::check_space(GIB + 9, 0, 10, 100),
        Err(KatError::StorageFull)
    ));
    assert!(uploads::check_space(10 * GIB, 90, 10, 100).is_ok());
    assert!(matches!(
        uploads::check_space(10 * GIB, 91, 10, 100),
        Err(KatError::StorageQuotaExceeded)
    ));
    // The free-space floor comes first.
    assert!(matches!(
        uploads::check_space(GIB + 9, 91, 10, 100),
        Err(KatError::StorageFull)
    ));
    assert!(matches!(
        uploads::check_space(0, 0, 0, 100),
        Err(KatError::StorageFull)
    ));
    assert!(matches!(
        uploads::check_space(u64::MAX, u64::MAX, u64::MAX, u64::MAX),
        Err(KatError::StorageFull)
    ));
    assert!(matches!(
        uploads::check_space(u64::MAX, u64::MAX, 1, u64::MAX),
        Err(KatError::StorageQuotaExceeded)
    ));
    assert!(uploads::check_space(u64::MAX, u64::MAX - 1, 1, u64::MAX).is_ok());
}

fn chunks(
    items: Vec<Result<&'static [u8], std::io::Error>>,
) -> impl futures::Stream<Item = Result<Bytes, std::io::Error>> + Unpin {
    futures::stream::iter(
        items
            .into_iter()
            .map(|item| item.map(Bytes::from_static))
            .collect::<Vec<_>>(),
    )
}

#[tokio::test(start_paused = true)]
async fn stream_body_stops_at_the_total_timeout() {
    let dir = TestDir::new();
    let blobs = dir.store();
    let mut staging = blobs.stage(WorkspaceId::new(), 1024).await.unwrap();
    let body = chunks(vec![Ok(b"a first chunk")]).chain(futures::stream::pending());
    let mut rate = RateCheck::new(0, Duration::from_secs(10));
    let start = Instant::now();
    let result =
        uploads::stream_body(body, &mut staging, &mut rate, Duration::from_secs(300)).await;
    assert!(matches!(result, Err(KatError::UploadTooSlow)));
    assert_eq!(start.elapsed(), Duration::from_secs(300));
}

#[tokio::test(start_paused = true)]
async fn stream_body_checks_the_rate_while_no_chunk_arrives() {
    let dir = TestDir::new();
    let blobs = dir.store();
    let mut staging = blobs.stage(WorkspaceId::new(), 1 << 20).await.unwrap();
    let body = chunks(vec![Ok(&[b'x'; 1024])]).chain(futures::stream::pending());
    let mut rate = RateCheck::new(1_048_576, Duration::from_millis(200));
    let start = Instant::now();
    let result =
        uploads::stream_body(body, &mut staging, &mut rate, Duration::from_secs(300)).await;
    assert!(matches!(result, Err(KatError::UploadTooSlow)));
    assert_eq!(start.elapsed(), Duration::from_millis(200));
}

#[tokio::test]
async fn stream_body_ends_on_a_body_error_or_the_cap_and_keeps_every_byte_otherwise() {
    let dir = TestDir::new();
    let blobs = dir.store();
    let workspace = WorkspaceId::new();
    let mut rate = RateCheck::new(0, Duration::from_secs(10));
    let mut staging = blobs.stage(workspace, 1024).await.unwrap();
    let broken = chunks(vec![
        Ok(b"half"),
        Err(std::io::Error::other("connection reset")),
    ]);
    assert!(matches!(
        uploads::stream_body(broken, &mut staging, &mut rate, Duration::from_secs(300)).await,
        Err(KatError::RequestInvalid)
    ));
    let mut staging = blobs.stage(workspace, 8).await.unwrap();
    let long = chunks(vec![Ok(b"12345"), Ok(b"6789")]);
    assert!(matches!(
        uploads::stream_body(long, &mut staging, &mut rate, Duration::from_secs(300)).await,
        Err(KatError::BodyTooLarge)
    ));
    let mut staging = blobs.stage(workspace, 1024).await.unwrap();
    let whole = chunks(vec![Ok(b"one "), Ok(b""), Ok(b"two")]);
    uploads::stream_body(whole, &mut staging, &mut rate, Duration::from_secs(300))
        .await
        .unwrap();
    let staged = staging.finish().await.unwrap();
    assert_eq!(staged.sha256, sha256(b"one two"));
    assert_eq!(staged.bytes, 7);
}

fn config(artifact_dir: &Path) -> Config {
    let mut config = Config::from_lookup(|name| match name {
        "DATABASE_URL" => Some("postgres://localhost/katwork".into()),
        "KATWORK_SECRET" => Some(Uuid::now_v7().to_string()),
        _ => None,
    })
    .unwrap();
    config.artifact_dir = artifact_dir.to_owned();
    config
}

#[tokio::test]
async fn core_start_fails_when_the_artifact_dir_is_unusable() {
    let dir = TestDir::new();
    let file = dir.0.join("a-file");
    std::fs::write(&file, b"not a directory").unwrap();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://localhost/katwork")
        .unwrap();
    let result = Core::start(pool.clone(), Arc::new(config(&file.join("artifacts"))));
    assert!(matches!(
        result,
        Err(KatError::ConfigInvalid {
            var: "KATWORK_ARTIFACT_DIR"
        })
    ));
    let root = dir.0.join("served");
    let core = Core::start(pool, Arc::new(config(&root))).unwrap();
    assert!(root.is_dir());
    let staged = core
        .blobs
        .stage_bytes(WorkspaceId::new(), b"x")
        .await
        .unwrap();
    assert_eq!(staged.bytes, 1);
}

#[tokio::test]
async fn core_new_touches_no_directory() {
    let dir = TestDir::new();
    let root = dir.0.join("never-created");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://localhost/katwork")
        .unwrap();
    let core = Core::new(pool, Arc::new(config(&root)));
    assert!(!root.exists());
    assert!(matches!(
        core.blobs.stage(WorkspaceId::new(), 1).await,
        Err(KatError::ConfigInvalid {
            var: "KATWORK_ARTIFACT_DIR"
        })
    ));
    assert!(matches!(
        core.blobs.free_bytes().await,
        Err(KatError::ConfigInvalid {
            var: "KATWORK_ARTIFACT_DIR"
        })
    ));
    let core = core.with_blob_store(Arc::new(dir.store()));
    assert!(core.blobs.free_bytes().await.unwrap() > 0);
    assert!(!root.exists());
    // Uploads share one limiter: eight at once, two for each person.
    let user = UserId::new();
    let permits: Vec<_> = (0..2)
        .map(|_| core.uploads.try_acquire(user).unwrap())
        .collect();
    assert!(matches!(
        core.uploads.try_acquire(user),
        Err(KatError::TooManyUploads)
    ));
    drop(permits);
    core.uploads.try_acquire(user).unwrap();
}
