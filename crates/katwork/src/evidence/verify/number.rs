//! The number check: a claim's single quantity against a ref's quantities, its scale and unit
//! read from the figure, the cell's format, its header unit or the block's unit context.
use katwork_thai::{Ctx, Flags, Found, Number, NumberRole, TooLong, Unit, find_numbers, match_key};
use rust_decimal::{Decimal, RoundingStrategy};
use serde_json::{Value, json};

use super::cell::{self, CellValue};
use super::qualifier::{self, Meaning};
use super::subject::{self, MIN_CONF, Source, Subject};
use super::{Basis, CheckMethod, CheckOutcome, CheckVerdict, ResolvedRef, boundary};
use crate::evidence::instruction::instruction_like;

/// A quantity as the comparisons read it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Qty {
    /// The figure as written or shown, before its scale.
    pub raw: Decimal,
    pub effective: Decimal,
    pub scale: u8,
    pub unit: Option<Unit>,
    /// The power of ten of the last digit the figure states; `None` for a cell's stored value.
    pub exp: Option<i64>,
    /// The power of ten of the last digit of `raw`.
    pub raw_exp: Option<i64>,
    /// Read below the clean-paragraph confidence with another reading beside it.
    pub ambiguous: bool,
    /// Its index in `find_numbers` of the quote, for pairing comparators with it.
    pub index: Option<usize>,
}

impl Qty {
    /// A figure `find_numbers` read; a figure stating neither scale nor unit takes the
    /// context's (`header_unit`, else `unit_context`).
    pub fn read(n: &Found<Number>, index: Option<usize>, context: &Context) -> Qty {
        let v = &n.value;
        let (scale, unit) = match (v.scale.0, &v.unit, context) {
            (0, None, Some((scale, unit))) => (*scale, unit.clone()),
            _ => (v.scale.0, v.unit.clone()),
        };
        let decimals = i64::from(v.decimals);
        Qty {
            raw: v.value,
            effective: scaled(v.value, scale).unwrap_or(v.value),
            scale,
            unit,
            exp: Some(i64::from(scale) - decimals),
            raw_exp: Some(-decimals),
            ambiguous: n.conf < MIN_CONF && !n.alts.is_empty(),
            index,
        }
    }
}

/// `value` × 10^`scale`.
fn scaled(value: Decimal, scale: u8) -> Option<Decimal> {
    (0..scale).try_fold(value, |v, _| v.checked_mul(Decimal::TEN))
}

/// The scale and unit a cell's header unit (else the block's unit context) states, read as
/// the XLSX reader read it: the text after `1 ` gives `1` a scale or a unit. A text that
/// `instruction_like` flags states nothing.
pub(super) type Context = Option<(u8, Option<Unit>)>;

pub(super) fn context(r: &ResolvedRef, ctx: &Ctx) -> Context {
    let ResolvedRef::Ok {
        unit_context, cell, ..
    } = r
    else {
        return None;
    };
    let read = |text: &str| -> Context {
        if instruction_like(text).is_some() {
            return None;
        }
        let found = find_numbers(&format!("1 {}", text.trim()), ctx).ok()?;
        found
            .iter()
            .find(|n| n.start == 0 && (n.value.scale.0 > 0 || n.value.unit.is_some()))
            .map(|n| (n.value.scale.0, n.value.unit.clone()))
    };
    let header = cell
        .as_ref()
        .and_then(|c| c.header_unit.as_deref())
        .and_then(read);
    header.or_else(|| {
        unit_context.as_deref().and_then(|text| {
            let after = text.split_once(':').map_or(text, |(_, after)| after);
            read(after)
        })
    })
}

/// Why a ref yields no quantities to compare, as a check code.
pub(super) type Stop = (CheckMethod, &'static str, Value);

/// A ref's quantities: a `Number` cell's value, else the `Quantity` figures of the quote (a
/// `Bool`, `Error` or date cell has none). An amount in words is never one of them; one that
/// disagrees with the figure it restates makes the quote ambiguous (`amount_words_mismatch`).
/// Stops first at a cell fact that bars the value, a quote that cuts a figure, a text too long.
pub(super) fn quantities(
    r: &ResolvedRef,
    ctx: &Ctx,
    method: CheckMethod,
) -> Result<Vec<Qty>, Stop> {
    let ResolvedRef::Ok { exact, cell, .. } = r else {
        return Ok(vec![]);
    };
    if let Some(code) = cell.as_ref().and_then(cell::gate) {
        return Err((CheckMethod::Cell, code, json!({})));
    }
    if boundary::cuts(r, ctx) {
        return Err((method, "quote_cuts_number", json!({"quote": exact})));
    }
    let context = context(r, ctx);
    match cell.as_ref().map(cell::value) {
        Some(CellValue::Number {
            value,
            percent,
            thousands,
        }) => {
            let unit = if percent {
                Some(Unit::Percent)
            } else {
                context.as_ref().and_then(|(_, unit)| unit.clone())
            };
            let qty = if thousands > 0 {
                // The format shows the value in thousands, millions...: that is its stated scale.
                let scale = u8::try_from(thousands * 3).unwrap_or(u8::MAX);
                let shown =
                    (0..thousands).try_fold(value, |v, _| v.checked_div(Decimal::ONE_THOUSAND));
                Qty {
                    raw: shown.unwrap_or(value),
                    effective: value,
                    scale,
                    unit,
                    exp: None,
                    raw_exp: None,
                    ambiguous: false,
                    index: None,
                }
            } else {
                let scale = context.as_ref().map_or(0, |(scale, _)| *scale);
                Qty {
                    raw: value,
                    effective: scaled(value, scale).unwrap_or(value),
                    scale,
                    unit,
                    exp: None,
                    raw_exp: None,
                    ambiguous: false,
                    index: None,
                }
            };
            return Ok(vec![qty]);
        }
        Some(CellValue::Date(_) | CellValue::Nothing) => return Ok(vec![]),
        Some(CellValue::Quote) | None => {}
    }
    let found = find_numbers(exact, ctx).map_err(too_long(method))?;
    if let Some(words) = words_disagree(&found) {
        return Err((method, "amount_words_mismatch", words));
    }
    Ok(counted(&found)
        .map(|(i, n)| Qty::read(n, Some(i), &context))
        .collect())
}

pub(super) fn too_long(method: CheckMethod) -> impl Fn(TooLong) -> Stop {
    move |e| {
        (
            method,
            "text_too_long",
            json!({"code_points": e.code_points}),
        )
    }
}

/// The figures of a text that count among its quantities, with their indexes: every `Quantity`
/// that is not an amount in words.
pub(super) fn counted(found: &[Found<Number>]) -> impl Iterator<Item = (usize, &Found<Number>)> {
    found.iter().enumerate().filter(|(_, n)| {
        n.value.role == NumberRole::Quantity && !n.flags.contains(Flags::AMOUNT_IN_WORDS)
    })
}

/// Each amount in words of a text with the figure right before it that it restates.
pub(super) fn amount_words(
    found: &[Found<Number>],
) -> impl Iterator<Item = (&Found<Number>, &Found<Number>)> {
    found
        .iter()
        .enumerate()
        .filter(|(_, n)| n.flags.contains(Flags::AMOUNT_IN_WORDS))
        .filter_map(|(i, words)| {
            let figure = found[..i].iter().rev().find(|n| {
                n.value.role == NumberRole::Quantity && !n.flags.contains(Flags::AMOUNT_IN_WORDS)
            })?;
            Some((figure, words))
        })
}

fn words_disagree(found: &[Found<Number>]) -> Option<Value> {
    amount_words(found)
        .find(|(figure, words)| figure.value.effective() != words.value.effective())
        .map(|(figure, words)| json!({"figure": figure.raw, "words": words.raw}))
}

/// `value` rounded half away from zero to a multiple of 10^`exp` (Excel's rule); unchanged for
/// `None` (an exact value).
pub(super) fn round_to(value: Decimal, exp: Option<i64>) -> Option<Decimal> {
    let Some(exp) = exp else {
        return Some(value);
    };
    let strategy = RoundingStrategy::MidpointAwayFromZero;
    if exp <= 0 {
        let dp = u32::try_from(-exp).ok()?;
        return Some(if dp > 28 {
            value
        } else {
            value.round_dp_with_strategy(dp, strategy)
        });
    }
    let power = scaled(Decimal::ONE, u8::try_from(exp).ok()?)?;
    value
        .checked_div(power)?
        .round_dp_with_strategy(0, strategy)
        .checked_mul(power)
}

/// The significant digits of a value, trailing zeros dropped: 1,250 and 1.25 share them.
fn digits(value: Decimal) -> u128 {
    let mut m = value.normalize().mantissa().unsigned_abs();
    while m != 0 && m.is_multiple_of(10) {
        m /= 10;
    }
    m
}

/// How a source quantity stands against the claim's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rel {
    Equal,
    /// The raw values agree and only one side states the scale or the unit.
    ScaleUnstated,
    /// The claim states a finer value than the source, and they agree at the source's precision.
    PrecisionDiffers,
    UnitMismatch,
    /// The values agree but for their sign.
    SignDiffers,
    Mismatch,
}

fn relate(c: &Qty, s: &Qty) -> Rel {
    let units_eq = c.unit == s.unit;
    if units_eq && round_to(s.effective, c.exp) == Some(c.effective) {
        return Rel::Equal;
    }
    let one_unit = c.unit.is_some() != s.unit.is_some();
    let scales_differ = c.scale != s.scale;
    let raw_eq = round_to(s.raw, c.raw_exp) == Some(c.raw);
    if (scales_differ || one_unit) && (raw_eq || (scales_differ && digits(c.raw) == digits(s.raw)))
    {
        return Rel::ScaleUnstated;
    }
    if units_eq
        && let (Some(ce), Some(se)) = (c.exp, s.exp)
        && se > ce
        && round_to(c.effective, Some(se)) == Some(s.effective)
    {
        return Rel::PrecisionDiffers;
    }
    if c.unit.is_some() && s.unit.is_some() && !units_eq {
        return Rel::UnitMismatch;
    }
    if units_eq && round_to(-s.effective, c.exp) == Some(c.effective) {
        return Rel::SignDiffers;
    }
    Rel::Mismatch
}

/// The words that make a sign-only difference a matter of wording ("a loss of 1,250" against
/// "(1,250)"), not a contradiction.
const SIGN_WORDS: &[&str] = &[
    "ขาดทุน",
    "ติดลบ",
    "ลบ",
    "ลดลง",
    "loss",
    "losses",
    "deficit",
    "negative",
    "decrease",
    "decline",
];

pub(super) fn has_word(text: &str, list: &[&str]) -> bool {
    let key = match_key(text).text;
    let latin: Vec<String> = katwork_thai::normalize_th(text)
        .text
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_owned)
        .collect();
    list.iter().any(|w| {
        if w.is_ascii() {
            latin.iter().any(|l| l == w)
        } else {
            key.contains(w)
        }
    })
}

/// The comparison's result before the subject rule.
enum Decision<'a> {
    Equal(&'a Qty),
    /// A contradiction, if the subject agrees and no comparator qualifies the figures.
    Contradicts(&'static str),
    Open(&'static str),
    Absent,
}

fn decide<'a>(c: &Qty, sources: &'a [Qty], sign_words: bool) -> Decision<'a> {
    if c.ambiguous {
        return Decision::Open("number_ambiguous");
    }
    if let Some(s) = sources.iter().find(|s| relate(c, s) == Rel::Equal) {
        return if s.ambiguous {
            Decision::Open("number_ambiguous")
        } else {
            Decision::Equal(s)
        };
    }
    let [s] = sources else {
        return Decision::Absent;
    };
    if s.ambiguous {
        return Decision::Open("number_ambiguous");
    }
    match relate(c, s) {
        Rel::Equal => Decision::Equal(s),
        Rel::ScaleUnstated => Decision::Open("scale_unstated"),
        Rel::PrecisionDiffers => Decision::Open("precision_differs"),
        Rel::UnitMismatch => Decision::Contradicts("unit_mismatch"),
        Rel::SignDiffers if sign_words => Decision::Open("sign_differs"),
        Rel::SignDiffers | Rel::Mismatch => Decision::Contradicts("number_mismatch"),
    }
}

fn outcome(
    method: CheckMethod,
    verdict: CheckVerdict,
    basis: Basis,
    code: &'static str,
    ordinal: Option<u32>,
    detail: Value,
) -> CheckOutcome {
    CheckOutcome {
        method,
        verdict,
        basis,
        code,
        ref_ordinal: ordinal,
        advisory: false,
        detail,
    }
}

pub(super) fn stopped(stop: Stop, ordinal: Option<u32>) -> CheckOutcome {
    let (method, code, detail) = stop;
    outcome(
        method,
        CheckVerdict::Inconclusive,
        Basis::None,
        code,
        ordinal,
        detail,
    )
}

/// A comparison's result: equal (with the basis its support has), or different with the code
/// a contradiction carries and whether a comparator qualifies either side.
pub(super) enum Compared {
    Equal(Basis),
    Differs(&'static str, bool),
}

/// The verdict a comparison gives once the subject rule is applied: an equal figure is
/// `Supported` only when the subject is met (otherwise the subject's own verdict, which may be
/// `period_mismatch`); a differing figure contradicts only when the subject is met and no
/// comparator qualifies either side, and is otherwise held open.
pub(super) fn settle(
    compared: Compared,
    subject: Result<(), subject::Miss>,
    method: CheckMethod,
    ordinal: Option<u32>,
    detail: Value,
) -> CheckOutcome {
    use CheckVerdict::{Contradicted, Inconclusive, Supported};
    let with = |extra: Value| {
        let mut d = detail.clone();
        if let (Some(d), Value::Object(extra)) = (d.as_object_mut(), extra) {
            d.extend(extra);
        }
        d
    };
    match (compared, subject) {
        (Compared::Equal(basis), Ok(())) => {
            outcome(method, Supported, basis, "checked", ordinal, detail)
        }
        (Compared::Equal(_), Err(miss)) => outcome(
            method,
            miss.verdict,
            Basis::None,
            miss.code,
            ordinal,
            with(miss.detail),
        ),
        (_, Err(miss)) => outcome(
            method,
            Inconclusive,
            Basis::None,
            miss.code,
            ordinal,
            with(miss.detail),
        ),
        (Compared::Differs(code, true), Ok(())) => outcome(
            method,
            Inconclusive,
            Basis::None,
            code,
            ordinal,
            with(json!({"reason": "qualified"})),
        ),
        (Compared::Differs(code, false), Ok(())) => {
            outcome(method, Contradicted, Basis::None, code, ordinal, detail)
        }
    }
}

/// The comparator meanings that belong to the figure at `index` of a text: those paired with it
/// and those paired with no figure (a postfix comparator such as "or more" pairs with the next
/// figure, so an unpaired one is kept, to fail closed).
fn comparators(text: &str, index: usize, ctx: &Ctx) -> Option<Vec<Meaning>> {
    let mut found: Vec<Meaning> = qualifier::pair_with_numbers(text, ctx)
        .ok()?
        .into_iter()
        .filter(|(_, paired)| paired.is_none_or(|p| p == index))
        .map(|(q, _)| q.meaning)
        .collect();
    found.sort();
    Some(found)
}

/// The number check of a claim's one quantity (`claim_index` in `find_numbers` of its text)
/// against one full-quality ref, plus a `Qualifier` check when the figure the claim matched
/// carries other comparators than the claim's.
pub(super) fn check(
    claim_text: &str,
    claim: &Qty,
    claim_index: usize,
    subject: &Subject,
    ordinal: u32,
    r: &ResolvedRef,
    ctx: &Ctx,
) -> Vec<CheckOutcome> {
    let at = Some(ordinal);
    let sources = match quantities(r, ctx, CheckMethod::Number) {
        Ok(sources) => sources,
        Err(stop) => return vec![stopped(stop, at)],
    };
    let source = match Source::read(&subject::fields(r), ctx) {
        Ok(source) => source,
        Err(e) => return vec![stopped(too_long(CheckMethod::Number)(e), at)],
    };
    let ResolvedRef::Ok { exact, .. } = r else {
        return vec![];
    };
    let detail = json!({
        "claim": claim.effective.to_string(),
        "source": sources.iter().map(|s| s.effective.to_string()).collect::<Vec<_>>(),
    });
    let sign_words = has_word(claim_text, SIGN_WORDS) || has_word(exact, SIGN_WORDS);
    let met = || subject::check(&subject.words, &subject.periods, &source, true);
    let qualified = !qualifier::spans(claim_text).is_empty() || !qualifier::spans(exact).is_empty();
    let number = |equal, code| {
        let compared = if equal {
            Compared::Equal(Basis::Exact)
        } else {
            Compared::Differs(code, qualified)
        };
        settle(compared, met(), CheckMethod::Number, at, detail.clone())
    };
    match decide(claim, &sources, sign_words) {
        Decision::Equal(s) => {
            let mut out = vec![number(true, "checked")];
            let pairing = s.index.and_then(|i| {
                Some((
                    comparators(claim_text, claim_index, ctx)?,
                    comparators(exact, i, ctx)?,
                ))
            });
            if let Some((mine, theirs)) = pairing
                && mine != theirs
            {
                let names = |m: &[Meaning]| m.iter().map(|m| m.wire()).collect::<Vec<_>>();
                out.push(outcome(
                    CheckMethod::Qualifier,
                    CheckVerdict::Inconclusive,
                    Basis::None,
                    "qualifier_mismatch",
                    at,
                    json!({"claim": names(&mine), "source": names(&theirs)}),
                ));
            }
            out
        }
        Decision::Contradicts(code) => vec![number(false, code)],
        Decision::Open(code) => vec![outcome(
            CheckMethod::Number,
            CheckVerdict::Inconclusive,
            Basis::None,
            code,
            at,
            detail,
        )],
        Decision::Absent => vec![outcome(
            CheckMethod::Number,
            CheckVerdict::Unsupported,
            Basis::None,
            "number_not_in_source",
            at,
            detail,
        )],
    }
}
