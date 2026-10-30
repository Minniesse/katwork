//! Document readers. Files are hostile input parsed in the server process, so every read runs
//! on `DocPool`'s dedicated threads, under `DocLimits`, the job deadline and its memory budget
//! (`deadline::check`).
use std::{borrow::Cow, ops::Range, sync::Arc};

use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;

pub mod colour;
pub mod deadline;
mod docx;
pub mod heap;
pub mod limits;
pub mod package;
pub mod pdf;
pub mod pdf_guard;
pub mod pdf_layer;
pub mod pdf_visibility;
pub mod pool;
mod text;
pub mod watchdog;
pub mod xml_guard;

pub use limits::DocLimits;
pub use pool::DocPool;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    Pdf,
    Docx,
    Xlsx,
    Text,
    Image,
}

/// Why a read failed. The `&'static str` names the cap or rule, in snake_case, and never holds
/// file content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DocError {
    #[error("file_too_large ({0})")]
    TooLarge(&'static str),
    #[error("file_too_complex ({0})")]
    TooComplex(&'static str),
    #[error("file_malformed ({0})")]
    Malformed(&'static str),
    #[error("file_encrypted")]
    Encrypted,
    #[error("file_type_unsupported ({0})")]
    Unsupported(&'static str),
    /// The caller's deadline passed.
    #[error("doc_timeout")]
    Timeout,
    /// The pool's own `call_timeout` fired, counted from when a worker picked the job up.
    #[error("doc_timeout (call_timeout)")]
    CallTimeout,
    #[error("cancelled")]
    Cancelled,
    #[error("internal_error")]
    Internal,
}

impl DocError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::TooLarge(_) => "file_too_large",
            Self::TooComplex(_) => "file_too_complex",
            Self::Malformed(_) => "file_malformed",
            Self::Encrypted => "file_encrypted",
            Self::Unsupported(_) => "file_type_unsupported",
            Self::Timeout | Self::CallTimeout => "doc_timeout",
            Self::Cancelled => "cancelled",
            Self::Internal => "internal_error",
        }
    }
}

/// The reader that produced a block, and the version of its rules. A change to what a reader
/// extracts is a new version.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExtractorId {
    pub name: Cow<'static, str>,
    pub version: u32,
}

impl ExtractorId {
    pub const TEXT: ExtractorId = ExtractorId {
        name: Cow::Borrowed("text"),
        version: 1,
    };
    pub const DOCX: ExtractorId = ExtractorId {
        name: Cow::Borrowed("docx-accepted"),
        version: 1,
    };
    pub const PDF: ExtractorId = ExtractorId {
        name: Cow::Borrowed("pdf-text"),
        version: 1,
    };
}

/// The PDF reader's extractor (`ExtractorId::PDF`).
pub const PDF: ExtractorId = ExtractorId::PDF;

/// A longer block is split into pieces of at most this many code points (see `split_pieces`).
pub const MAX_BLOCK_CHARS: usize = 1_500;

#[derive(Debug, Clone, PartialEq)]
pub struct Extracted {
    pub extractor: ExtractorId,
    pub blocks: Vec<Block>,
    pub pages: Vec<PageInfo>,
    pub warnings: Vec<ExtractWarning>,
    pub workbook: Option<WorkbookFacts>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub ordinal: u32,
    pub piece: u32,
    /// U+0000 is replaced with U+FFFD.
    pub text: String,
    pub locator: Locator,
    pub lines: Vec<LineBox>,
    pub cell: Option<CellFacts>,
    pub unit_context: Option<String>,
    pub hidden: Option<HiddenWhy>,
    /// Why a visible block's text may differ from what a person sees; a span over it is never
    /// full quality.
    pub suspect: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HiddenWhy {
    InvisibleMode,
    OptionalContentOff,
    OutsideCropBox,
    Clipped,
    Transparent,
    Tiny,
    LowContrast,
    Covered,
    UnknownPaint,
    HiddenRow,
    HiddenColumn,
    HiddenSheet,
    EmptyFormat,
}

impl HiddenWhy {
    /// The wire name, as serde writes it.
    pub fn as_str(self) -> &'static str {
        match self {
            HiddenWhy::InvisibleMode => "invisible_mode",
            HiddenWhy::OptionalContentOff => "optional_content_off",
            HiddenWhy::OutsideCropBox => "outside_crop_box",
            HiddenWhy::Clipped => "clipped",
            HiddenWhy::Transparent => "transparent",
            HiddenWhy::Tiny => "tiny",
            HiddenWhy::LowContrast => "low_contrast",
            HiddenWhy::Covered => "covered",
            HiddenWhy::UnknownPaint => "unknown_paint",
            HiddenWhy::HiddenRow => "hidden_row",
            HiddenWhy::HiddenColumn => "hidden_column",
            HiddenWhy::HiddenSheet => "hidden_sheet",
            HiddenWhy::EmptyFormat => "empty_format",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Locator {
    /// Code points of the decoded file.
    Text {
        start: u32,
        end: u32,
    },
    PdfPage {
        page: u32,
    },
    /// `part` is the package part's name (`word/document.xml`), shared by every locator of the
    /// part; `ordinal` counts every `w:p` of that part in document order, from 1.
    DocxParagraph {
        part: Arc<str>,
        ordinal: u32,
        para_id: Option<String>,
    },
    XlsxCell {
        sheet_id: u32,
        sheet: String,
        row: u32,
        col: u32,
    },
}

/// Code points within the block's text, and a rectangle in PDF user space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineBox {
    pub start: u32,
    pub end: u32,
    pub page: u32,
    pub rect: [f32; 4],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CellFacts {
    pub kind: CellKind,
    pub cached: Option<String>,
    pub displayed: Option<String>,
    pub formula: Option<String>,
    pub number_format: Option<String>,
    pub date1904: bool,
    pub hidden: bool,
    pub row_header: Option<String>,
    pub col_header: Option<String>,
    pub header_unit: Option<String>,
    pub cache_untrusted: bool,
}

/// The type of a cell's stored value or formula result, from its `t` attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellKind {
    Number,
    Text,
    Bool,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkbookFacts {
    pub full_calc_on_load: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PageInfo {
    pub page: u32,
    pub media_box: [f32; 4],
    pub crop_box: [f32; 4],
    pub text_layer: TextLayer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextLayer {
    Ok,
    Suspect(&'static str),
    Absent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractWarning {
    /// Hidden text left out of the block at `at` (for DOCX, the paragraph, named by its part and
    /// ordinal, since ordinals restart in each part).
    HiddenContent {
        at: Locator,
        why: &'static str,
    },
    SuspectTextLayer {
        page: u32,
        why: &'static str,
    },
    NoTextLayer {
        page: u32,
    },
    ActiveContentIgnored {
        what: &'static str,
    },
    EmbeddedContentIgnored {
        what: &'static str,
    },
    FullCalcOnLoad,
}

/// Reads `bytes` as `kind`. Synchronous and bounded by `limits`; callers run it inside
/// `DocPool::run`, whose deadline every reader loop checks.
pub fn extract(kind: MediaKind, bytes: &[u8], limits: &DocLimits) -> Result<Extracted, DocError> {
    match kind {
        MediaKind::Text => text::read(bytes, limits),
        MediaKind::Docx => docx::read(bytes, limits),
        MediaKind::Xlsx => Err(DocError::Unsupported("xlsx")),
        MediaKind::Pdf => pdf::read(bytes, limits),
        MediaKind::Image => Err(DocError::Unsupported("image")),
    }
}

/// Byte ranges of `text`'s pieces, in order. A piece ends after the last whitespace-led grapheme
/// that fits in `MAX_BLOCK_CHARS` code points, else at the last grapheme boundary that fits; a
/// single grapheme longer than the limit stays whole, because no piece splits a grapheme.
pub(crate) fn split_pieces(text: &str) -> Result<Vec<Range<usize>>, DocError> {
    let mut pieces = Vec::new();
    // The piece's start, its code points so far, and the last cut after whitespace with the
    // code points before it.
    let (mut start, mut count) = (0, 0);
    let mut space: Option<(usize, usize)> = None;
    for (index, (offset, grapheme)) in text.grapheme_indices(true).enumerate() {
        if index % 1024 == 1023 {
            deadline::check()?;
        }
        let chars = grapheme.chars().count();
        while count > 0 && count + chars > MAX_BLOCK_CHARS {
            let (cut, before) = space.take().unwrap_or((offset, count));
            pieces.push(start..cut);
            start = cut;
            count -= before;
        }
        count += chars;
        if grapheme.starts_with(char::is_whitespace) {
            space = Some((offset + grapheme.len(), count));
        }
    }
    if start < text.len() {
        pieces.push(start..text.len());
    }
    Ok(pieces)
}

/// Block text never holds U+0000 (Postgres text cannot).
pub(crate) fn replace_nul(text: &str) -> String {
    text.replace('\0', "\u{FFFD}")
}
