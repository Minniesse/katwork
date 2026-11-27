//! The document writers `write_docx` and `write_xlsx` (`Draft`, `Trusted`) and their one commit
//! path, `commit_document` (SP2a Task 9). Trust core.
//!
//! A writer validates the Kat's content, loads every citation its claims name for the Kat's
//! job, renders the file in the document pool, reads it back with KatWork's own readers, and
//! commits it as an output version with a claim record for every cited claim and every figure
//! or date written outside a claim. Nothing is committed unless every cited claim reads back at
//! the place the renderer put it.
use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};

use async_trait::async_trait;
use katwork_thai::match_key;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    DocError, Extracted, Locator, Rendered,
    content::{
        self, ClaimSpec, DocContent, MAX_CLAIMS, PlacedClaim, SheetSpec, WrittenContent, check_doc,
        check_sheets, cited_ordinals, claim_specs, number_uncited, uncited_claims,
    },
    deadline, package,
    write_docx::bookmark_name,
    xml_guard,
};
use crate::{
    agent::{StopReason, Tool, ToolCtx, ToolDeps, ToolError, ToolOutput, guards},
    artifacts::{
        GridcraftCheck, MediaType, names,
        store::{self, OutputCtx},
        tools::{from_doc, from_kat},
    },
    db,
    error::KatError,
    evidence::{
        citation::{self, CitationRow},
        claim::insert_claims_on,
    },
    ids::{ArtifactId, ArtifactVersionId, CitationId, JobId, SpaceId, WorkspaceId},
    models::ToolSpec,
    types::SideEffect,
};

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

/// A `kw-claim-<n>` bookmark of a DOCX main part: its name, the 1-based ordinal of the `w:p` it
/// starts in (the reader's paragraph ordinal), and the text of the `w:t` elements between its
/// start and its end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimBookmark {
    pub name: String,
    pub paragraph: u32,
    pub text: String,
}

/// The bookmarks whose name starts `kw-claim-`, in document order of their ends. KatWork's
/// readers do not expose bookmarks; Task 25's claim markers should reuse this scanner.
pub fn docx_claim_bookmarks(document_xml: &[u8]) -> Result<Vec<ClaimBookmark>, DocError> {
    use quick_xml::events::{BytesStart, Event};
    let mut reader = xml_guard::reader(document_xml);
    let mut buf = Vec::new();
    let mut paragraphs = 0u32;
    let mut in_text = 0u32;
    // Open bookmarks by `w:id`.
    let mut open: HashMap<String, ClaimBookmark> = HashMap::new();
    let mut done = Vec::new();
    let attribute = |start: &BytesStart, local: &str| -> Result<Option<String>, DocError> {
        for attr in start.attributes() {
            let attr = attr.map_err(|_| DocError::Malformed("xml_attribute"))?;
            if attr.key.local_name().as_ref() == local {
                return Ok(Some(xml_guard::attribute_value(&attr.value)?.into_owned()));
            }
        }
        Ok(None)
    };
    let mut events = 0u32;
    loop {
        events = events.wrapping_add(1);
        if events.is_multiple_of(1024) {
            deadline::check()?;
        }
        buf.clear();
        let event = reader
            .read_event_into(&mut buf)
            .map_err(xml_guard::map_error)?;
        match event {
            Event::Start(ref start) | Event::Empty(ref start) => {
                let empty = matches!(event, Event::Empty(_));
                match start.local_name().as_ref() {
                    "p" => paragraphs += 1,
                    "t" if !empty => in_text += 1,
                    "bookmarkStart" => {
                        let (Some(id), Some(name)) =
                            (attribute(start, "id")?, attribute(start, "name")?)
                        else {
                            continue;
                        };
                        if name.starts_with("kw-claim-") {
                            open.insert(
                                id,
                                ClaimBookmark {
                                    name,
                                    paragraph: paragraphs,
                                    text: String::new(),
                                },
                            );
                        }
                    }
                    "bookmarkEnd" => {
                        if let Some(id) = attribute(start, "id")?
                            && let Some(mark) = open.remove(&id)
                        {
                            done.push(mark);
                        }
                    }
                    _ => {}
                }
            }
            Event::End(ref end) => {
                if end.local_name().as_ref() == "t" {
                    in_text = in_text.saturating_sub(1);
                }
            }
            Event::Text(text) if in_text > 0 && !open.is_empty() => {
                let text = text.xml10_content();
                open.values_mut().for_each(|m| m.text.push_str(&text));
            }
            Event::GeneralRef(reference) if in_text > 0 && !open.is_empty() => {
                let c = xml_guard::resolve_ref(&reference)?;
                open.values_mut().for_each(|m| m.text.push(c));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(done)
}

/// The 1-based row and column of an A1 reference.
fn parse_a1(a1: &str) -> Option<(u32, u32)> {
    let split = a1.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = a1.split_at(split);
    if letters.is_empty() || !letters.bytes().all(|b| b.is_ascii_uppercase()) {
        return None;
    }
    let col = letters.bytes().try_fold(0u32, |n, b| {
        n.checked_mul(26)?.checked_add(u32::from(b - b'A' + 1))
    })?;
    let row: u32 = digits.parse().ok()?;
    (row > 0 && !digits.starts_with('0')).then_some((row, col))
}

fn visible(block: &super::Block) -> bool {
    block.hidden.is_none()
        && block.suspect.is_none()
        && block.cell.as_ref().is_none_or(|cell| !cell.hidden)
}

/// The blocks of one extraction, indexed by where a claim may be.
#[derive(Default)]
struct Placed<'a> {
    /// A DOCX main-part paragraph's pieces joined, and whether every piece is visible.
    paragraphs: HashMap<u32, (String, bool)>,
    cells: HashMap<(&'a str, u32, u32), (String, bool)>,
    /// Each PDF page's visible blocks, in order.
    pages: HashMap<u32, Vec<&'a str>>,
}

impl<'a> Placed<'a> {
    fn new(extracted: &'a Extracted) -> Result<Self, DocError> {
        let mut placed = Placed::default();
        for (index, block) in extracted.blocks.iter().enumerate() {
            if index % 1024 == 1023 {
                deadline::check()?;
            }
            let seen = visible(block);
            let join = |entry: &mut (String, bool)| {
                entry.0.push_str(&block.text);
                entry.1 &= seen;
            };
            match &block.locator {
                Locator::DocxParagraph { part, ordinal, .. } if &**part == DOCUMENT_PART => {
                    join(
                        placed
                            .paragraphs
                            .entry(*ordinal)
                            .or_insert((String::new(), true)),
                    );
                }
                Locator::XlsxCell {
                    sheet, row, col, ..
                } => join(
                    placed
                        .cells
                        .entry((sheet.as_str(), *row, *col))
                        .or_insert((String::new(), true)),
                ),
                Locator::PdfPage { page } if seen => {
                    placed.pages.entry(*page).or_default().push(&block.text);
                }
                _ => {}
            }
        }
        Ok(placed)
    }

    /// Whether `claim`'s text, after `match_key`, is where its location says.
    fn holds(&self, claim: &PlacedClaim, marks: &HashMap<&str, Option<&ClaimBookmark>>) -> bool {
        let want = match_key(&claim.text).text;
        if want.is_empty() {
            return false;
        }
        if let Some(name) = claim.location.strip_prefix("bookmark:") {
            // The bookmark wraps exactly the claim, in a paragraph the reader shows whole.
            let Some(Some(mark)) = marks.get(name) else {
                return false;
            };
            return match_key(&mark.text).text == want
                && self
                    .paragraphs
                    .get(&mark.paragraph)
                    .is_some_and(|(text, seen)| *seen && match_key(text).text.contains(&want));
        }
        if let Some(cell) = claim.location.strip_prefix("cell:") {
            // A sheet name may hold `!`: the reference follows the last one.
            let Some((sheet, a1)) = cell.rsplit_once('!') else {
                return false;
            };
            let Some((row, col)) = parse_a1(a1) else {
                return false;
            };
            return self
                .cells
                .get(&(sheet, row, col))
                .is_some_and(|(text, seen)| *seen && match_key(text).text == want);
        }
        if let Some(page) = claim.location.strip_prefix("page:") {
            let Ok(page) = page.parse::<u32>() else {
                return false;
            };
            let Some(blocks) = self.pages.get(&page) else {
                return false;
            };
            let next = page
                .checked_add(1)
                .and_then(|n| self.pages.get(&n))
                .and_then(|b| b.first());
            // A claim that breaks across the page starts in a block of this page and ends in the
            // next page's first block, whatever blocks (footnotes) follow it on this page.
            return blocks.iter().any(|block| {
                let own = match_key(block).text;
                if own.contains(&want) {
                    return true;
                }
                let Some(next) = next else {
                    return false;
                };
                let joined = match_key(&format!("{block} {next}")).text;
                let own_len = own.chars().count();
                joined.match_indices(&want).any(|(at, _)| {
                    let start = joined[..at].chars().count();
                    start < own_len && start + want.chars().count() > own_len
                })
            });
        }
        false
    }
}

/// Re-extracts `bytes` with KatWork's own reader, in the pool and never from the extraction
/// cache (the bytes are not committed), and checks that each cited claim's text is at its
/// location: in its `bookmark:` (DOCX), its `cell:` (XLSX) or on its `page:` (PDF). Any claim
/// that is not is `read_back_failed`.
pub async fn read_back(
    media: MediaType,
    bytes: &[u8],
    claims: &[PlacedClaim],
    deps: &ToolDeps,
    workspace: WorkspaceId,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<(), ToolError> {
    if !matches!(media, MediaType::Docx | MediaType::Xlsx | MediaType::Pdf) {
        return Err(internal());
    }
    let extract = deps.extract.clone();
    let bytes = bytes.to_vec();
    let claims = claims.to_vec();
    let held = deps
        .docs
        .run(workspace, cancel, Some(deadline), move |limits| {
            let extracted = extract(media.kind(), &bytes, limits)?;
            let marks = if media == MediaType::Docx {
                let package = package::open_package(&bytes, limits)?;
                let part = package
                    .part(DOCUMENT_PART)
                    .ok_or(DocError::Malformed("docx_main_part"))?;
                docx_claim_bookmarks(part)?
            } else {
                Vec::new()
            };
            // A name used twice locates nothing.
            let mut by_name: HashMap<&str, Option<&ClaimBookmark>> = HashMap::new();
            for mark in &marks {
                by_name
                    .entry(mark.name.as_str())
                    .and_modify(|m| *m = None)
                    .or_insert(Some(mark));
            }
            let placed = Placed::new(&extracted)?;
            for (index, claim) in claims.iter().enumerate() {
                if index % 256 == 255 {
                    deadline::check()?;
                }
                if !placed.holds(claim, &by_name) {
                    return Ok(false);
                }
            }
            Ok(true)
        })
        .await
        .map_err(from_doc)?;
    if held {
        Ok(())
    } else {
        Err(ToolError::Coded("read_back_failed"))
    }
}

/// Renders a writer's content: `render_docx` or `render_xlsx` in the product, a hook in tests.
pub type RenderDocxFn =
    Arc<dyn Fn(&DocContent, &[CitationRow]) -> Result<Rendered, DocError> + Send + Sync>;
pub type RenderXlsxFn =
    Arc<dyn Fn(&[SheetSpec], &[CitationRow]) -> Result<Rendered, DocError> + Send + Sync>;

/// The name a new output gets: `name` with `.<extension>` appended unless it ends with it
/// (ignoring case), then the file-name rules.
fn output_name(name: &str, extension: &str) -> Result<String, ToolError> {
    let suffix = format!(".{extension}");
    let name = if name.to_lowercase().ends_with(&suffix) {
        name.to_owned()
    } else {
        format!("{name}{suffix}")
    };
    names::validate_name(&name).map_err(|_| ToolError::InvalidArgs("name".into()))
}

/// The plain checks before anything renders: an earlier call with this receipt key returns its
/// receipt (after the policy check, as inside the commit), and every citation the claims name
/// is one of this job's, of a live file of this Space.
async fn prepare(
    ctx: &ToolCtx,
    content: &WrittenContent,
    file_id: Option<ArtifactId>,
) -> Result<Result<Vec<CitationRow>, CommitReceipt>, ToolError> {
    let ids: Vec<CitationId> = claim_specs(content)
        .iter()
        .flat_map(|s| s.cite.iter().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    enum Plain {
        Halted(StopReason),
        Replay(CommitReceipt),
        Citations(Vec<CitationRow>),
    }
    let plain = db::guarded(&ctx.pool, async |conn| {
        let mut tx = conn.begin().await?;
        if let Some(stop) = guards::authorize_write(&mut tx, ctx).await? {
            return Ok(Plain::Halted(stop));
        }
        if let Some(receipt) = stored_receipt_on(&mut tx, ctx, file_id).await? {
            return Ok(Plain::Replay(receipt));
        }
        tx.rollback().await?;
        if ids.is_empty() {
            return Ok(Plain::Citations(Vec::new()));
        }
        let rows = citation::get_for_job_on(conn, &ctx.tenant, ctx.job_id, &ids).await?;
        if rows.len() != ids.len() {
            return Err(KatError::CitationUnknown);
        }
        // Fail fast: a source deleted or moved since it was cited is refused before rendering;
        // the commit checks again under the locks.
        sources_in_space_on(conn, ctx, &source_ids(&rows)).await?;
        Ok(Plain::Citations(rows))
    })
    .await
    .map_err(from_write)?;
    match plain {
        Plain::Halted(stop) => Err(ToolError::Halted(stop)),
        Plain::Replay(receipt) => Ok(Err(receipt)),
        Plain::Citations(rows) => Ok(Ok(rows)),
    }
}

/// Every writer's call after its arguments are checked.
async fn write(
    ctx: &ToolCtx,
    deps: &ToolDeps,
    name: String,
    file_id: Option<ArtifactId>,
    media: MediaType,
    content: WrittenContent,
    render: impl FnOnce(&WrittenContent, &[CitationRow]) -> Result<Rendered, DocError> + Send + 'static,
) -> Result<ToolOutput, ToolError> {
    let citations = match prepare(ctx, &content, file_id).await? {
        Ok(rows) => rows,
        Err(receipt) => return Ok(ToolOutput::Text(receipt.line())),
    };
    let content = Arc::new(content);
    let (job_content, job_citations) = (content.clone(), citations.clone());
    let rendered = deps
        .docs
        .run(
            ctx.tenant.workspace_id(),
            &ctx.cancel,
            Some(ctx.deadline),
            move |_| render(&job_content, &job_citations),
        )
        .await
        .map_err(from_doc)?;
    let receipt = commit_document(
        ctx,
        deps,
        CommitRequest {
            name,
            file_id,
            media,
            rendered,
            citations,
            content: Arc::unwrap_or_clone(content),
        },
    )
    .await?;
    Ok(ToolOutput::Text(receipt.line()))
}

/// The head version of `file_id` in `args`, so writing a file again after it changed is not a
/// repeat; `None` for a new output.
async fn head_revision(ctx: &ToolCtx, args: &Value) -> Option<String> {
    let id: Uuid = args.get("file_id")?.as_str()?.parse().ok()?;
    let (_, version) = db::guarded(&ctx.pool, async |conn| {
        store::version_in_space_on(conn, &ctx.tenant, ctx.space_id, ArtifactId(id), None).await
    })
    .await
    .ok()?;
    Some(version.id.to_string())
}

const UUID_PATTERN: &str =
    "^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$";

/// The JSON Schema of a claim inline: exactly the fields `ClaimSpec` takes.
fn claim_schema() -> Value {
    json!({"type": "object", "properties": {"claim": {"type": "object", "properties": {
        "text": {"type": "string", "minLength": 1, "maxLength": crate::evidence::claim::MAX_CLAIM_CHARS},
        "kind": {"enum": ["number", "date", "quote", "fact"]},
        "cite": {"type": "array", "items": {"type": "string", "pattern": UUID_PATTERN},
                 "minItems": 1, "maxItems": content::MAX_CITE},
        "derive": {"type": "object", "properties": {
            "op": {"enum": ["sum", "difference", "ratio", "share", "percent_change"]},
            "args": {"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255}}},
            "required": ["op", "args"], "additionalProperties": false}},
        "required": ["text", "kind", "cite"], "additionalProperties": false}},
        "required": ["claim"], "additionalProperties": false})
}

fn docx_schema() -> Value {
    let inlines = json!({"type": "array", "items": {"anyOf": [{"type": "string"}, {"$ref": "#/$defs/claim"}]}});
    let block = |kind: &str, properties: Value, required: &[&str]| {
        let mut properties = properties;
        properties["type"] = json!({"const": kind});
        let mut required: Vec<&str> = required.to_vec();
        required.push("type");
        json!({"type": "object", "properties": properties, "required": required,
               "additionalProperties": false})
    };
    json!({"type": "object", "properties": {
        "name": {"type": "string", "minLength": 1, "maxLength": 512},
        "file_id": {"type": "string", "pattern": UUID_PATTERN},
        "content": {"type": "object", "properties": {
            "title": {"type": ["string", "null"]},
            "blocks": {"type": "array", "maxItems": content::MAX_BLOCKS, "items": {"oneOf": [
                block("heading", json!({"level": {"type": "integer", "minimum": 1, "maximum": 3},
                    "text": {"$ref": "#/$defs/inlines"}}), &["level", "text"]),
                block("paragraph", json!({"text": {"$ref": "#/$defs/inlines"}}), &["text"]),
                block("list", json!({"ordered": {"type": "boolean"},
                    "items": {"type": "array", "items": {"$ref": "#/$defs/inlines"}}}),
                    &["ordered", "items"]),
                block("table", json!({
                    "header": {"type": "array", "items": {"$ref": "#/$defs/inlines"}},
                    "rows": {"type": "array", "items": {"type": "array",
                        "items": {"$ref": "#/$defs/inlines"}}}}), &["header", "rows"])]}}},
            "required": ["blocks"], "additionalProperties": false}},
        "required": ["name", "content"], "additionalProperties": false,
        "$defs": {"claim": claim_schema(), "inlines": inlines}})
}

fn xlsx_schema() -> Value {
    json!({"type": "object", "properties": {
        "name": {"type": "string", "minLength": 1, "maxLength": 512},
        "file_id": {"type": "string", "pattern": UUID_PATTERN},
        "sheets": {"type": "array", "minItems": 1, "maxItems": content::MAX_SHEETS, "items": {
            "type": "object", "properties": {
                "name": {"type": "string", "minLength": 1, "maxLength": content::MAX_SHEET_NAME_CHARS},
                "rows": {"type": "array", "items": {"type": "array", "items": {"anyOf": [
                    {"type": "null"},
                    {"type": "string", "maxLength": content::MAX_CELL_CHARS},
                    {"type": "object", "properties": {"number": {"type": "string",
                        "maxLength": content::MAX_NUMBER_CHARS,
                        "pattern": "^-?(0|[1-9][0-9]*)(\\.[0-9]+)?$"}},
                        "required": ["number"], "additionalProperties": false},
                    {"$ref": "#/$defs/claim"}]}}}},
            "required": ["name", "rows"], "additionalProperties": false}}},
        "required": ["name", "sheets"], "additionalProperties": false,
        "$defs": {"claim": claim_schema()}})
}

const CLAIM_HELP: &str = "A claim is {\"claim\": {\"text\", \"kind\": number|date|quote|fact, \
    \"cite\": [citation ids from `cite`], \"derive\"?: {\"op\", \"args\": indexes into cite}}}: \
    never type a page, cell, file or link. Every figure or date you write outside a claim is \
    recorded as uncited and listed in the result, so cite it or leave it out. Pass `file_id` (an \
    output of this job) to write a new version of it.";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DocxArgs {
    name: String,
    content: DocContent,
    #[serde(default)]
    file_id: Option<Uuid>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct XlsxArgs {
    name: String,
    sheets: Vec<SheetSpec>,
    #[serde(default)]
    file_id: Option<Uuid>,
}

/// `write_docx`: a Word document of the Kat's structured content.
pub struct WriteDocx {
    deps: ToolDeps,
    render: RenderDocxFn,
}

impl WriteDocx {
    pub fn new(deps: ToolDeps) -> Self {
        Self::with_renderer(deps, Arc::new(super::write_docx::render_docx))
    }

    /// The tool with another renderer, for tests of the commit path.
    #[doc(hidden)]
    pub fn with_renderer(deps: ToolDeps, render: RenderDocxFn) -> Self {
        Self { deps, render }
    }
}

#[async_trait]
impl Tool for WriteDocx {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write_docx".into(),
            description: format!(
                "Write a Word document (.docx) into this Space: an optional title and blocks \
                 (heading level 1-3, paragraph, list, table) whose text is a list of strings \
                 and claims. Each claim is bookmarked and numbered against a sources list the \
                 harness writes. {CLAIM_HELP}"
            ),
            parameters: docx_schema(),
        }
    }

    fn side_effect(&self) -> SideEffect {
        SideEffect::Draft
    }

    /// The result echoes the Kat's own arguments and ids.
    fn output_trust(&self) -> crate::agent::OutputTrust {
        crate::agent::OutputTrust::Trusted
    }

    async fn target_revision(&self, ctx: &ToolCtx, args: &Value) -> Option<String> {
        head_revision(ctx, args).await
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let args: DocxArgs = serde_json::from_value(args)
            .map_err(|_| ToolError::InvalidArgs("write_docx".into()))?;
        let mut content = args.content;
        check_doc(&mut content).map_err(|e| ToolError::InvalidArgs(e.0.into()))?;
        let name = output_name(&args.name, "docx")?;
        let render = self.render.clone();
        write(
            ctx,
            &self.deps,
            name,
            args.file_id.map(ArtifactId),
            MediaType::Docx,
            WrittenContent::Doc(content),
            move |content, sources| match content {
                WrittenContent::Doc(doc) => render(doc, sources),
                WrittenContent::Sheets(_) => Err(DocError::Internal),
            },
        )
        .await
    }
}

/// `write_xlsx`: an Excel workbook of the Kat's sheets.
pub struct WriteXlsx {
    deps: ToolDeps,
    render: RenderXlsxFn,
}

impl WriteXlsx {
    pub fn new(deps: ToolDeps) -> Self {
        Self::with_renderer(deps, Arc::new(super::write_xlsx::render_xlsx))
    }

    /// The tool with another renderer, for tests of the commit path.
    #[doc(hidden)]
    pub fn with_renderer(deps: ToolDeps, render: RenderXlsxFn) -> Self {
        Self { deps, render }
    }
}

#[async_trait]
impl Tool for WriteXlsx {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write_xlsx".into(),
            description: format!(
                "Write an Excel workbook (.xlsx) into this Space: 1-16 sheets of rows of cells. \
                 A cell is null, a string (always text, never a formula), {{\"number\": \
                 \"<decimal>\"}} or a claim; a claim cell gets a note naming its sources. \
                 {CLAIM_HELP}"
            ),
            parameters: xlsx_schema(),
        }
    }

    fn side_effect(&self) -> SideEffect {
        SideEffect::Draft
    }

    /// The result echoes the Kat's own arguments and ids.
    fn output_trust(&self) -> crate::agent::OutputTrust {
        crate::agent::OutputTrust::Trusted
    }

    async fn target_revision(&self, ctx: &ToolCtx, args: &Value) -> Option<String> {
        head_revision(ctx, args).await
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let args: XlsxArgs = serde_json::from_value(args)
            .map_err(|_| ToolError::InvalidArgs("write_xlsx".into()))?;
        let mut sheets = args.sheets;
        check_sheets(&mut sheets).map_err(|e| ToolError::InvalidArgs(e.0.into()))?;
        let name = output_name(&args.name, "xlsx")?;
        let render = self.render.clone();
        write(
            ctx,
            &self.deps,
            name,
            args.file_id.map(ArtifactId),
            MediaType::Xlsx,
            WrittenContent::Sheets(sheets),
            move |content, sources| match content {
                WrittenContent::Sheets(sheets) => render(sheets, sources),
                WrittenContent::Doc(_) => Err(DocError::Internal),
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a1_references_parse() {
        assert_eq!(parse_a1("A1"), Some((1, 1)));
        assert_eq!(parse_a1("B2"), Some((2, 2)));
        assert_eq!(parse_a1("AA10"), Some((10, 27)));
        assert_eq!(parse_a1("XFD1048576"), Some((1_048_576, 16_384)));
        for bad in ["", "A", "1", "a1", "A0", "A01", "A1B"] {
            assert_eq!(parse_a1(bad), None, "{bad}");
        }
    }

    #[test]
    fn output_names_get_their_extension() {
        assert_eq!(output_name("minutes", "docx").unwrap(), "minutes.docx");
        assert_eq!(output_name("a/B.DOCX", "docx").unwrap(), "a/B.DOCX");
        assert_eq!(
            output_name("book.xlsx.docx", "xlsx").unwrap(),
            "book.xlsx.docx.xlsx"
        );
        assert!(output_name("../x", "docx").is_err());
        assert!(output_name("a\u{200B}b", "docx").is_err());
    }

    #[test]
    fn bookmarks_scan_names_paragraphs_and_text() {
        let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
            <w:p><w:r><w:t>a</w:t></w:r></w:p>
            <w:p><w:r><w:t>b </w:t></w:r><w:bookmarkStart w:id="2" w:name="kw-claim-2"/><w:r><w:t>x &amp; &#3585;</w:t></w:r><w:r><w:t>y</w:t></w:r><w:bookmarkEnd w:id="2"/></w:p>
            <w:p><w:bookmarkStart w:id="9" w:name="_Other"/><w:r><w:t>z</w:t></w:r><w:bookmarkEnd w:id="9"/></w:p>
            </w:body></w:document>"#;
        assert_eq!(
            docx_claim_bookmarks(xml).unwrap(),
            vec![ClaimBookmark {
                name: "kw-claim-2".into(),
                paragraph: 2,
                text: "x & กy".into(),
            }]
        );
    }
}
