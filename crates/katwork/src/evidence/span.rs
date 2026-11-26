//! Spans: the rows the harness mints over a version's blocks, which `cite` (Task 7) cites.
//! A span is a snapshot; a reread compares it with the fresh block and fails closed on any
//! difference the reader should have versioned.
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

use katwork_thai::{NORMALIZE_VERSION, match_key};
use sha2::{Digest, Sha256};
use sqlx::PgConnection;

use super::{
    instruction::block_instruction_like,
    source_ref::{SourceRef, suspect_reason},
};
use crate::{
    artifacts::{ArtifactRole, store, store::VersionView},
    docs::{Block, Extracted, HiddenWhy, Locator, PageInfo},
    error::KatError,
    ids::{ArtifactId, ArtifactVersionId, SpaceId, SpanId},
    tenancy::Tenant,
};

#[derive(Debug, Clone, PartialEq)]
pub struct SpanRow {
    pub id: SpanId,
    pub artifact_id: ArtifactId,
    pub artifact_version_id: ArtifactVersionId,
    pub file_name: String,
    /// The DOCX package part, `""` for other types.
    pub part: String,
    pub ordinal: u32,
    pub piece: u32,
    pub text: String,
    pub source_ref: SourceRef,
    pub generated: bool,
    pub hidden: Option<String>,
    pub instruction_like: Option<String>,
    pub suspect_text_layer: Option<String>,
}

/// `(part, ordinal, piece)`: a block's identity within one extraction.
type BlockKey = (String, u32, u32);

fn block_key(block: &Block) -> BlockKey {
    let part = match &block.locator {
        Locator::DocxParagraph { part, .. } => part.to_string(),
        Locator::Text { .. } | Locator::PdfPage { .. } | Locator::XlsxCell { .. } => String::new(),
    };
    (part.replace('\0', "\u{FFFD}"), block.ordinal, block.piece)
}

/// The `HiddenWhy` wire name.
pub fn hidden_wire(why: HiddenWhy) -> String {
    match serde_json::to_value(why) {
        Ok(serde_json::Value::String(wire)) => wire,
        _ => unreachable!("HiddenWhy serializes as a string"),
    }
}

fn sha256(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// The page a block lies on, for a PDF block.
pub fn page_of<'a>(extracted: &'a Extracted, block: &Block) -> Option<&'a PageInfo> {
    match block.locator {
        Locator::PdfPage { page } => extracted.pages.iter().find(|p| p.page == page),
        _ => None,
    }
}

/// What a stored row must still agree with.
#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    text_sha256: String,
    hidden: Option<String>,
    suspect_text_layer: Option<String>,
    generated: bool,
}

/// Mints spans for `extracted.blocks[blocks]`, inside the caller's transaction, and returns
/// their ids in that order. It first takes the artifact's row `FOR SHARE` (the artifact lock
/// contract: a deleted file is `NotFound` and mints nothing). A block already minted keeps its
/// id; a stored row whose text hash, hidden reason, suspect reason or generated flag differs
/// from the fresh block is `SpanMismatch` (a reader changed its output without a version bump),
/// and the caller must not commit. Two blocks of one extraction with one key are refused the
/// same way, before anything is inserted.
pub async fn mint_on(
    conn: &mut PgConnection,
    tenant: &Tenant,
    version: &VersionView,
    role: ArtifactRole,
    extracted: &Extracted,
    blocks: Range<usize>,
) -> Result<Vec<SpanId>, KatError> {
    store::lock_source_on(conn, tenant, &[version.artifact_id]).await?;
    let mut seen = HashSet::with_capacity(extracted.blocks.len());
    if !extracted.blocks.iter().all(|b| seen.insert(block_key(b))) {
        tracing::error!(version_id = %version.id, extractor = %extracted.extractor.name,
            "duplicate_block_key");
        return Err(KatError::SpanMismatch);
    }
    let page = extracted
        .blocks
        .get(blocks)
        .ok_or(KatError::RequestInvalid)?;
    if page.is_empty() {
        return Ok(vec![]);
    }
    let generated = role == ArtifactRole::Output;
    let n = page.len();
    let mut ids = Vec::with_capacity(n);
    let (mut parts, mut ordinals, mut pieces) = (
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
    );
    let (mut refs, mut texts, mut hashes, mut keys) = (
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
    );
    let (mut hidden, mut instruction, mut suspect) = (
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
    );
    let mut expected = Vec::with_capacity(n);
    for block in page {
        let page_info = page_of(extracted, block);
        let source_ref =
            SourceRef::for_block(version, role, extracted.extractor.clone(), block, page_info);
        let text = block.text.replace('\0', "\u{FFFD}");
        let (part, ordinal, piece) = block_key(block);
        let snapshot = Snapshot {
            text_sha256: sha256(&text),
            hidden: block.hidden.map(hidden_wire),
            suspect_text_layer: suspect_reason(block, page_info).map(str::to_owned),
            generated,
        };
        ids.push(SpanId::new().0);
        parts.push(part.clone());
        ordinals.push(i32::try_from(ordinal).map_err(|_| KatError::RequestInvalid)?);
        pieces.push(i32::try_from(piece).map_err(|_| KatError::RequestInvalid)?);
        // `to_string` writes an `f32` in its shortest form (301.2), which jsonb keeps exactly;
        // a `serde_json::Value` would carry its widened `f64` (301.20001220703125).
        refs.push(serde_json::to_string(&source_ref).map_err(|_| KatError::RequestInvalid)?);
        keys.push(match_key(&text).text);
        texts.push(text);
        hashes.push(snapshot.text_sha256.clone());
        hidden.push(snapshot.hidden.clone());
        instruction.push(block_instruction_like(block));
        suspect.push(snapshot.suspect_text_layer.clone());
        expected.push(((part, ordinal, piece), snapshot));
    }
    let workspace = tenant.workspace_id();
    sqlx::query(
        "INSERT INTO source_spans (id, workspace_id, artifact_version_id, extractor,
             extractor_version, part, ordinal, piece, source_ref, text, text_sha256, match_key,
             normalize_version, generated, hidden, instruction_like, suspect_text_layer)
         SELECT u.id, $2, $3, $4, $5, u.part, u.ordinal, u.piece, u.source_ref::jsonb, u.text,
                u.text_sha256, u.match_key, $6, $7, u.hidden, u.instruction_like, u.suspect
         FROM UNNEST($1::uuid[], $8::text[], $9::int[], $10::int[], $11::text[], $12::text[],
                     $13::text[], $14::text[], $15::text[], $16::text[], $17::text[])
              AS u(id, part, ordinal, piece, source_ref, text, text_sha256, match_key, hidden,
                   instruction_like, suspect)
         ON CONFLICT ON CONSTRAINT source_spans_block_key DO NOTHING",
    )
    .bind(&ids)
    .bind(workspace)
    .bind(version.id)
    .bind(extracted.extractor.name.as_ref())
    .bind(i32::try_from(extracted.extractor.version).map_err(|_| KatError::RequestInvalid)?)
    .bind(i32::try_from(NORMALIZE_VERSION).map_err(|_| KatError::RequestInvalid)?)
    .bind(generated)
    .bind(&parts)
    .bind(&ordinals)
    .bind(&pieces)
    .bind(&refs)
    .bind(&texts)
    .bind(&hashes)
    .bind(&keys)
    .bind(&hidden)
    .bind(&instruction)
    .bind(&suspect)
    .execute(&mut *conn)
    .await?;
    // `ON CONFLICT DO NOTHING` returns no row for a block minted before: read every key back.
    let rows: Vec<StoredRow> = sqlx::query_as(
        "SELECT s.id, s.part, s.ordinal, s.piece, s.text_sha256, s.hidden, s.suspect_text_layer,
                s.generated
         FROM source_spans s
         JOIN UNNEST($5::text[], $6::int[], $7::int[]) AS u(part, ordinal, piece)
           ON s.part = u.part AND s.ordinal = u.ordinal AND s.piece = u.piece
         WHERE s.workspace_id = $1 AND s.artifact_version_id = $2 AND s.extractor = $3
           AND s.extractor_version = $4",
    )
    .bind(workspace)
    .bind(version.id)
    .bind(extracted.extractor.name.as_ref())
    .bind(i32::try_from(extracted.extractor.version).map_err(|_| KatError::RequestInvalid)?)
    .bind(&parts)
    .bind(&ordinals)
    .bind(&pieces)
    .fetch_all(&mut *conn)
    .await?;
    let stored: HashMap<BlockKey, StoredRow> = rows
        .into_iter()
        .map(|row| {
            (
                (row.part.clone(), row.ordinal as u32, row.piece as u32),
                row,
            )
        })
        .collect();
    expected
        .into_iter()
        .map(|(key, snapshot)| {
            let row = stored.get(&key).ok_or(KatError::SpanMismatch)?;
            if row.snapshot() != snapshot {
                tracing::error!(version_id = %version.id, span_id = %row.id, "span_mismatch");
                return Err(KatError::SpanMismatch);
            }
            Ok(row.id)
        })
        .collect()
}

#[derive(sqlx::FromRow)]
struct StoredRow {
    id: SpanId,
    part: String,
    ordinal: i32,
    piece: i32,
    text_sha256: String,
    hidden: Option<String>,
    suspect_text_layer: Option<String>,
    generated: bool,
}

impl StoredRow {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text_sha256: self.text_sha256.clone(),
            hidden: self.hidden.clone(),
            suspect_text_layer: self.suspect_text_layer.clone(),
            generated: self.generated,
        }
    }
}

#[derive(sqlx::FromRow)]
struct LookupRow {
    id: SpanId,
    artifact_id: ArtifactId,
    artifact_version_id: ArtifactVersionId,
    file_name: String,
    part: String,
    ordinal: i32,
    piece: i32,
    text: String,
    source_ref: sqlx::types::Json<SourceRef>,
    generated: bool,
    hidden: Option<String>,
    instruction_like: Option<String>,
    suspect_text_layer: Option<String>,
}

/// A span of a live file of `space_id` in the tenant's workspace. Another workspace's or
/// Space's span, or one over a deleted file, is `NotFound`.
pub async fn get_in_space_on(
    conn: &mut PgConnection,
    tenant: &Tenant,
    space_id: SpaceId,
    id: SpanId,
) -> Result<SpanRow, KatError> {
    let row: LookupRow = sqlx::query_as(
        "SELECT s.id, a.id AS artifact_id, s.artifact_version_id, a.name AS file_name, s.part,
                s.ordinal, s.piece, s.text, s.source_ref, s.generated, s.hidden,
                s.instruction_like, s.suspect_text_layer
         FROM source_spans s
         JOIN artifact_versions v ON v.id = s.artifact_version_id AND v.workspace_id = s.workspace_id
         JOIN artifacts a ON a.id = v.artifact_id AND a.workspace_id = v.workspace_id
         WHERE s.workspace_id = $1 AND s.id = $2 AND a.space_id = $3 AND a.deleted_at IS NULL",
    )
    .bind(tenant.workspace_id())
    .bind(id)
    .bind(space_id)
    .fetch_optional(conn)
    .await?
    .ok_or(KatError::NotFound)?;
    Ok(SpanRow {
        id: row.id,
        artifact_id: row.artifact_id,
        artifact_version_id: row.artifact_version_id,
        file_name: row.file_name,
        part: row.part,
        ordinal: row.ordinal as u32,
        piece: row.piece as u32,
        text: row.text,
        source_ref: row.source_ref.0,
        generated: row.generated,
        hidden: row.hidden,
        instruction_like: row.instruction_like,
        suspect_text_layer: row.suspect_text_layer,
    })
}
