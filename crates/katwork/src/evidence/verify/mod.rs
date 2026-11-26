//! The verification ladder (ADR-0007): resolve each ref and cap its quality, compare the claim
//! with the refs that resolved with full quality, and roll the checks up into one status. Pure
//! functions, no I/O: Task 12 loads the refs and stores the checks.
//!
//! The one rule everything here keeps: only a ref whose `Resolve` check is `Supported` is ever
//! compared, so a generated, hidden, instruction-like, suspect, deleted or weak-OCR span leaves
//! its claim unverified and can never fail it.
use serde::Serialize;

use super::claim::{ClaimKind, Derive};
use crate::docs::CellFacts;

pub mod qualifier;
mod quote;
mod resolve;
mod rollup;

pub use qualifier::check_qualifiers;
pub use quote::check_quote;
pub use resolve::{check_resolve, relocate};
pub use rollup::{flags, is_disputed, roll_up};

/// Stored on every check row; a change to what these functions return needs a new version.
pub const VERIFIER_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckMethod {
    Resolve,
    Quote,
    Qualifier,
    Number,
    Date,
    Cell,
    Arithmetic,
    AmountWords,
    Judge,
    /// Reserved for sub-project 3; nothing produces it yet. `roll_up` treats it as deterministic.
    Human,
}

impl CheckMethod {
    pub fn wire(self) -> &'static str {
        match self {
            CheckMethod::Resolve => "resolve",
            CheckMethod::Quote => "quote",
            CheckMethod::Qualifier => "qualifier",
            CheckMethod::Number => "number",
            CheckMethod::Date => "date",
            CheckMethod::Cell => "cell",
            CheckMethod::Arithmetic => "arithmetic",
            CheckMethod::AmountWords => "amount_words",
            CheckMethod::Judge => "judge",
            CheckMethod::Human => "human",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckVerdict {
    Supported,
    Unsupported,
    Contradicted,
    Inconclusive,
}

impl CheckVerdict {
    pub fn wire(self) -> &'static str {
        match self {
            CheckVerdict::Supported => "supported",
            CheckVerdict::Unsupported => "unsupported",
            CheckVerdict::Contradicted => "contradicted",
            CheckVerdict::Inconclusive => "inconclusive",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    Exact,
    Derived,
    Judged,
    None,
}

impl Basis {
    pub fn wire(self) -> &'static str {
        match self {
            Basis::Exact => "exact",
            Basis::Derived => "derived",
            Basis::Judged => "judged",
            Basis::None => "none",
        }
    }
}

/// One check of one claim (against one ref when `ref_ordinal` is set). `detail` holds the values
/// compared, as strings. An `advisory` check is stored and shown but never enters `roll_up` or
/// `is_disputed`.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckOutcome {
    pub method: CheckMethod,
    pub verdict: CheckVerdict,
    pub basis: Basis,
    pub code: &'static str,
    pub ref_ordinal: Option<u32>,
    pub advisory: bool,
    pub detail: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    Passed,
    Failed,
    Unverified,
}

impl ClaimStatus {
    pub fn wire(self) -> &'static str {
        match self {
            ClaimStatus::Passed => "passed",
            ClaimStatus::Failed => "failed",
            ClaimStatus::Unverified => "unverified",
        }
    }
}

/// How far a ref's text can be trusted, decided by the caller (Task 12) from the live
/// extraction, never from the quality snapshot stored in the `SourceRef` or the span row: the
/// instruction detector in particular is run again on the current block, because the stored flag
/// reflects the lexicon at minting time. When several apply, Task 12 picks one in the order
/// `Deleted`, `Hidden`, `Generated`, `InstructionLike`, `SuspectLayer`.
#[derive(Debug, Clone, PartialEq)]
pub enum RefQuality {
    Full,
    Generated,
    Hidden,
    InstructionLike,
    /// The reason the text layer is suspect (`source_ref::suspect_reason`); it goes to `detail`.
    SuspectLayer(&'static str),
    Deleted,
    Ocr {
        conf: Option<f32>,
        align_score: Option<f32>,
        boxed: bool,
        /// Task 41 caps the line's numbers; the resolve step ignores it.
        number_capped: bool,
    },
}

/// A claim's ref as the verifier sees it: the quote and the block's current text, with no ids, so
/// nothing here can look anything up. `quote_start..quote_end` is the quote's position in
/// `block_text`, in code points.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedRef {
    Ok {
        exact: String,
        block_text: String,
        quote_start: u32,
        quote_end: u32,
        unit_context: Option<String>,
        cell: Option<CellFacts>,
        quality: RefQuality,
    },
    Stale {
        code: &'static str,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaimInput {
    pub kind: ClaimKind,
    pub text: String,
    pub cited: bool,
    pub derive: Option<Derive>,
    pub refs: Vec<ResolvedRef>,
}

/// Each ref with its ordinal, kept only when it resolved with full quality: the only refs any
/// comparing check may read.
pub(crate) fn resolved_refs(refs: &[ResolvedRef]) -> impl Iterator<Item = (u32, &ResolvedRef)> {
    refs.iter()
        .enumerate()
        .filter(|(_, r)| resolve::is_resolved(r))
        .map(|(i, r)| (ordinal(i), r))
}

pub(crate) fn ordinal(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}
