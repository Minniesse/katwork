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

mod arithmetic;
pub mod boundary;
mod cell;
mod date;
mod number;
pub mod qualifier;
mod quote;
mod resolve;
mod rollup;
mod scan;
pub mod subject;

pub use qualifier::check_qualifiers;
pub use quote::check_quote;
pub use resolve::{check_resolve, relocate};
pub use rollup::{flags, is_disputed, roll_up};

/// The refs of one claim that get number or date checks; each one past it gets one
/// `Inconclusive` check, `too_many_refs`, and a derived claim with more operands gets one too.
pub(crate) const MAX_FIGURE_REFS: usize = 16;

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
    /// Reserved for sub-project 3; nothing produces it yet. Its `Contradicted` and `Unsupported`
    /// lower a claim like a deterministic check's, but its support never passes one.
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
#[expect(
    clippy::large_enum_variant,
    reason = "the plan's interface; a claim holds a handful of refs, built once per pass"
)]
pub enum ResolvedRef {
    Ok {
        exact: String,
        block_text: String,
        quote_start: u32,
        quote_end: u32,
        unit_context: Option<String>,
        cell: Option<CellFacts>,
        /// The sheet of a cell ref, from its locator (Task 12 fills it); `None` elsewhere.
        sheet: Option<String>,
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

/// Every check of one claim: Task 10's `check_resolve`, `check_quote` and `check_qualifiers`,
/// then, on the refs whose `Resolve` check is `Supported`, the figure checks. A claim text
/// holding exactly one `Quantity` (an amount in words is not counted) gets a number check per
/// ref, its dates then being periods of its subject; one holding dates and no quantity gets a
/// date check per ref; one with several quantities and no `derive` is `claim_number_ambiguous`;
/// a derived claim gets one `Arithmetic` check and no per-ref number check. Each amount in
/// words of the claim gets an `AmountWords` check. By content, never by `kind`. An uncited
/// claim gets no check at all, and a claim none of whose refs resolved gets only Task 10's.
pub fn check_claim(claim: &ClaimInput, ctx: &katwork_thai::Ctx) -> Vec<CheckOutcome> {
    if !claim.cited {
        return vec![];
    }
    let mut out = check_resolve(&claim.refs);
    out.extend(check_quote(claim, ctx));
    out.extend(check_qualifiers(claim));
    let refs: Vec<(u32, &ResolvedRef)> = resolved_refs(&claim.refs).collect();
    if !refs.is_empty() {
        out.extend(figure_checks(claim, &refs, ctx));
    }
    out
}

fn figure_checks(
    claim: &ClaimInput,
    refs: &[(u32, &ResolvedRef)],
    ctx: &katwork_thai::Ctx,
) -> Vec<CheckOutcome> {
    let read = katwork_thai::find_numbers(&claim.text, ctx)
        .and_then(|found| Ok((found, subject::terms(&claim.text, ctx)?)));
    let (found, subject) = match read {
        Ok(read) => read,
        Err(e) => {
            let stop = number::too_long(CheckMethod::Number)(e);
            return refs
                .iter()
                .map(|(ordinal, _)| number::stopped(stop.clone(), Some(*ordinal)))
                .collect();
        }
    };
    let mut out: Vec<CheckOutcome> = number::amount_words(&found)
        .map(|(figure, words)| {
            let equal = figure.value.effective() == words.value.effective();
            CheckOutcome {
                method: CheckMethod::AmountWords,
                verdict: if equal {
                    CheckVerdict::Supported
                } else {
                    CheckVerdict::Contradicted
                },
                basis: Basis::None,
                code: if equal {
                    "checked"
                } else {
                    "amount_words_mismatch"
                },
                ref_ordinal: None,
                advisory: false,
                detail: serde_json::json!({"figure": figure.raw, "words": words.raw}),
            }
        })
        .collect();
    let quantities: Vec<(usize, &katwork_thai::Found<katwork_thai::Number>)> =
        number::counted(&found).collect();
    let ambiguous = || {
        number::stopped(
            (
                CheckMethod::Number,
                "claim_number_ambiguous",
                serde_json::json!({"quantities": quantities.len()}),
            ),
            None,
        )
    };
    // Every figure `find_numbers` returns fits a `Decimal` at its own scale.
    let one = match quantities.as_slice() {
        [(i, n)] => number::Qty::read(n, Some(*i), &None, &claim.text).map(|value| (*i, value)),
        _ => None,
    };
    let single = one.is_some();
    let side = number::ClaimSide::new(&claim.text, &subject, &found, one);
    // Each ref is read once; refs past the cap are not compared at all.
    let mut each = |method: CheckMethod, compare: &dyn Fn(&scan::Scan) -> Vec<CheckOutcome>| {
        for (k, (ordinal, r)) in refs.iter().enumerate() {
            let at = Some(*ordinal);
            if k >= MAX_FIGURE_REFS {
                let detail = serde_json::json!({"cap": MAX_FIGURE_REFS});
                out.push(number::stopped((method, "too_many_refs", detail), at));
                continue;
            }
            match scan::read(*ordinal, r, method, ctx) {
                Ok(read) => out.extend(compare(&read)),
                Err(stop) => out.push(number::stopped(stop, at)),
            }
        }
    };
    match (&claim.derive, single, quantities.as_slice()) {
        (Some(derive), true, _) => out.extend(arithmetic::check(claim, &side, derive, ctx)),
        (None, true, _) => each(CheckMethod::Number, &|read| number::check(&side, read, ctx)),
        (None, false, []) if !subject.periods.is_empty() => {
            each(CheckMethod::Date, &|read| date::check(&side, read));
        }
        (None, false, []) => {}
        _ => out.push(ambiguous()),
    }
    out
}
