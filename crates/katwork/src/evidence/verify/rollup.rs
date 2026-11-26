//! One status per claim from its checks, the visible flag, and which claims may reach a judge.
//! The ladder only moves down: a judge can hold or lower a claim, never lift or fail it (R18).
use super::{
    Basis, CheckMethod, CheckOutcome, CheckVerdict, ClaimInput, ClaimStatus, ResolvedRef,
    resolve::is_resolved,
};
use crate::evidence::codes::FLAG_AI_POSSIBLE_MISMATCH;

/// The codes of an open, undecided claim a judge may read.
const OPEN_CODES: &[&str] = &["paraphrase", "needs_judge", "subject_not_in_source"];

fn counted(checks: &[CheckOutcome]) -> impl Iterator<Item = &CheckOutcome> + Clone {
    checks.iter().filter(|c| !c.advisory)
}

fn is_judge(c: &CheckOutcome) -> bool {
    c.method == CheckMethod::Judge
}

fn exact_or_derived(c: &CheckOutcome) -> bool {
    c.verdict == CheckVerdict::Supported && matches!(c.basis, Basis::Exact | Basis::Derived)
}

/// The claim's status and code, skipping advisory checks, by the first rule that applies:
/// 1. a non-judge `Contradicted`: `Failed` with its code;
/// 2. not cited: `uncited`;
/// 3. a `Resolve` check not `Supported`: its code;
/// 4. a non-judge `Unsupported`: its code;
/// 5. a non-judge, non-resolve check `Supported` with an exact or derived basis, no
///    `Inconclusive` qualifier, number, date, cell or arithmetic check, and no judge
///    `Contradicted` or `Unsupported`: `Passed`, `checked`;
/// 6. to 8. a judge `Contradicted`, `Unsupported`, `Supported`: `judge_contradicted`,
///    `judge_unsupported`, `judged_supported`;
/// 9. the first `Inconclusive` code, else `needs_review`.
///
/// Every status but rule 1's and rule 5's is `Unverified`.
pub fn roll_up(cited: bool, checks: &[CheckOutcome]) -> (ClaimStatus, &'static str) {
    use CheckVerdict::*;
    let checks = counted(checks);
    let unverified = |code| (ClaimStatus::Unverified, code);
    if let Some(c) = checks
        .clone()
        .find(|c| !is_judge(c) && c.verdict == Contradicted)
    {
        return (ClaimStatus::Failed, c.code);
    }
    if !cited {
        return unverified("uncited");
    }
    if let Some(c) = checks
        .clone()
        .find(|c| c.method == CheckMethod::Resolve && c.verdict != Supported)
    {
        return unverified(c.code);
    }
    if let Some(c) = checks
        .clone()
        .find(|c| !is_judge(c) && c.verdict == Unsupported)
    {
        return unverified(c.code);
    }
    let judge = |verdict| checks.clone().any(|c| is_judge(c) && c.verdict == verdict);
    let supported = checks
        .clone()
        .any(|c| !is_judge(c) && c.method != CheckMethod::Resolve && exact_or_derived(c));
    let blocking = checks.clone().any(|c| {
        c.verdict == Inconclusive
            && matches!(
                c.method,
                CheckMethod::Qualifier
                    | CheckMethod::Number
                    | CheckMethod::Date
                    | CheckMethod::Cell
                    | CheckMethod::Arithmetic
            )
    });
    if supported && !blocking && !judge(Contradicted) && !judge(Unsupported) {
        return (ClaimStatus::Passed, "checked");
    }
    if judge(Contradicted) {
        return unverified("judge_contradicted");
    }
    if judge(Unsupported) {
        return unverified("judge_unsupported");
    }
    if judge(Supported) {
        return unverified("judged_supported");
    }
    unverified(
        checks
            .clone()
            .find(|c| c.verdict == Inconclusive)
            .map_or("needs_review", |c| c.code),
    )
}

/// `["ai_possible_mismatch"]` when an advisory check is `Contradicted` or `Unsupported`, or a
/// judge is `Contradicted` (R4, R18); otherwise empty. It never changes a status.
pub fn flags(checks: &[CheckOutcome]) -> Vec<&'static str> {
    let flagged = checks.iter().any(|c| {
        (c.advisory
            && matches!(
                c.verdict,
                CheckVerdict::Contradicted | CheckVerdict::Unsupported
            ))
            || (!c.advisory && is_judge(c) && c.verdict == CheckVerdict::Contradicted)
    });
    if flagged {
        vec![FLAG_AI_POSSIBLE_MISMATCH]
    } else {
        vec![]
    }
}

/// Thai script, digits and signs included (R3: such a claim never reaches a judge).
fn has_thai(text: &str) -> bool {
    text.chars().any(|c| ('\u{0E00}'..='\u{0E7F}').contains(&c))
}

/// Every text of a ref a judge could be shown.
fn ref_has_thai(r: &ResolvedRef) -> bool {
    let ResolvedRef::Ok {
        exact,
        block_text,
        unit_context,
        cell,
        ..
    } = r
    else {
        return false;
    };
    let cell_texts = cell.iter().flat_map(|c| {
        [
            &c.cached,
            &c.displayed,
            &c.formula,
            &c.number_format,
            &c.row_header,
            &c.col_header,
            &c.header_unit,
        ]
    });
    has_thai(exact)
        || has_thai(block_text)
        || unit_context.as_deref().is_some_and(has_thai)
        || cell_texts.flatten().any(|t| has_thai(t))
}

/// Whether a judge may read the claim: never when its text or any cited span holds Thai script;
/// otherwise when it is cited, every ref and every `Resolve` check resolved with full quality, no
/// check is `Contradicted`, none is `qualifier_mismatch`, none is `Supported` with an exact or
/// derived basis, and one is `Inconclusive` with `paraphrase`, `needs_judge` or
/// `subject_not_in_source`. Advisory checks are skipped.
pub fn is_disputed(claim: &ClaimInput, checks: &[CheckOutcome]) -> bool {
    if has_thai(&claim.text) || claim.refs.iter().any(ref_has_thai) {
        return false;
    }
    if !claim.cited || claim.refs.is_empty() || !claim.refs.iter().all(is_resolved) {
        return false;
    }
    let mut checks = counted(checks);
    let blocked = checks.clone().any(|c| {
        (c.method == CheckMethod::Resolve && c.verdict != CheckVerdict::Supported)
            || c.verdict == CheckVerdict::Contradicted
            || c.code == "qualifier_mismatch"
            || exact_or_derived(c)
    });
    !blocked
        && checks.any(|c| c.verdict == CheckVerdict::Inconclusive && OPEN_CODES.contains(&c.code))
}
