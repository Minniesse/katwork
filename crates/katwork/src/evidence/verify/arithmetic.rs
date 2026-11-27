//! A derived figure: one aggregate comparison of the claim's value with the operation over its
//! cited operands, in `Decimal`, never operand-by-operand equality.
use std::collections::BTreeSet;

use katwork_thai::{Ctx, Unit};
use rust_decimal::Decimal;
use serde_json::json;

use super::number::{self, ClaimSide, Compared, Qty, round_to, settle, stopped};
use super::scan::{self, Scan};
use super::subject::{self, OP_WORDS, Source};
use super::{
    Basis, CheckMethod, CheckOutcome, CheckVerdict, ClaimInput, MAX_FIGURE_REFS, ordinal,
    resolve::is_resolved,
};
use crate::evidence::claim::{Derive, DeriveOp};

/// Words saying the change went up or down: a decrease word makes the claim's value negative.
const INCREASE: &[&str] = &["เพิ่มขึ้น", "increase", "increased", "rose", "grew"];
const DECREASE: &[&str] = &["ลดลง", "decrease", "decreased", "fell", "declined"];

/// The operands in `args` order (a, b): `sum` of all; `difference` = a − b; `ratio` = a ÷ b;
/// `share` = a ÷ b × 100; `percent_change` = (b − a) ÷ a × 100. `Err` for a division by zero (or an overflow).
fn compute(op: DeriveOp, values: &[Decimal]) -> Result<Decimal, &'static str> {
    let overflow = "arithmetic_mismatch";
    let pair = || match values {
        [a, b] => Ok((*a, *b)),
        _ => Err("operand_missing"),
    };
    let divide = |n: Decimal, d: Decimal| {
        if d.is_zero() {
            Err("division_by_zero")
        } else {
            n.checked_div(d).ok_or(overflow)
        }
    };
    let hundred = |v: Decimal| v.checked_mul(Decimal::ONE_HUNDRED).ok_or(overflow);
    match op {
        DeriveOp::Sum => values
            .iter()
            .try_fold(Decimal::ZERO, |acc, v| acc.checked_add(*v))
            .ok_or(overflow),
        DeriveOp::Difference => {
            let (a, b) = pair()?;
            a.checked_sub(b).ok_or(overflow)
        }
        DeriveOp::Ratio => {
            let (a, b) = pair()?;
            divide(a, b)
        }
        DeriveOp::Share => {
            let (a, b) = pair()?;
            hundred(divide(a, b)?)
        }
        DeriveOp::PercentChange => {
            let (a, b) = pair()?;
            hundred(divide(b.checked_sub(a).ok_or(overflow)?, a)?)
        }
    }
}

/// The `Arithmetic` check of a derived claim, plus a `Qualifier` check per operand whose fields
/// hold other negations or comparators than the claim. Each operand ref must resolve with full
/// quality and yield exactly one quantity, and no ref may be named twice (`operand_missing`);
/// their explicit units must agree with each other and with the claim (`operand_units_differ`).
/// The result, compared at the claim's precision, is `Supported`, `Derived`, or `Contradicted`,
/// `arithmetic_mismatch`; it is held open when only one side states a scale (`scale_unstated`),
/// when it matches only with the operands swapped (`operand_order`), or when it differs only in
/// sign from a claim with no direction word. The subject rule, the operation's own words left out
/// of the claim's, reads every operand on its own for a sum and their union otherwise (a change
/// spans two periods); a missed subject holds the check open.
pub(super) fn check(
    claim: &ClaimInput,
    side: &ClaimSide,
    derive: &Derive,
    ctx: &Ctx,
) -> Vec<CheckOutcome> {
    let open =
        |code: &'static str, detail| vec![stopped((CheckMethod::Arithmetic, code, detail), None)];
    let Some((_, value)) = &side.figure else {
        return vec![];
    };
    let distinct: BTreeSet<u8> = derive.args.iter().copied().collect();
    if distinct.len() != derive.args.len() {
        return open("operand_missing", json!({"reason": "repeated_operand"}));
    }
    if distinct.len() > MAX_FIGURE_REFS {
        return open("too_many_refs", json!({"operands": distinct.len()}));
    }
    let mut scans: Vec<Scan> = Vec::new();
    let mut operands: Vec<Qty> = Vec::new();
    for &arg in &derive.args {
        let index = usize::from(arg);
        let Some(r) = claim.refs.get(index).filter(|r| is_resolved(r)) else {
            return open("operand_missing", json!({"arg": arg}));
        };
        let scan = match scan::read(ordinal(index), r, CheckMethod::Arithmetic, ctx) {
            Ok(scan) => scan,
            Err((_, code, detail)) => return open(code, detail),
        };
        let found = match number::quantities(&scan, CheckMethod::Arithmetic) {
            Ok(found) => found,
            Err((_, code, detail)) => return open(code, detail),
        };
        let [qty] = found.as_slice() else {
            return open(
                "operand_missing",
                json!({"arg": arg, "quantities": found.len()}),
            );
        };
        operands.push(qty.clone());
        scans.push(scan);
    }
    let mut out: Vec<CheckOutcome> = Vec::new();
    let mut units: Vec<_> = operands.iter().filter_map(|q| q.unit.as_ref()).collect();
    units.dedup();
    // The claim's unit: a sum or a difference keeps the operands' unit, a share or a percent
    // change is a percent, a ratio has none.
    let expected = match derive.op {
        DeriveOp::Sum | DeriveOp::Difference => units.first().copied(),
        DeriveOp::Share | DeriveOp::PercentChange => Some(&Unit::Percent),
        DeriveOp::Ratio => None,
    };
    let claim_unit_differs = value.unit.as_ref().is_some_and(|u| Some(u) != expected);
    if units.len() > 1 || claim_unit_differs {
        return open(
            "operand_units_differ",
            json!({
                "units": units.iter().map(|u| format!("{u:?}")).collect::<Vec<_>>(),
                "claim": value.unit.as_ref().map(|u| format!("{u:?}")),
            }),
        );
    }
    let values: Vec<Decimal> = operands.iter().map(|q| q.effective).collect();
    let result = match compute(derive.op, &values) {
        Ok(result) => result,
        Err(code) => return open(code, json!({})),
    };
    let decrease = number::has_word(&claim.text, DECREASE);
    let increase = number::has_word(&claim.text, INCREASE);
    let claimed = if decrease && value.effective.is_sign_positive() {
        -value.effective
    } else {
        value.effective
    };
    let rounded = round_to(result, value.exp);
    let detail = json!({
        "claim": claimed.to_string(),
        "result": result.to_string(),
        "operands": values.iter().map(ToString::to_string).collect::<Vec<_>>(),
    });
    let words: Vec<_> = side
        .subject
        .words
        .iter()
        .filter(|w| !OP_WORDS.contains(&w.text.as_str()))
        .cloned()
        .collect();
    let periods = &side.subject.periods;
    let met = if derive.op == DeriveOp::Sum {
        scans
            .iter()
            .try_for_each(|scan| subject::check(&words, periods, &scan.source, false))
    } else {
        let mut union = Source::default();
        for scan in &scans {
            union.extend(scan.source.clone());
        }
        subject::check(&words, periods, &union, false)
    }
    // An operand's other subject leaves the result open, never contradicted.
    .map_err(|mut miss| {
        miss.verdict = CheckVerdict::Inconclusive;
        miss
    });
    let qualified = !side.counts.is_empty() || scans.iter().any(|s| !s.counts.is_empty());
    for scan in &scans {
        out.extend(number::field_qualifiers(&side.counts, scan));
    }
    // A claim of one significant digit against a result it does not equal exactly.
    let coarse = number::digits(value.raw) < 10 && result != claimed;
    let first = if rounded == Some(claimed) && coarse {
        stopped((CheckMethod::Arithmetic, "precision_differs", detail), None)
    } else if rounded == Some(claimed) {
        settle(
            Compared::Equal(Basis::Derived),
            met,
            CheckMethod::Arithmetic,
            None,
            detail,
        )
    } else if mixed_scales(derive.op, value, &operands) {
        stopped((CheckMethod::Arithmetic, "scale_unstated", detail), None)
    } else if swapped(derive.op, &values).and_then(|r| round_to(r, value.exp)) == Some(claimed) {
        stopped((CheckMethod::Arithmetic, "operand_order", detail), None)
    } else if !increase && !decrease && rounded.map(|r| -r) == Some(claimed) {
        stopped(
            (CheckMethod::Arithmetic, "arithmetic_mismatch", detail),
            None,
        )
    } else {
        settle(
            Compared::Differs("arithmetic_mismatch", qualified),
            met,
            CheckMethod::Arithmetic,
            None,
            detail,
        )
    };
    out.insert(0, first);
    out
}

/// Only one side states a scale: some operands do and others do not, or (for a sum or a
/// difference, whose result keeps the operands' scale) the claim does and no operand does, or
/// the reverse. In a ratio, a share or a change the operands' scales cancel.
fn mixed_scales(op: DeriveOp, claim: &Qty, operands: &[Qty]) -> bool {
    let stated: BTreeSet<bool> = operands.iter().map(|q| q.scale > 0).collect();
    let keeps_scale = matches!(op, DeriveOp::Sum | DeriveOp::Difference);
    stated.len() > 1 || (keeps_scale && stated.first().is_some_and(|s| *s != (claim.scale > 0)))
}

/// The result with the two operands swapped, for an operation whose order matters.
fn swapped(op: DeriveOp, values: &[Decimal]) -> Option<Decimal> {
    match (op, values) {
        (DeriveOp::Sum, _) => None,
        (_, [a, b]) => compute(op, &[*b, *a]).ok(),
        _ => None,
    }
}
