//! The tools `write_docx` and `write_xlsx` (`Draft`, `Trusted`): arguments, plain checks,
//! rendering in the pool, then `commit_document` (SP2a Task 9). Trust core.
use std::{collections::BTreeSet, sync::Arc};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Connection;
use uuid::Uuid;

use super::{
    CommitReceipt, CommitRequest, commit_document, from_write, source_ids, sources_in_space_on,
    stored_receipt_on,
};
use crate::{
    agent::{StopReason, Tool, ToolCtx, ToolDeps, ToolError, ToolOutput, guards},
    artifacts::{MediaType, names, store, tools::from_doc},
    db,
    docs::{
        DocError, Rendered,
        content::{
            self, DocContent, SheetSpec, WrittenContent, check_doc, check_sheets, claim_specs,
        },
    },
    error::KatError,
    evidence::citation::{self, CitationRow},
    ids::{ArtifactId, CitationId},
    models::ToolSpec,
    types::SideEffect,
};

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
    recorded as uncited and listed in the result, so cite it or leave it out. Put a figure's \
    sign, currency, %, ปี and scale words (ล้าน, million) inside the claim text: text next to a \
    claim that changes the figure a reader sees is recorded as uncited too. Bidi control \
    characters and right-to-left letters (Hebrew, Arabic) are refused. Pass `file_id` (an output of this job) to write a new version of it.";

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
        Self::with_renderer(deps, Arc::new(crate::docs::write_docx::render_docx))
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
        Self::with_renderer(deps, Arc::new(crate::docs::write_xlsx::render_xlsx))
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
}
