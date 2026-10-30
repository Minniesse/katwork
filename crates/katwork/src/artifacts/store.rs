//! Artifact rows: creating inputs and outputs, listing and finding them, and the primitives of
//! the artifact lock contract (the SP2a plan's Lock order).
//!
//! Every read and write filters by the tenant's workspace and refuses (or leaves out) an
//! artifact whose `deleted_at` is set. Row locks take one row per statement, in ascending id,
//! and trust only what the locking statement itself returns.
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{Connection, PgConnection, PgPool};

use super::{ArtifactRole, BlobRef, BlobStore, GridcraftCheck, MediaType, StagedBlob, names};
use crate::{
    db,
    error::KatError,
    ids::{ArtifactId, ArtifactVersionId, JobId, KatId, RunId, SpaceId, WorkspaceId},
    tenancy::Tenant,
};

/// A file of a Space with its latest version.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ArtifactView {
    pub id: ArtifactId,
    pub space_id: SpaceId,
    pub name: String,
    pub role: ArtifactRole,
    /// The name, or `"<name> · <upload date as YYYY-MM-DD in UTC>"` for an input that shares
    /// its name with another input of the Space.
    pub display: String,
    pub latest: VersionView,
}

#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
pub struct VersionView {
    pub id: ArtifactVersionId,
    pub artifact_id: ArtifactId,
    pub version: i32,
    pub sha256: String,
    pub bytes: i64,
    pub media_type: MediaType,
    pub gridcraft_check: Option<GridcraftCheck>,
    pub created_at: DateTime<Utc>,
}

/// Who writes an output: the run of a Kat's job in a Space.
#[derive(Debug, Clone)]
pub struct OutputCtx {
    pub tenant: Tenant,
    pub space_id: SpaceId,
    pub kat_id: KatId,
    pub job_id: JobId,
    pub run_id: RunId,
}

#[derive(sqlx::FromRow)]
struct ViewRow {
    id: ArtifactId,
    space_id: SpaceId,
    name: String,
    role: ArtifactRole,
    uploaded_at: DateTime<Utc>,
    shares_name: bool,
    version_id: ArtifactVersionId,
    version: i32,
    sha256: String,
    bytes: i64,
    media_type: MediaType,
    gridcraft_check: Option<GridcraftCheck>,
    version_created_at: DateTime<Utc>,
}

impl ViewRow {
    fn into_view(self) -> ArtifactView {
        let display = if self.shares_name {
            format!("{} · {}", self.name, self.uploaded_at.format("%Y-%m-%d"))
        } else {
            self.name.clone()
        };
        ArtifactView {
            id: self.id,
            space_id: self.space_id,
            name: self.name,
            role: self.role,
            display,
            latest: VersionView {
                id: self.version_id,
                artifact_id: self.id,
                version: self.version,
                sha256: self.sha256,
                bytes: self.bytes,
                media_type: self.media_type,
                gridcraft_check: self.gridcraft_check,
                created_at: self.version_created_at,
            },
        }
    }
}

/// Live artifacts of workspace `$1` with their latest versions. Whether an input shares its
/// name is decided over its whole Space, never over a page.
macro_rules! view {
    () => {
        "SELECT a.id, a.space_id, a.name, a.role, a.created_at AS uploaded_at,
                (a.role = 'input' AND EXISTS (
                    SELECT 1 FROM artifacts o
                    WHERE o.workspace_id = a.workspace_id AND o.space_id = a.space_id
                      AND o.name = a.name AND o.id <> a.id AND o.role = 'input'
                      AND o.deleted_at IS NULL)) AS shares_name,
                v.id AS version_id, v.version, v.sha256, v.bytes, v.media_type,
                v.gridcraft_check, v.created_at AS version_created_at
         FROM artifacts a
         JOIN LATERAL (
             SELECT id, version, sha256, bytes, media_type, gridcraft_check, created_at
             FROM artifact_versions
             WHERE artifact_id = a.id AND workspace_id = a.workspace_id
             ORDER BY version DESC LIMIT 1) v ON true
         WHERE a.workspace_id = $1 AND a.deleted_at IS NULL"
    };
}

/// The advisory lock key of a blob: the first 8 bytes, big-endian, of SHA-256 over the
/// workspace id's 16 bytes followed by the digest's ASCII hex.
pub fn blob_lock_key(workspace: WorkspaceId, sha256: &str) -> i64 {
    let digest = Sha256::new()
        .chain_update(workspace.0.as_bytes())
        .chain_update(sha256.as_bytes())
        .finalize();
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest[..8]);
    i64::from_be_bytes(first)
}

/// Commits a staged blob inside the caller's guarded transaction, under
/// `pg_advisory_xact_lock(blob_lock_key(..))`, so a removal of the same bytes never interleaves.
/// The caller has set its lock timeout; the rename is the only file I/O in the transaction.
pub async fn commit_blob_on(
    conn: &mut PgConnection,
    blobs: &dyn BlobStore,
    staged: StagedBlob,
) -> Result<BlobRef, KatError> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(blob_lock_key(staged.workspace_id, &staged.sha256))
        .execute(&mut *conn)
        .await?;
    blobs.commit(staged).await
}

/// An uploaded file: one guarded transaction inserts the artifact and its version 1, then
/// commits the blob. The rename comes last, so whatever refuses the upload leaves no blob, and
/// only a crash between the rename and the COMMIT can leave an unreferenced one.
#[allow(clippy::too_many_arguments, reason = "The plan's interface")]
pub async fn create_input(
    pool: &PgPool,
    blobs: &dyn BlobStore,
    tenant: &Tenant,
    space_id: SpaceId,
    name: &str,
    media: MediaType,
    staged: StagedBlob,
    lock_timeout: Duration,
) -> Result<ArtifactView, KatError> {
    let name = names::validate_name(name)?;
    if staged.workspace_id != tenant.workspace_id() {
        return Err(KatError::RequestInvalid);
    }
    let id = ArtifactId::new();
    db::guarded(pool, async |conn| {
        let mut tx = conn.begin().await?;
        db::set_lock_timeout(&mut tx, lock_timeout).await?;
        let bytes = i64::try_from(staged.bytes).map_err(|_| KatError::BodyTooLarge)?;
        sqlx::query(
            "INSERT INTO artifacts (id, workspace_id, space_id, name, role, created_by_user)
             VALUES ($1, $2, $3, $4, 'input', $5)",
        )
        .bind(id)
        .bind(tenant.workspace_id())
        .bind(space_id)
        .bind(&name)
        .bind(tenant.user_id())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO artifact_versions (id, workspace_id, artifact_id, version, sha256, bytes, media_type)
             VALUES ($1, $2, $3, 1, $4, $5, $6)",
        )
        .bind(ArtifactVersionId::new())
        .bind(tenant.workspace_id())
        .bind(id)
        .bind(&staged.sha256)
        .bind(bytes)
        .bind(media)
        .execute(&mut *tx)
        .await?;
        commit_blob_on(&mut tx, blobs, staged).await?;
        let (view, _) = version_on(&mut tx, tenant, None, id, None).await?;
        tx.commit().await?;
        Ok(view)
    })
    .await
}

/// A new output and its version 1, inside the caller's transaction (after
/// `guards::authorize_write`). A stored `idempotency_key` returns its version unchanged.
pub async fn create_output_on(
    conn: &mut PgConnection,
    ctx: &OutputCtx,
    name: &str,
    media: MediaType,
    blob: &BlobRef,
    gridcraft: Option<GridcraftCheck>,
    idempotency_key: &str,
) -> Result<VersionView, KatError> {
    let name = names::validate_name(name)?;
    check_write(ctx, media, blob, gridcraft, idempotency_key)?;
    let workspace = ctx.tenant.workspace_id();
    if let Some(stored) = stored_version(conn, workspace, idempotency_key).await? {
        let owner: Option<(ArtifactRole, Option<JobId>)> = sqlx::query_as(
            "SELECT role, job_id FROM artifacts WHERE workspace_id = $1 AND id = $2",
        )
        .bind(workspace)
        .bind(stored.artifact_id)
        .fetch_optional(&mut *conn)
        .await?;
        return match owner {
            Some((ArtifactRole::Output, Some(job))) if job == ctx.job_id => Ok(stored),
            _ => Err(KatError::RequestInvalid),
        };
    }
    let id = ArtifactId::new();
    sqlx::query(
        "INSERT INTO artifacts (id, workspace_id, space_id, name, role, created_by_kat, job_id)
         VALUES ($1, $2, $3, $4, 'output', $5, $6)",
    )
    .bind(id)
    .bind(workspace)
    .bind(ctx.space_id)
    .bind(&name)
    .bind(ctx.kat_id)
    .bind(ctx.job_id)
    .execute(&mut *conn)
    .await?;
    Ok(sqlx::query_as(
        "INSERT INTO artifact_versions
         (id, workspace_id, artifact_id, version, sha256, bytes, media_type, run_id, idempotency_key, gridcraft_check)
         VALUES ($1, $2, $3, 1, $4, $5, $6, $7, $8, $9)
         RETURNING id, artifact_id, version, sha256, bytes, media_type, gridcraft_check, created_at",
    )
    .bind(ArtifactVersionId::new())
    .bind(workspace)
    .bind(id)
    .bind(&blob.sha256)
    .bind(stored_bytes(blob)?)
    .bind(media)
    .bind(ctx.run_id)
    .bind(idempotency_key)
    .bind(gridcraft)
    .fetch_one(&mut *conn)
    .await?)
}

/// The next version of this job's output, inside the caller's transaction, which has locked the
/// artifact row with `lock_artifacts_on` (this takes no lock). A missing, deleted or foreign
/// artifact, or one of another Space whatever its role, is `NotFound`; then an input is
/// `ArtifactImmutable` and another job's output `NotFound`. A stored `idempotency_key` returns
/// its version unchanged.
pub async fn new_output_version_on(
    conn: &mut PgConnection,
    ctx: &OutputCtx,
    artifact_id: ArtifactId,
    media: MediaType,
    blob: &BlobRef,
    gridcraft: Option<GridcraftCheck>,
    idempotency_key: &str,
) -> Result<VersionView, KatError> {
    check_write(ctx, media, blob, gridcraft, idempotency_key)?;
    let workspace = ctx.tenant.workspace_id();
    if let Some(stored) = stored_version(conn, workspace, idempotency_key).await? {
        return if stored.artifact_id == artifact_id {
            Ok(stored)
        } else {
            Err(KatError::RequestInvalid)
        };
    }
    let row: Option<OwnerRow> = sqlx::query_as(
        "SELECT role, job_id, space_id, deleted_at FROM artifacts WHERE workspace_id = $1 AND id = $2",
    )
    .bind(workspace)
    .bind(artifact_id)
    .fetch_optional(&mut *conn)
    .await?;
    // Another Space's file is not_found whatever its role, so a writer learns nothing about it;
    // in the writer's own Space an input is immutable and another job's output not_found.
    let Some(row) = row else {
        return Err(KatError::NotFound);
    };
    if row.deleted_at.is_some() || row.space_id != ctx.space_id {
        return Err(KatError::NotFound);
    }
    if row.role == ArtifactRole::Input {
        return Err(KatError::ArtifactImmutable);
    }
    if row.job_id != Some(ctx.job_id) {
        return Err(KatError::NotFound);
    }
    Ok(sqlx::query_as(
        "INSERT INTO artifact_versions
         (id, workspace_id, artifact_id, version, sha256, bytes, media_type, run_id, idempotency_key, gridcraft_check)
         SELECT $1, $2, $3, COALESCE(MAX(version), 0) + 1, $4, $5, $6, $7, $8, $9
         FROM artifact_versions WHERE workspace_id = $2 AND artifact_id = $3
         RETURNING id, artifact_id, version, sha256, bytes, media_type, gridcraft_check, created_at",
    )
    .bind(ArtifactVersionId::new())
    .bind(workspace)
    .bind(artifact_id)
    .bind(&blob.sha256)
    .bind(stored_bytes(blob)?)
    .bind(media)
    .bind(ctx.run_id)
    .bind(idempotency_key)
    .bind(gridcraft)
    .fetch_one(&mut *conn)
    .await?)
}

#[derive(sqlx::FromRow)]
struct OwnerRow {
    role: ArtifactRole,
    job_id: Option<JobId>,
    space_id: SpaceId,
    deleted_at: Option<DateTime<Utc>>,
}

/// The write's own checks: a usable receipt key, a blob of the writer's workspace, and a
/// GridCraft result only for an XLSX file.
fn check_write(
    ctx: &OutputCtx,
    media: MediaType,
    blob: &BlobRef,
    gridcraft: Option<GridcraftCheck>,
    idempotency_key: &str,
) -> Result<(), KatError> {
    if idempotency_key.is_empty()
        || idempotency_key.contains('\0')
        || blob.workspace_id != ctx.tenant.workspace_id()
        || (gridcraft.is_some() && media != MediaType::Xlsx)
    {
        return Err(KatError::RequestInvalid);
    }
    Ok(())
}

fn stored_bytes(blob: &BlobRef) -> Result<i64, KatError> {
    i64::try_from(blob.bytes).map_err(|_| KatError::BodyTooLarge)
}

/// The version a receipt key already names in this workspace. The key is unique across
/// workspaces, but a lookup by key alone could return another workspace's version.
async fn stored_version(
    conn: &mut PgConnection,
    workspace: WorkspaceId,
    idempotency_key: &str,
) -> Result<Option<VersionView>, KatError> {
    Ok(sqlx::query_as(
        "SELECT id, artifact_id, version, sha256, bytes, media_type, gridcraft_check, created_at
         FROM artifact_versions WHERE workspace_id = $1 AND idempotency_key = $2",
    )
    .bind(workspace)
    .bind(idempotency_key)
    .fetch_optional(conn)
    .await?)
}

/// The Space's live files, ordered by name, then id; `prefix` is a folder or name prefix.
pub async fn list(
    pool: &PgPool,
    tenant: &Tenant,
    space_id: SpaceId,
    prefix: Option<&str>,
    limit: u32,
) -> Result<Vec<ArtifactView>, KatError> {
    db::guarded(pool, async |conn| {
        list_page(conn, tenant, space_id, prefix, None, limit).await
    })
    .await
}

/// `list`, starting strictly after `(name, id)` when `after` is set. The key is unique, so two
/// files that share a name are each listed once across pages. Another workspace's or a missing
/// Space is `NotFound`.
pub async fn list_page(
    conn: &mut PgConnection,
    tenant: &Tenant,
    space_id: SpaceId,
    prefix: Option<&str>,
    after: Option<(String, ArtifactId)>,
    limit: u32,
) -> Result<Vec<ArtifactView>, KatError> {
    let prefix = prefix.map(names::validate_prefix).transpose()?;
    let space: Option<i32> =
        sqlx::query_scalar("SELECT 1 FROM spaces WHERE workspace_id = $1 AND id = $2")
            .bind(tenant.workspace_id())
            .bind(space_id)
            .fetch_optional(&mut *conn)
            .await?;
    if space.is_none() {
        return Err(KatError::NotFound);
    }
    let (after_name, after_id) = after.unzip();
    // `starts_with` compares text, so `%` and `_` in a prefix are never patterns.
    let rows: Vec<ViewRow> = sqlx::query_as(concat!(
        view!(),
        " AND a.space_id = $2 AND ($3::text IS NULL OR starts_with(a.name, $3))
          AND ($4::text IS NULL OR (a.name, a.id) > ($4::text, $5::uuid))
          ORDER BY a.name, a.id LIMIT $6"
    ))
    .bind(tenant.workspace_id())
    .bind(space_id)
    .bind(prefix)
    .bind(after_name)
    .bind(after_id)
    .bind(i64::from(limit))
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(ViewRow::into_view).collect())
}

/// A version of a live artifact of the tenant's workspace, with the artifact.
pub async fn version(
    pool: &PgPool,
    tenant: &Tenant,
    artifact_id: ArtifactId,
    version: i32,
) -> Result<(ArtifactView, VersionView), KatError> {
    db::guarded(pool, async |conn| {
        version_on(conn, tenant, None, artifact_id, Some(version)).await
    })
    .await
}

/// `version` within one Space: another Space's artifact is `NotFound`. `None` is the latest.
pub async fn version_in_space_on(
    conn: &mut PgConnection,
    tenant: &Tenant,
    space_id: SpaceId,
    artifact_id: ArtifactId,
    version: Option<i32>,
) -> Result<(ArtifactView, VersionView), KatError> {
    version_on(conn, tenant, Some(space_id), artifact_id, version).await
}

async fn version_on(
    conn: &mut PgConnection,
    tenant: &Tenant,
    space_id: Option<SpaceId>,
    artifact_id: ArtifactId,
    version: Option<i32>,
) -> Result<(ArtifactView, VersionView), KatError> {
    let row: Option<ViewRow> = sqlx::query_as(concat!(
        view!(),
        " AND a.id = $2 AND ($3::uuid IS NULL OR a.space_id = $3)"
    ))
    .bind(tenant.workspace_id())
    .bind(artifact_id)
    .bind(space_id)
    .fetch_optional(&mut *conn)
    .await?;
    let view = row.ok_or(KatError::NotFound)?.into_view();
    let chosen = match version {
        None => view.latest.clone(),
        Some(version) if version == view.latest.version => view.latest.clone(),
        Some(version) => sqlx::query_as(
            "SELECT id, artifact_id, version, sha256, bytes, media_type, gridcraft_check, created_at
             FROM artifact_versions WHERE workspace_id = $1 AND artifact_id = $2 AND version = $3",
        )
        .bind(tenant.workspace_id())
        .bind(artifact_id)
        .bind(version)
        .fetch_optional(conn)
        .await?
        .ok_or(KatError::NotFound)?,
    };
    Ok((view, chosen))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Share,
    NoKeyUpdate,
}

/// Locks one artifact row of the tenant's workspace and returns its `deleted_at` as the locking
/// statement saw it (a statement that waited for a concurrent delete sees the deleted row):
/// `None` when the row is missing or foreign.
async fn lock_row(
    conn: &mut PgConnection,
    tenant: &Tenant,
    id: ArtifactId,
    mode: Mode,
) -> Result<Option<Option<DateTime<Utc>>>, KatError> {
    let sql = match mode {
        Mode::Share => {
            "SELECT deleted_at FROM artifacts WHERE workspace_id = $1 AND id = $2 FOR SHARE"
        }
        Mode::NoKeyUpdate => {
            "SELECT deleted_at FROM artifacts WHERE workspace_id = $1 AND id = $2 FOR NO KEY UPDATE"
        }
    };
    Ok(sqlx::query_scalar(sql)
        .bind(tenant.workspace_id())
        .bind(id)
        .fetch_optional(conn)
        .await?)
}

/// The artifact lock contract's reader side: each row `FOR SHARE`, in ascending id, inside the
/// caller's transaction. A missing, foreign or deleted artifact is `NotFound`.
pub async fn lock_source_on(
    conn: &mut PgConnection,
    tenant: &Tenant,
    artifact_ids: &[ArtifactId],
) -> Result<(), KatError> {
    for id in artifact_ids.iter().copied().collect::<BTreeSet<_>>() {
        if lock_row(conn, tenant, id, Mode::Share).await? != Some(None) {
            return Err(KatError::NotFound);
        }
    }
    Ok(())
}

/// The artifact lock contract's writer side, in one ascending pass over the union of `output`
/// and `sources`: the output's row `FOR NO KEY UPDATE`, every other row `FOR SHARE`, and an id
/// that is both only once, `FOR NO KEY UPDATE`, so no lock is ever upgraded. A missing, foreign
/// or deleted source is `CitationUnknown`; such an output is `NotFound`.
pub async fn lock_artifacts_on(
    conn: &mut PgConnection,
    tenant: &Tenant,
    output: Option<ArtifactId>,
    sources: &[ArtifactId],
) -> Result<(), KatError> {
    let mut rows: BTreeMap<ArtifactId, Mode> =
        sources.iter().map(|&id| (id, Mode::Share)).collect();
    if let Some(output) = output {
        rows.insert(output, Mode::NoKeyUpdate);
    }
    for (index, (id, mode)) in rows.into_iter().enumerate() {
        if lock_row(conn, tenant, id, mode).await? != Some(None) {
            return Err(if Some(id) == output {
                KatError::NotFound
            } else {
                KatError::CitationUnknown
            });
        }
        if index == 0 {
            test_hooks::after_first_lock().await;
        }
    }
    Ok(())
}

/// The verification pass's side: each row `FOR SHARE`, in ascending id, returning the ids whose
/// `deleted_at` is set instead of refusing them. A missing or foreign id is `NotFound`.
pub async fn lock_sources_for_pass_on(
    conn: &mut PgConnection,
    tenant: &Tenant,
    artifact_ids: &[ArtifactId],
) -> Result<BTreeSet<ArtifactId>, KatError> {
    let mut deleted = BTreeSet::new();
    for id in artifact_ids.iter().copied().collect::<BTreeSet<_>>() {
        match lock_row(conn, tenant, id, Mode::Share).await? {
            None => return Err(KatError::NotFound),
            Some(Some(_)) => {
                deleted.insert(id);
            }
            Some(None) => {}
        }
    }
    Ok(deleted)
}

/// The storage quota's sum: the bytes of every version of the workspace's live files. Each
/// version counts, even when two share one blob.
pub async fn workspace_bytes_on(
    conn: &mut PgConnection,
    workspace: WorkspaceId,
) -> Result<u64, KatError> {
    let sum: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(v.bytes), 0)::bigint FROM artifact_versions v
         JOIN artifacts a ON a.id = v.artifact_id AND a.workspace_id = v.workspace_id
         WHERE v.workspace_id = $1 AND a.deleted_at IS NULL",
    )
    .bind(workspace)
    .fetch_one(conn)
    .await?;
    Ok(u64::try_from(sum).unwrap_or(0))
}

/// A pause point for lock-order tests.
#[doc(hidden)]
pub mod test_hooks {
    use std::{future::Future, sync::Arc};

    use tokio::sync::Notify;

    /// `reached` is notified after `lock_artifacts_on`'s first row-lock statement, which then
    /// waits for `release`.
    #[derive(Clone)]
    pub struct Gate {
        pub reached: Arc<Notify>,
        pub release: Arc<Notify>,
    }

    tokio::task_local! {
        static GATE: Gate;
    }

    /// Runs `fut` with `gate` set for it alone, so tests running in parallel never share one.
    /// Outside a gate, `lock_artifacts_on` never pauses.
    #[doc(hidden)]
    pub async fn pause_after_first_lock<F: Future>(gate: Gate, fut: F) -> F::Output {
        GATE.scope(gate, fut).await
    }

    pub(super) async fn after_first_lock() {
        let Ok(gate) = GATE.try_with(Gate::clone) else {
            return;
        };
        gate.reached.notify_one();
        gate.release.notified().await;
    }
}
