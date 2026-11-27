//! The document writers `write_docx` and `write_xlsx` (`Draft`, `Trusted`) and their one commit
//! path, `commit_document` (SP2a Task 9). Trust core.
//!
//! A writer validates the Kat's content, loads every citation its claims name for the Kat's
//! job, renders the file in the document pool, reads it back with KatWork's own readers, and
//! commits it as an output version with a claim record for every cited claim and every figure
//! or date written outside a claim. Nothing is committed unless every cited claim reads back at
//! the place the renderer put it.
use std::{collections::BTreeSet, sync::Arc};

use async_trait::async_trait;
use serde_json::json;
use sqlx::{Connection, PgConnection};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{
    Rendered,
    content::{
        ClaimSpec, MAX_CLAIMS, PlacedClaim, WrittenContent, cited_ordinals, claim_specs,
        number_uncited, uncited_claims,
    },
    write_docx::bookmark_name,
};
use crate::{
    agent::{StopReason, ToolCtx, ToolDeps, ToolError, guards},
    artifacts::{
        GridcraftCheck, MediaType,
        store::{self, OutputCtx},
        tools::{from_doc, from_kat},
    },
    db,
    error::KatError,
    evidence::{citation::CitationRow, claim::insert_claims_on},
    ids::{ArtifactId, ArtifactVersionId, CitationId, JobId, SpaceId, WorkspaceId},
};

mod read_back;
mod writers;

pub use read_back::{ClaimBookmark, docx_claim_bookmarks, read_back};
pub use writers::{RenderDocxFn, RenderXlsxFn, WriteDocx, WriteXlsx};

/// Uncited figures a receipt lists; the version records all of them.
pub const RECEIPT_UNCITED: usize = 20;
/// The main part of a DOCX, whose bookmarks hold the claims.
const DOCUMENT_PART: &str = "word/document.xml";

/// Why an XLSX check refused a workbook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum XlsxRefused {
    /// GridCraft would repair the file on opening it.
    #[error("xlsx_would_be_repaired")]
    WouldBeRepaired,
}

/// The check every XLSX a writer produces goes through before commit (R19). It keeps its own
/// budget inside `deadline`; Task 15 plugs in GridCraft.
#[async_trait]
pub trait XlsxCheck: Send + Sync {
    async fn check(
        &self,
        xlsx: &[u8],
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> Result<GridcraftCheck, XlsxRefused>;
}

/// No check: every workbook is stored with `gridcraft_check = 'off'`.
pub struct NoXlsxCheck;

#[async_trait]
impl XlsxCheck for NoXlsxCheck {
    async fn check(
        &self,
        _xlsx: &[u8],
        _deadline: Instant,
        _cancel: &CancellationToken,
    ) -> Result<GridcraftCheck, XlsxRefused> {
        Ok(GridcraftCheck::Off)
    }
}

/// What a writer hands the commit path: the rendered file and the content it came from.
pub struct CommitRequest {
    /// The new output's name; a new version keeps its artifact's name.
    pub name: String,
    /// An output of this job to version, or `None` for a new output.
    pub file_id: Option<ArtifactId>,
    pub media: MediaType,
    pub rendered: Rendered,
    /// The citations `citation::get_for_job_on` loaded for the claims' cite ids.
    pub citations: Vec<CitationRow>,
    pub content: WrittenContent,
}

/// What a writer returns, also on a replay of the same call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitReceipt {
    pub file_id: ArtifactId,
    pub version: i32,
    pub name: String,
    pub cited_claims: u32,
    /// `None` for anything but an XLSX.
    pub gridcraft_check: Option<GridcraftCheck>,
    /// The first `RECEIPT_UNCITED` uncited figures, `(text, location)`, in ordinal order.
    pub uncited: Vec<(String, String)>,
}

impl CommitReceipt {
    /// The tool's one JSON line.
    pub fn line(&self) -> String {
        json!({
            "file_id": self.file_id,
            "version": self.version,
            "name": self.name,
            "cited_claims": self.cited_claims,
            "gridcraft_check": self.gridcraft_check.map(GridcraftCheck::wire),
            "uncited": self.uncited.iter()
                .map(|(text, location)| json!({"text": text, "location": location}))
                .collect::<Vec<_>>(),
        })
        .to_string()
    }
}

/// A `KatError` from the commit path as the model sees it.
fn from_write(error: KatError) -> ToolError {
    match error {
        KatError::ArtifactImmutable => ToolError::Coded("artifact_not_writable"),
        KatError::CitationUnknown => ToolError::Coded("citation_unknown"),
        KatError::FileNameInvalid => ToolError::InvalidArgs("name".into()),
        KatError::RequestInvalid => ToolError::InvalidArgs("request".into()),
        other => from_kat(other),
    }
}

fn internal() -> ToolError {
    ToolError::Failed("internal_error".into())
}

/// The receipt of the version a receipt key already names, rebuilt from its row and its claims.
/// A deleted output is `NotFound`; a key that names another job's or Space's output, or another
/// file than `file_id`, is `RequestInvalid`. Reads `claims` only when such a version exists.
async fn stored_receipt_on(
    conn: &mut PgConnection,
    ctx: &ToolCtx,
    file_id: Option<ArtifactId>,
) -> Result<Option<CommitReceipt>, KatError> {
    #[derive(sqlx::FromRow)]
    struct Stored {
        id: ArtifactVersionId,
        artifact_id: ArtifactId,
        version: i32,
        gridcraft_check: Option<GridcraftCheck>,
        name: String,
        deleted: bool,
        job_id: Option<JobId>,
        space_id: SpaceId,
    }
    let workspace = ctx.tenant.workspace_id();
    let stored: Option<Stored> = sqlx::query_as(
        "SELECT v.id, v.artifact_id, v.version, v.gridcraft_check, a.name,
                a.deleted_at IS NOT NULL AS deleted, a.job_id, a.space_id
         FROM artifact_versions v
         JOIN artifacts a ON a.id = v.artifact_id AND a.workspace_id = v.workspace_id
         WHERE v.workspace_id = $1 AND v.idempotency_key = $2",
    )
    .bind(workspace)
    .bind(&ctx.idempotency_key)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(stored) = stored else {
        return Ok(None);
    };
    if stored.deleted {
        return Err(KatError::NotFound);
    }
    if stored.job_id != Some(ctx.job_id)
        || stored.space_id != ctx.space_id
        || file_id.is_some_and(|id| id != stored.artifact_id)
    {
        return Err(KatError::RequestInvalid);
    }
    let cited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM claims WHERE workspace_id = $1 AND artifact_version_id = $2 AND cited",
    )
    .bind(workspace)
    .bind(stored.id)
    .fetch_one(&mut *conn)
    .await?;
    let uncited: Vec<(String, String)> = sqlx::query_as(
        "SELECT text, location FROM claims
         WHERE workspace_id = $1 AND artifact_version_id = $2 AND NOT cited
         ORDER BY ordinal LIMIT $3",
    )
    .bind(workspace)
    .bind(stored.id)
    .bind(RECEIPT_UNCITED as i64)
    .fetch_all(&mut *conn)
    .await?;
    Ok(Some(CommitReceipt {
        file_id: stored.artifact_id,
        version: stored.version,
        name: stored.name,
        cited_claims: u32::try_from(cited).map_err(|_| KatError::RequestInvalid)?,
        gridcraft_check: stored.gridcraft_check,
        uncited,
    }))
}

/// The advisory lock that writers with one receipt key take in turn, from before their receipt
/// check to their COMMIT: the two big-endian halves of the first 8 bytes of SHA-256 over
/// `writer receipt`, the workspace id's 16 bytes and the key. A two-key lock never shares a key
/// with a one-key blob lock.
fn receipt_lock_keys(workspace: WorkspaceId, key: &str) -> (i32, i32) {
    use sha2::{Digest, Sha256};
    let digest = Sha256::new()
        .chain_update(b"writer receipt")
        .chain_update(workspace.0.as_bytes())
        .chain_update(key.as_bytes())
        .finalize();
    let (mut high, mut low) = ([0u8; 4], [0u8; 4]);
    high.copy_from_slice(&digest[..4]);
    low.copy_from_slice(&digest[4..8]);
    (i32::from_be_bytes(high), i32::from_be_bytes(low))
}

/// Whether every one of `ids` is a live artifact of `space`.
async fn sources_in_space_on(
    conn: &mut PgConnection,
    ctx: &ToolCtx,
    ids: &[ArtifactId],
) -> Result<(), KatError> {
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM artifacts
         WHERE workspace_id = $1 AND id = ANY($2) AND space_id = $3 AND deleted_at IS NULL",
    )
    .bind(ctx.tenant.workspace_id())
    .bind(ids)
    .bind(ctx.space_id)
    .fetch_one(conn)
    .await?;
    if live != ids.len() as i64 {
        return Err(KatError::CitationUnknown);
    }
    Ok(())
}

/// The distinct source artifacts of `citations`, in ascending id.
fn source_ids(citations: &[CitationRow]) -> Vec<ArtifactId> {
    citations
        .iter()
        .map(|c| c.artifact_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Checks that the renderer placed exactly the content's claims, in document order, with their
/// final ordinals and a location of its format. The content, never the renderer, decides what
/// is recorded.
fn check_placed(
    placed: &[PlacedClaim],
    specs: &[&ClaimSpec],
    ordinals: &[u32],
    media: MediaType,
) -> Result<(), ToolError> {
    if placed.len() != specs.len() {
        return Err(internal());
    }
    for ((claim, spec), &ordinal) in placed.iter().zip(specs).zip(ordinals) {
        let located = match media {
            MediaType::Docx => claim.location == format!("bookmark:{}", bookmark_name(ordinal)),
            MediaType::Xlsx => claim.location.starts_with("cell:"),
            MediaType::Pdf => claim.location.starts_with("page:"),
            _ => false,
        };
        if !located
            || claim.ordinal != ordinal
            || !claim.cited
            || claim.text != spec.text
            || claim.kind != spec.kind
            || claim.cite != spec.cite
            || claim.derive != spec.derive
        {
            return Err(internal());
        }
    }
    Ok(())
}

/// Steps 3 to 6 of every document writer: read-back (and the XLSX check), the uncited figures,
/// staging, and one guarded transaction that authorizes the write, returns a stored receipt on
/// a replay, locks the output and every cited source in one ascending pass, commits the blob,
/// inserts the version and its claims.
pub async fn commit_document(
    ctx: &ToolCtx,
    deps: &ToolDeps,
    req: CommitRequest,
) -> Result<CommitReceipt, ToolError> {
    let workspace = ctx.tenant.workspace_id();
    // The figure scan is CPU work over up to 200,000 characters: it runs in the pool.
    let content = Arc::new(req.content);
    let scan = content.clone();
    let uncited = deps
        .docs
        .run(workspace, &ctx.cancel, Some(ctx.deadline), move |_| {
            Ok(uncited_claims(&scan))
        })
        .await
        .map_err(from_doc)?
        .map_err(|_| ToolError::Coded("text_too_long"))?;
    let specs = claim_specs(&content);
    if specs.len() + uncited.len() > MAX_CLAIMS {
        return Err(ToolError::InvalidArgs("too_many_claims".into()));
    }
    let ordinals = cited_ordinals(specs.len(), &uncited);
    check_placed(&req.rendered.claims, &specs, &ordinals, req.media)?;
    let known: BTreeSet<CitationId> = req.citations.iter().map(|c| c.id).collect();
    if !specs
        .iter()
        .flat_map(|s| &s.cite)
        .all(|id| known.contains(id))
    {
        return Err(ToolError::Coded("citation_unknown"));
    }
    read_back(
        req.media,
        &req.rendered.bytes,
        &req.rendered.claims,
        deps,
        workspace,
        &ctx.cancel,
        ctx.deadline,
    )
    .await?;
    let gridcraft = if req.media == MediaType::Xlsx {
        let check = deps
            .xlsx_check
            .check(&req.rendered.bytes, ctx.deadline, &ctx.cancel);
        match tokio::time::timeout_at(ctx.deadline, check).await {
            Ok(Ok(check)) => Some(check),
            Ok(Err(XlsxRefused::WouldBeRepaired)) => {
                return Err(ToolError::Coded("xlsx_would_be_repaired"));
            }
            Err(_) => return Err(ToolError::Coded("doc_timeout")),
        }
    } else {
        None
    };
    let mut claims: Vec<_> = req
        .rendered
        .claims
        .iter()
        .cloned()
        .chain(number_uncited(uncited))
        .collect();
    claims.sort_by_key(|c| c.ordinal);
    let claims: Vec<_> = claims.iter().map(PlacedClaim::to_new_claim).collect();
    let sources = source_ids(&req.citations);
    let staged = deps
        .blobs
        .stage_bytes(workspace, &req.rendered.bytes)
        .await
        .map_err(from_kat)?;
    let output = OutputCtx {
        tenant: ctx.tenant.clone(),
        space_id: ctx.space_id,
        kat_id: ctx.kat_id,
        job_id: ctx.job_id,
        run_id: ctx.run_id,
    };
    enum Outcome {
        Halted(StopReason),
        Done(CommitReceipt),
    }
    let outcome = db::guarded(&ctx.pool, async |conn| {
        let mut tx = conn.begin().await?;
        // Sets the lock timeout, then takes the workspace, task and Kat share locks.
        if let Some(stop) = guards::authorize_write(&mut tx, ctx).await? {
            return Ok(Outcome::Halted(stop));
        }
        // Calls with one receipt key commit in turn, so the second sees the first's receipt
        // instead of inserting its claims again.
        let (high, low) = receipt_lock_keys(workspace, &ctx.idempotency_key);
        sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
            .bind(high)
            .bind(low)
            .execute(&mut *tx)
            .await?;
        // A replay returns what the first call stored, even when a source it cited is gone.
        if let Some(receipt) = stored_receipt_on(&mut tx, ctx, req.file_id).await? {
            return Ok(Outcome::Done(receipt));
        }
        // Task 26's lease check (`human_has_control`) goes here, before any artifact lock.
        store::lock_artifacts_on(&mut tx, &ctx.tenant, req.file_id, &sources).await?;
        // Under the locks: a source may have moved to another Space since it was cited.
        if !sources.is_empty() {
            sources_in_space_on(&mut tx, ctx, &sources).await?;
        }
        let blob = store::commit_blob_on(&mut tx, &*deps.blobs, staged).await?;
        let version = match req.file_id {
            None => {
                store::create_output_on(
                    &mut tx,
                    &output,
                    &req.name,
                    req.media,
                    &blob,
                    gridcraft,
                    &ctx.idempotency_key,
                )
                .await?
            }
            Some(id) => {
                store::new_output_version_on(
                    &mut tx,
                    &output,
                    id,
                    req.media,
                    &blob,
                    gridcraft,
                    &ctx.idempotency_key,
                )
                .await?
            }
        };
        // A new version keeps its file's type: a DOCX never becomes an XLSX's next version.
        // Checked after the store's own refusals (missing, deleted, input, another job's).
        if req.file_id.is_some() {
            let previous: MediaType = sqlx::query_scalar(
                "SELECT media_type FROM artifact_versions
                 WHERE workspace_id = $1 AND artifact_id = $2 AND version = $3",
            )
            .bind(workspace)
            .bind(version.artifact_id)
            .bind(version.version - 1)
            .fetch_one(&mut *tx)
            .await?;
            if previous != req.media {
                return Err(KatError::RequestInvalid);
            }
        }
        insert_claims_on(&mut tx, &ctx.tenant, version.id, &claims, &req.citations).await?;
        let name: String =
            sqlx::query_scalar("SELECT name FROM artifacts WHERE workspace_id = $1 AND id = $2")
                .bind(workspace)
                .bind(version.artifact_id)
                .fetch_one(&mut *tx)
                .await?;
        tx.commit().await?;
        Ok(Outcome::Done(CommitReceipt {
            file_id: version.artifact_id,
            version: version.version,
            name,
            cited_claims: claims.iter().filter(|c| c.cited).count() as u32,
            gridcraft_check: gridcraft,
            uncited: claims
                .iter()
                .filter(|c| !c.cited)
                .take(RECEIPT_UNCITED)
                .map(|c| (c.text.clone(), c.location.clone()))
                .collect(),
        }))
    })
    .await
    .map_err(from_write)?;
    match outcome {
        Outcome::Halted(stop) => Err(ToolError::Halted(stop)),
        Outcome::Done(receipt) => Ok(receipt),
    }
}
