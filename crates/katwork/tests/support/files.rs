//! Files and read-tool fixtures: a blob directory per test, uploads, hand-built blocks and a
//! counting fake `extract`.
#![allow(dead_code, reason = "Shared by the evidence and agent tests")]

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use katwork::{
    agent::{ExtractFn, RunBudget, ToolCtx, ToolDeps},
    artifacts::{
        BlobStore, LocalBlobStore, MediaType,
        store::{self, ArtifactView},
    },
    config::Config,
    docs::{
        Block, CellFacts, CellKind, DocLimits, DocPool, Extracted, ExtractorId, LineBox, Locator,
        PageInfo, TextLayer,
    },
    ids::*,
    runtime::Core,
    tenancy::{self, Tenant},
};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::Fixture;

pub const LOCK: Duration = Duration::from_secs(5);

/// A directory under the system temp dir, removed when dropped.
pub struct TestDir(pub PathBuf);

impl TestDir {
    pub fn new() -> Self {
        let path = std::env::temp_dir().join(format!("katwork-evidence-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    pub fn store(&self) -> Arc<LocalBlobStore> {
        Arc::new(LocalBlobStore::new(self.0.join("artifacts")).unwrap())
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn config() -> Config {
    Config::from_lookup(|name| match name {
        "DATABASE_URL" => Some("postgres://unused".into()),
        "KATWORK_SECRET" => Some(Uuid::now_v7().to_string()),
        _ => None,
    })
    .unwrap()
}

/// A core with a blob store in `dir` and a two-thread pool with `limits`.
pub fn core(pool: &PgPool, dir: &TestDir, limits: DocLimits) -> Core {
    Core::new(pool.clone(), Arc::new(config()))
        .with_blob_store(dir.store())
        .with_docs(Arc::new(DocPool::new(2, limits).unwrap()))
}

pub async fn tenant(pool: &PgPool, f: &Fixture) -> Tenant {
    tenancy::resolve(pool, UserId(f.user), WorkspaceId(f.workspace))
        .await
        .unwrap()
}

pub async fn upload(
    pool: &PgPool,
    blobs: &dyn BlobStore,
    tenant: &Tenant,
    space: SpaceId,
    name: &str,
    media: MediaType,
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
        media,
        staged,
        u64::MAX,
        LOCK,
    )
    .await
    .unwrap()
}

/// A tool context for the fixture's run, with a 60 s deadline.
pub fn tool_ctx(pool: &PgPool, f: &Fixture, tenant: &Tenant) -> ToolCtx {
    ToolCtx {
        lock_timeout: LOCK,
        tenant: tenant.clone(),
        thread_id: ThreadId(f.thread),
        space_id: SpaceId(f.space),
        kat_id: KatId(f.kat),
        job_id: JobId(f.job),
        task_id: TaskId(f.task),
        run_id: RunId(f.run),
        step: 1,
        tool_call_id: "call".into(),
        idempotency_key: Uuid::now_v7().to_string(),
        lease_epoch: LeaseEpoch(0),
        policy_version: 1,
        actor: UserId(f.user),
        initiator: katwork::agent::Initiator::Person,
        computer: None,
        pool: pool.clone(),
        cancel: CancellationToken::new(),
        deadline: tokio::time::Instant::now() + Duration::from_secs(60),
        budget: RunBudget::new(200_000),
    }
}

/// A fake `extract` that returns `make()` and counts its calls.
pub fn counting(
    make: impl Fn() -> Result<Extracted, katwork::docs::DocError> + Send + Sync + 'static,
) -> (ExtractFn, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let extract: ExtractFn = Arc::new(move |_, _, _| {
        counter.fetch_add(1, Ordering::SeqCst);
        make()
    });
    (extract, calls)
}

/// The core's deps with `extract` in place of the real readers.
pub fn deps(core: &Core, extract: ExtractFn) -> ToolDeps {
    ToolDeps {
        extract,
        ..ToolDeps::from_core(core)
    }
}

pub fn extracted(extractor: ExtractorId, blocks: Vec<Block>) -> Extracted {
    Extracted {
        extractor,
        blocks,
        pages: vec![],
        warnings: vec![],
        workbook: None,
    }
}

pub fn text_block(ordinal: u32, text: &str) -> Block {
    Block {
        ordinal,
        piece: 0,
        text: text.into(),
        locator: Locator::Text { start: 0, end: 0 },
        lines: vec![],
        cell: None,
        unit_context: None,
        hidden: None,
        suspect: None,
    }
}

pub fn pdf_block(ordinal: u32, page: u32, text: &str) -> Block {
    let len = text.chars().count() as u32;
    Block {
        locator: Locator::PdfPage { page },
        lines: vec![LineBox {
            start: 0,
            end: len,
            page,
            rect: [72.0, 410.5, 301.2, 424.0],
        }],
        ..text_block(ordinal, text)
    }
}

pub fn page(page: u32, text_layer: TextLayer) -> PageInfo {
    PageInfo {
        page,
        media_box: [0.0, 0.0, 595.0, 842.0],
        crop_box: [0.0, 0.0, 595.0, 842.0],
        text_layer,
    }
}

pub fn cell_block(
    ordinal: u32,
    sheet_id: u32,
    sheet: &str,
    row: u32,
    col: u32,
    text: &str,
) -> Block {
    Block {
        locator: Locator::XlsxCell {
            sheet_id,
            sheet: sheet.into(),
            row,
            col,
        },
        cell: Some(CellFacts {
            kind: CellKind::Number,
            cached: Some(text.into()),
            displayed: Some(text.into()),
            formula: None,
            number_format: Some("#,##0".into()),
            date1904: false,
            hidden: false,
            row_header: Some("รายได้".into()),
            col_header: Some("2567".into()),
            header_unit: None,
            cache_untrusted: false,
        }),
        unit_context: Some("หน่วย: ล้านบาท".into()),
        ..text_block(ordinal, text)
    }
}

pub fn docx_block(part: &str, ordinal: u32, piece: u32, text: &str) -> Block {
    Block {
        piece,
        locator: Locator::DocxParagraph {
            part: part.into(),
            ordinal,
            para_id: Some("1A2B3C4D".into()),
        },
        ..text_block(ordinal, text)
    }
}

pub fn xlsx_extractor() -> ExtractorId {
    ExtractorId {
        name: "xlsx-umya".into(),
        version: 1,
    }
}

pub fn pdf_extractor() -> ExtractorId {
    ExtractorId {
        name: "pdf-text".into(),
        version: 1,
    }
}

pub fn calls(counter: &AtomicUsize) -> usize {
    counter.load(Ordering::SeqCst)
}
