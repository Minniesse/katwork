//! Task 11: the number, date, cell and arithmetic checks with subject words. Pure functions; no
//! database. Every check runs through `check_claim`, with `evidence::scan_ctx()` so two-digit
//! years never depend on today's date.
use katwork::docs::numfmt::date_to_serial;
use katwork::docs::{CellFacts, CellKind};
use katwork::evidence::cite::widen_to_tokens;
use katwork::evidence::claim::{ClaimKind, Derive, DeriveOp};
use katwork::evidence::codes::CODES;
use katwork::evidence::scan_ctx;
use katwork::evidence::verify::subject::{self, OP_WORDS, STOP_WORDS};
use katwork::evidence::verify::{
    Basis, CheckMethod, CheckOutcome, CheckVerdict, ClaimInput, ClaimStatus, RefQuality,
    ResolvedRef, boundary, check_claim, roll_up,
};
use katwork_thai::MAX_SCAN_CODE_POINTS;

use CheckVerdict::{Contradicted, Inconclusive, Supported, Unsupported};

// ---------------------------------------------------------------- helpers

/// A full-quality ref whose quote is the first occurrence of `exact` in `block`.
fn in_block(exact: &str, block: &str) -> ResolvedRef {
    let chars: Vec<char> = block.chars().collect();
    let needle: Vec<char> = exact.chars().collect();
    let start = (0..=chars.len().saturating_sub(needle.len()))
        .find(|&i| chars[i..].starts_with(&needle))
        .unwrap_or_else(|| panic!("{exact:?} not in {block:?}"));
    ResolvedRef::Ok {
        exact: exact.into(),
        block_text: block.into(),
        quote_start: start as u32,
        quote_end: (start + needle.len()) as u32,
        unit_context: None,
        cell: None,
        sheet: None,
        quality: RefQuality::Full,
    }
}

/// A full-quality ref that quotes its whole block.
fn q(text: &str) -> ResolvedRef {
    in_block(text, text)
}

fn facts(kind: CellKind, cached: Option<&str>) -> CellFacts {
    CellFacts {
        kind,
        cached: cached.map(Into::into),
        displayed: cached.map(Into::into),
        formula: None,
        number_format: None,
        date1904: false,
        hidden: false,
        row_header: None,
        col_header: None,
        header_unit: None,
        cache_untrusted: false,
    }
}

/// A full-quality cell ref quoting `quote`, its whole block.
fn cell_quoting(quote: &str, c: CellFacts) -> ResolvedRef {
    let mut r = q(quote);
    if let ResolvedRef::Ok { cell, .. } = &mut r {
        *cell = Some(c);
    }
    r
}

/// A full-quality cell ref quoting its cached value.
fn cell(c: CellFacts) -> ResolvedRef {
    let text = c.cached.clone().unwrap_or_default();
    cell_quoting(&text, c)
}

fn number_cell(cached: &str) -> CellFacts {
    facts(CellKind::Number, Some(cached))
}

fn row(mut c: CellFacts, header: &str) -> CellFacts {
    c.row_header = Some(header.into());
    c
}

fn col(mut c: CellFacts, header: &str) -> CellFacts {
    c.col_header = Some(header.into());
    c
}

fn fmt(mut c: CellFacts, format: &str) -> CellFacts {
    c.number_format = Some(format.into());
    c
}

fn with_unit_context(mut r: ResolvedRef, text: &str) -> ResolvedRef {
    if let ResolvedRef::Ok { unit_context, .. } = &mut r {
        *unit_context = Some(text.into());
    }
    r
}

fn with_sheet(mut r: ResolvedRef, name: &str) -> ResolvedRef {
    if let ResolvedRef::Ok { sheet, .. } = &mut r {
        *sheet = Some(name.into());
    }
    r
}

fn claim(kind: ClaimKind, text: &str, refs: Vec<ResolvedRef>) -> ClaimInput {
    ClaimInput {
        kind,
        text: text.into(),
        cited: !refs.is_empty(),
        derive: None,
        refs,
    }
}

fn number(text: &str, r: ResolvedRef) -> ClaimInput {
    claim(ClaimKind::Number, text, vec![r])
}

fn date(text: &str, r: ResolvedRef) -> ClaimInput {
    claim(ClaimKind::Date, text, vec![r])
}

fn derived(text: &str, op: DeriveOp, refs: Vec<ResolvedRef>) -> ClaimInput {
    let args = (0..refs.len() as u8).collect();
    ClaimInput {
        derive: Some(Derive { op, args }),
        ..claim(ClaimKind::Number, text, refs)
    }
}

fn checks(c: &ClaimInput) -> Vec<CheckOutcome> {
    check_claim(c, &scan_ctx())
}

fn status(c: &ClaimInput) -> (ClaimStatus, &'static str) {
    roll_up(c.cited, &checks(c))
}

const FIGURE_METHODS: [CheckMethod; 4] = [
    CheckMethod::Number,
    CheckMethod::Date,
    CheckMethod::Cell,
    CheckMethod::Arithmetic,
];

/// The first number, date, cell or arithmetic check of a claim.
fn figure(c: &ClaimInput) -> (CheckVerdict, &'static str) {
    let all = checks(c);
    let found = all
        .iter()
        .find(|o| FIGURE_METHODS.contains(&o.method))
        .unwrap_or_else(|| panic!("no figure check for {:?}: {all:?}", c.text));
    (found.verdict, found.code)
}

fn of_method(c: &ClaimInput, method: CheckMethod) -> Vec<CheckOutcome> {
    checks(c)
        .into_iter()
        .filter(|o| o.method == method)
        .collect()
}

fn assert_figure(c: &ClaimInput, want: (CheckVerdict, &str)) {
    assert_eq!(figure(c), want, "{:?} against {:?}", c.text, c.refs);
    assert_known_codes(c);
}

fn assert_known_codes(c: &ClaimInput) {
    for o in checks(c) {
        assert!(CODES.contains(&o.code), "{} not in CODES", o.code);
    }
}

const OK: (CheckVerdict, &str) = (Supported, "checked");

// ---------------------------------------------------------------- numbers

#[test]
fn number_checks_table() {
    let rows: Vec<(&str, ResolvedRef, (CheckVerdict, &str))> = vec![
        // The plan's rows.
        ("รายได้รวม 1,250 ล้านบาท", q("รายได้รวม ๑,๒๕๐ ล้านบาท"), OK),
        ("รายได้รวม 1.25 พันล้านบาท", q("รายได้รวม 1,250 ล้านบาท"), OK),
        (
            "รายได้รวม 1,300 ล้าน",
            q("รายได้รวม 1,254 ล้าน"),
            (Contradicted, "number_mismatch"),
        ),
        (
            "รายได้รวม 1,250 ล้านบาท",
            with_unit_context(cell(row(number_cell("1250"), "รายได้รวม")), "หน่วย: ล้านบาท"),
            OK,
        ),
        (
            "รายได้รวม 1,250 ล้านบาท",
            q("รายได้รวม 1,250"),
            (Inconclusive, "scale_unstated"),
        ),
        ("ขาดทุน -1,250", q("ขาดทุน (1,250)"), OK),
        (
            "growth 12.5%",
            cell_quoting("12.5%", col(fmt(number_cell("0.125"), "0.0%"), "growth")),
            OK,
        ),
        (
            "revenue 1,250 million",
            cell_quoting(
                "1,250",
                row(fmt(number_cell("1250000000"), "#,##0,,"), "revenue"),
            ),
            OK,
        ),
        (
            "growth 12.5%",
            q("growth 12.5 baht"),
            (Contradicted, "unit_mismatch"),
        ),
        (
            "total 3,450",
            q("total 3,451 and 200"),
            (Unsupported, "number_not_in_source"),
        ),
        (
            "งบประมาณ 3.48 ล้านล้านบาท",
            q("งบประมาณ 3,480,000 ล้านบาท"),
            OK,
        ),
        (
            "ค่าใช้จ่าย 1,200 บาท (หนึ่งพันสองร้อยบาทถ้วน)",
            q("ค่าใช้จ่าย 1,200 บาท"),
            OK,
        ),
        // Scale, unit and precision.
        (
            "revenue 1,250 baht",
            q("revenue 1,250 ล้านบาท"),
            (Inconclusive, "scale_unstated"),
        ),
        (
            "revenue 1,250 USD",
            q("revenue 1,250 THB"),
            (Contradicted, "unit_mismatch"),
        ),
        (
            "growth 12.5",
            q("growth 12.5%"),
            (Inconclusive, "scale_unstated"),
        ),
        ("revenue 1.2 mn", q("revenue 1.2 million"), OK),
        ("revenue 1,250.00 บาท", q("revenue 1,250 บาท"), OK),
        (
            "รายได้รวม 1,254 ล้านบาท",
            q("รายได้รวม 1.25 พันล้านบาท"),
            (Inconclusive, "precision_differs"),
        ),
        ("รายได้รวม 1,250 ล้านบาท", q("รายได้รวม 1.25 พันล้านบาท"), OK),
        // A value that differs only by its unstated scale is not contradicted.
        (
            "รายได้รวม 1,250 ล้านบาท",
            q("รายได้รวม 1.25"),
            (Inconclusive, "scale_unstated"),
        ),
        (
            "revenue 1,251",
            cell(row(number_cell("1250.5"), "revenue")),
            OK,
        ),
        (
            "revenue 1,250",
            cell(row(number_cell("1250.5"), "revenue")),
            (Contradicted, "number_mismatch"),
        ),
        (
            "revenue 1,250",
            cell_quoting(
                "1,250",
                row(fmt(number_cell("1250000000"), "#,##0,,"), "revenue"),
            ),
            (Inconclusive, "scale_unstated"),
        ),
        (
            "value 0.3",
            cell(row(number_cell("0.30000000000000004"), "value")),
            OK,
        ),
        (
            "value 0.0000125",
            cell(row(number_cell("1.25E-05"), "value")),
            OK,
        ),
        // Several source figures: one equal is enough; none equal is not in the source.
        ("total 3,450", q("total 3,450 and 200"), OK),
        ("revenue 1,250 ล้านบาท", q("revenue 1,250\nล้านบาท"), OK),
        ("revenue 1,250 บาท", q("revenue １，２５０ บาท"), OK),
        (
            "revenue 1,250 บาท",
            q("revenue 1,\n250 บาท"),
            (Unsupported, "number_not_in_source"),
        ),
        (
            "revenue 1,250",
            q("revenue none here"),
            (Unsupported, "number_not_in_source"),
        ),
        // A different subject or a comparator never lets a mismatch fail the claim.
        (
            "Entity B revenue 1,300 baht",
            q("Entity A revenue 1,250 baht"),
            (Inconclusive, "subject_not_in_source"),
        ),
        (
            "budget at most 1,300",
            q("budget at most 1,250"),
            (Inconclusive, "number_mismatch"),
        ),
        // A bare figure has no subject to check.
        (
            "1250",
            q("revenue 1250"),
            (Inconclusive, "subject_not_in_source"),
        ),
        // Cells that are never numbers, and cell facts.
        (
            "flag 1",
            cell(row(facts(CellKind::Bool, Some("TRUE")), "flag")),
            (Unsupported, "number_not_in_source"),
        ),
        (
            "flag 0",
            cell(row(facts(CellKind::Error, Some("#DIV/0!")), "flag")),
            (Unsupported, "number_not_in_source"),
        ),
        (
            "growth 12.5 baht",
            cell_quoting("12.5%", col(fmt(number_cell("0.125"), "0.0%"), "growth")),
            (Contradicted, "unit_mismatch"),
        ),
        (
            "รายได้ 1,250 ล้านบาท",
            with_unit_context(
                cell({
                    let mut c = col(number_cell("1250"), "รายได้ (ล้านบาท)");
                    c.header_unit = Some("ล้านบาท".into());
                    c
                }),
                "หน่วย: บาท",
            ),
            OK,
        ),
        (
            "รายได้ 1,250 ล้านบาท",
            with_unit_context(q("รายได้ 1,250"), "หน่วย: ล้านบาท"),
            OK,
        ),
        // A source amount in words that disagrees with its own figure.
        (
            "ค่าใช้จ่าย 1,200 บาท",
            q("ค่าใช้จ่าย 1,200 บาท (หนึ่งพันสามร้อยบาทถ้วน)"),
            (Inconclusive, "amount_words_mismatch"),
        ),
        // A header's scale word is never read into the quote's figure.
        (
            "revenue 1,250 ล้านบาท",
            cell_quoting(
                "revenue 1250",
                col(facts(CellKind::Text, Some("revenue 1250")), "ล้านบาท"),
            ),
            (Inconclusive, "scale_unstated"),
        ),
        // Both sides state a scale: a different value is a mismatch, not an unstated scale.
        (
            "รายได้รวม 1.25 พันล้านบาท",
            q("รายได้รวม 125 ล้านบาท"),
            (Contradicted, "number_mismatch"),
        ),
        (
            "รายได้รวม 1,250 ล้านบาท",
            q("รายได้รวม 1,250 พันล้านบาท"),
            (Contradicted, "number_mismatch"),
        ),
        // A date cell is not a number.
        (
            "ยอด 45931",
            cell(row(fmt(number_cell("45931"), "d/m/yyyy"), "ยอด")),
            (Unsupported, "number_not_in_source"),
        ),
        ("ยอด 45931", cell(row(number_cell("45931"), "ยอด")), OK),
    ];
    assert!(rows.len() >= 40, "{} rows", rows.len());
    for (text, r, want) in rows {
        assert_figure(&number(text, r), want);
    }
}

#[test]
fn amount_in_words_is_checked_against_its_figure() {
    let ok = number(
        "ค่าใช้จ่าย 1,200 บาท (หนึ่งพันสองร้อยบาทถ้วน)",
        q("ค่าใช้จ่าย 1,200 บาท"),
    );
    let words = of_method(&ok, CheckMethod::AmountWords);
    assert_eq!(words.len(), 1, "{words:?}");
    assert_eq!((words[0].verdict, words[0].basis), (Supported, Basis::None));
    assert_eq!(figure(&ok), OK);
    assert_eq!(status(&ok), (ClaimStatus::Passed, "checked"));
    let bad = number(
        "ค่าใช้จ่าย 1,200 บาท (หนึ่งพันสามร้อยบาทถ้วน)",
        q("ค่าใช้จ่าย 1,200 บาท"),
    );
    let words = of_method(&bad, CheckMethod::AmountWords);
    assert_eq!(
        (words[0].verdict, words[0].code),
        (Contradicted, "amount_words_mismatch")
    );
    assert_eq!(status(&bad), (ClaimStatus::Failed, "amount_words_mismatch"));
}

#[test]
fn amount_words_alone_never_passes() {
    let alone = CheckOutcome {
        method: CheckMethod::AmountWords,
        verdict: Supported,
        basis: Basis::None,
        code: "checked",
        ref_ordinal: None,
        advisory: false,
        detail: serde_json::json!({}),
    };
    assert_eq!(roll_up(true, &[alone]).0, ClaimStatus::Unverified);
    // The amount-in-words check on a claim whose every ref is hidden: nothing to fail it.
    let c = number(
        "ค่าใช้จ่าย 1,200 บาท (หนึ่งพันสามร้อยบาทถ้วน)",
        {
            let mut r = q("ค่าใช้จ่าย 1,200 บาท");
            if let ResolvedRef::Ok { quality, .. } = &mut r {
                *quality = RefQuality::Hidden;
            }
            r
        },
    );
    assert!(of_method(&c, CheckMethod::AmountWords).is_empty());
    assert_eq!(status(&c), (ClaimStatus::Unverified, "hidden_source"));
}

#[test]
fn same_value_wrong_entity_is_not_passed() {
    for (text, source) in [
        ("Entity B revenue 1,250 baht", "Entity A revenue 1,250 baht"),
        ("ฝ่าย ข งบประมาณ 1,250 ล้านบาท", "ฝ่าย ก งบประมาณ 1,250 ล้านบาท"),
    ] {
        let c = number(text, q(source));
        assert_figure(&c, (Inconclusive, "subject_not_in_source"));
        assert_eq!(status(&c).0, ClaimStatus::Unverified);
    }
}

#[test]
fn same_value_wrong_period_is_contradicted() {
    let c = number("รายได้ปี 2567 1,250 ล้านบาท", q("รายได้ปี 2566 1,250 ล้านบาท"));
    assert_figure(&c, (Contradicted, "period_mismatch"));
    assert_eq!(status(&c), (ClaimStatus::Failed, "period_mismatch"));
}

#[test]
fn era_and_fiscal_restatements_pass() {
    let rows = [
        ("รายได้ปี 2567 1,250 ล้านบาท", "รายได้ปี 2024 1,250 ล้านบาท", OK),
        (
            "งบประมาณ FY2026 3,450 ล้านบาท",
            "งบประมาณ ปีงบประมาณ 2569 3,450 ล้านบาท",
            OK,
        ),
        (
            "รายได้ปีงบประมาณ 2568 1,250",
            "รายได้ปี 2568 1,250",
            (Inconclusive, "period_ambiguous"),
        ),
        (
            "รายได้ปีงบประมาณ 2568 1,250",
            "รายได้ปีปฏิทิน 2568 1,250",
            (Inconclusive, "period_kind_differs"),
        ),
        (
            "Q2 revenue 1,250",
            "Q3 revenue 1,250",
            (Inconclusive, "subject_not_in_source"),
        ),
        (
            "Q2 FY2026 revenue 1,250",
            "Q3 FY2026 revenue 1,250",
            (Contradicted, "period_mismatch"),
        ),
        // A quarter and a whole year are not one period: never a mismatch.
        (
            "Q2 FY2026 revenue 1,250",
            "FY2026 revenue 1,250",
            (Inconclusive, "subject_not_in_source"),
        ),
        (
            "รายได้ ค.ศ. 2567 1,250",
            "รายได้ พ.ศ. 2567 1,250",
            (Contradicted, "period_mismatch"),
        ),
        (
            "รายได้ปีงบ 69 1,250",
            "รายได้ปีงบประมาณ 2569 1,250",
            (Inconclusive, "date_ambiguous"),
        ),
    ];
    for (text, source, want) in rows {
        assert_figure(&number(text, q(source)), want);
    }
}

#[test]
fn any_matching_source_period_satisfies() {
    // The quote's year matches; another year sits in the sheet name.
    let r = with_sheet(q("รายได้ปี 2567 1,250 ล้านบาท"), "ปี 2566");
    assert_figure(&number("รายได้ปี 2567 1,250 ล้านบาท", r), OK);
    // A year found only in the sheet name may agree but never contradicts.
    let r = with_sheet(q("รายได้ 1,250 ล้านบาท"), "ปี 2567");
    assert_figure(&number("รายได้ปี 2567 1,250 ล้านบาท", r), OK);
    let r = with_sheet(q("รายได้ 1,250 ล้านบาท"), "ปี 2566");
    assert_figure(
        &number("รายได้ปี 2567 1,250 ล้านบาท", r),
        (Inconclusive, "period_ambiguous"),
    );
    let r = with_unit_context(q("รายได้ 1,250 ล้านบาท"), "ปี 2566");
    assert_figure(
        &number("รายได้ปี 2567 1,250 ล้านบาท", r),
        (Inconclusive, "period_ambiguous"),
    );
}

#[test]
fn instruction_like_sheet_name_is_left_out() {
    let r = with_sheet(q("รายได้ 1,250 ล้านบาท"), "ปี 2567\u{E0041}");
    assert_figure(
        &number("รายได้ปี 2567 1,250 ล้านบาท", r.clone()),
        (Inconclusive, "subject_not_in_source"),
    );
    assert_eq!(subject::source_text(&r), "รายได้ 1,250 ล้านบาท");
}

#[test]
fn dropped_comparator_on_a_number_is_not_passed() {
    for (text, source) in [
        ("งบประมาณ 1,250 ล้านบาท", "งบประมาณไม่เกิน 1,250 ล้านบาท"),
        ("budget 1,250", "budget at least 1,250"),
    ] {
        assert_eq!(
            status(&number(text, q(source))),
            (ClaimStatus::Unverified, "qualifier_mismatch")
        );
    }
    let c = number(
        "งบประมาณไม่เกิน 1,250 ล้านบาท",
        q("งบประมาณไม่เกิน 1,250 ล้านบาท"),
    );
    assert_eq!(status(&c), (ClaimStatus::Passed, "checked"));
}

#[test]
fn comparator_belongs_to_a_different_figure() {
    // The counts agree (one "more than" each), but the source's belongs to the 3.
    let c = number("more than 5 staff", q("5 staff and more than 3 managers"));
    assert_eq!(status(&c), (ClaimStatus::Unverified, "qualifier_mismatch"));
    assert!(
        of_method(&c, CheckMethod::Qualifier)
            .iter()
            .any(|o| o.code == "qualifier_mismatch")
    );
}

#[test]
fn postfix_comparator_pairs_with_the_previous_figure() {
    // The scan pairs "or more" with the next figure, so a postfix comparator is held open.
    let c = number("5 or more staff", q("5 or more staff, 3 managers"));
    assert_eq!(status(&c), (ClaimStatus::Unverified, "qualifier_mismatch"));
}

#[test]
fn changed_subject_word_is_not_passed() {
    assert_figure(
        &number("net revenue 1,250", q("gross revenue 1,250")),
        (Inconclusive, "subject_not_in_source"),
    );
}

#[test]
fn latin_word_is_whole_word() {
    assert_figure(
        &number(
            "Entity b revenue 1,250 baht",
            q("Entity revenue 1,250 baht"),
        ),
        (Inconclusive, "subject_not_in_source"),
    );
}

/// Known gap: Thai has no word boundaries here, so a Thai subject word found inside a longer
/// source word passes (`รายได้` inside `รายได้สุทธิ`). Pinned so a fix shows up.
#[test]
fn known_gaps_thai_substring_subject() {
    assert_figure(&number("รายได้ 1,250", q("รายได้สุทธิ 1,250")), OK);
}

#[test]
fn quote_that_cuts_a_number_or_date_is_not_passed() {
    let rows = [
        (
            ClaimKind::Number,
            "งบประมาณ 12",
            "งบประมาณ 12",
            "งบประมาณ 12,500 บาท",
        ),
        (
            ClaimKind::Number,
            "total revenue 1,250 baht",
            "1,250 baht total revenue",
            "11,250 baht total revenue",
        ),
        (
            ClaimKind::Date,
            "ประชุม 1 ต.ค. 2568",
            "1 ต.ค. 2568 ประชุม",
            "11 ต.ค. 2568 ประชุม",
        ),
        (
            ClaimKind::Number,
            "วาระที่ 3 อนุมัติงบ 3,450",
            "วาระที่ 3",
            "วาระที่ 31 อนุมัติงบ 3,450",
        ),
        // Thai digits, a scale word and a percent sign cut.
        (ClaimKind::Number, "งบ 1,250 บาท", "งบ ๑,๒", "งบ ๑,๒๕๐ บาท"),
        (
            ClaimKind::Number,
            "งบ 1,250 ล",
            "งบ 1,250 ล",
            "งบ 1,250 ล้าน",
        ),
        (
            ClaimKind::Number,
            "growth 12.5",
            "growth 12.5",
            "growth 12.5%",
        ),
    ];
    for (kind, text, exact, block) in rows {
        let r = in_block(exact, block);
        assert!(boundary::cuts(&r, &scan_ctx()), "{exact:?} in {block:?}");
        let c = claim(kind, text, vec![r]);
        let want = match kind {
            ClaimKind::Date => CheckMethod::Date,
            _ => CheckMethod::Number,
        };
        let found = checks(&c);
        let cut: Vec<_> = found
            .iter()
            .filter(|o| FIGURE_METHODS.contains(&o.method))
            .collect();
        assert_eq!(cut.len(), 1, "{found:?}");
        assert_eq!(
            (cut[0].method, cut[0].verdict, cut[0].code),
            (want, Inconclusive, "quote_cuts_number")
        );
        assert_eq!(status(&c).0, ClaimStatus::Unverified, "{text}");
    }
}

#[test]
fn cuts_agrees_with_cite_widening() {
    let ctx = scan_ctx();
    let blocks = [
        "งบประมาณ 12,500 บาท และวาระที่ 31",
        "11 ต.ค. 2568 ประชุม 1,250 ล้าน",
        "growth 12.5% in ๑,๒๕๐ units",
    ];
    for block in blocks {
        let len = block.chars().count() as u32;
        for start in 0..len {
            for end in start + 1..=len {
                let r = in_block(
                    &block
                        .chars()
                        .skip(start as usize)
                        .take((end - start) as usize)
                        .collect::<String>(),
                    block,
                );
                let r = match r {
                    ResolvedRef::Ok {
                        exact,
                        block_text,
                        unit_context,
                        cell,
                        sheet,
                        quality,
                        ..
                    } => ResolvedRef::Ok {
                        exact,
                        block_text,
                        quote_start: start,
                        quote_end: end,
                        unit_context,
                        cell,
                        sheet,
                        quality,
                    },
                    stale => stale,
                };
                let widened = widen_to_tokens(block, start, end, &ctx).unwrap();
                assert_eq!(
                    boundary::cuts(&r, &ctx),
                    widened != (start, end),
                    "{block:?} {start}..{end}"
                );
            }
        }
    }
}

#[test]
fn quote_at_a_figure_edge_does_not_cut_and_long_block_counts_as_cut() {
    let r = in_block("งบประมาณ", "งบประมาณ 12,500 บาท");
    assert!(!boundary::cuts(&r, &scan_ctx()));
    let long = "ก".repeat(MAX_SCAN_CODE_POINTS + 1);
    let r = in_block("ก", &long);
    assert!(boundary::cuts(&r, &scan_ctx()));
}

#[test]
fn text_cells_are_never_compared_as_numbers() {
    let text = cell_quoting("รายได้", row(facts(CellKind::Text, Some("9,999")), "รายได้"));
    assert_figure(
        &number("รายได้ 9,999", text),
        (Unsupported, "number_not_in_source"),
    );
    let num = cell(row(number_cell("9999"), "รายได้"));
    assert_figure(&number("รายได้ 9,999", num), OK);
}

#[test]
fn bool_and_error_cells_are_never_numbers() {
    for c in [
        facts(CellKind::Bool, Some("TRUE")),
        facts(CellKind::Error, Some("#N/A")),
    ] {
        let r = cell(row(c, "flag"));
        assert_eq!(
            figure(&number("flag 1", r)),
            (Unsupported, "number_not_in_source")
        );
    }
}

#[test]
fn hidden_cell_on_a_full_ref_is_not_compared() {
    let mut c = row(number_cell("9999"), "รายได้");
    c.hidden = true;
    let found = number("รายได้ 9,999", cell(c));
    assert_figure(&found, (Inconclusive, "hidden_source"));
    assert!(of_method(&found, CheckMethod::Number).is_empty());
}

#[test]
fn text_too_long_to_scan_is_inconclusive() {
    let long = "ก".repeat(MAX_SCAN_CODE_POINTS + 1);
    let c = number(&long, q("รายได้ 1,250"));
    let found = checks(&c);
    assert!(
        found
            .iter()
            .any(|o| o.verdict == Inconclusive && o.code == "text_too_long"),
        "{found:?}"
    );
    assert_eq!(status(&c).0, ClaimStatus::Unverified);
}

#[test]
fn header_unit_scales_a_cell() {
    let header = |unit: &str| {
        let mut c = col(number_cell("1250"), "รายได้ (ล้านบาท)");
        c.header_unit = Some(unit.into());
        cell(c)
    };
    assert_figure(&number("รายได้ 1,250 ล้านบาท", header("ล้านบาท")), OK);
    assert_figure(
        &number("รายได้ 1,250 ล้านบาท", header("ล้านบาท\u{E0041}")),
        (Inconclusive, "scale_unstated"),
    );
}

#[test]
fn bare_figure_quote_is_not_passed() {
    assert_figure(
        &number("รายได้รวม 1,250 ล้านบาท", q("1,250 ล้านบาท")),
        (Inconclusive, "subject_not_in_source"),
    );
}

#[test]
fn year_in_claim_is_not_a_quantity() {
    let c = number("รายได้ปี 2567 1,250 ล้านบาท", q("รายได้ปี 2567 1,250 ล้านบาท"));
    assert_figure(&c, OK);
    assert!(
        checks(&c)
            .iter()
            .all(|o| o.code != "claim_number_ambiguous")
    );
}

#[test]
fn ordinal_beside_a_figure_is_not_ambiguous() {
    let text = "งบ 1,250 ล้านบาท วาระที่ 3";
    assert_figure(&number(text, q(text)), OK);
}

#[test]
fn number_inside_a_fact_claim_is_still_number_checked() {
    let c = claim(
        ClaimKind::Fact,
        "รายได้รวม 1,300 ล้าน",
        vec![q("รายได้รวม 1,254 ล้าน")],
    );
    assert_figure(&c, (Contradicted, "number_mismatch"));
    assert_eq!(status(&c), (ClaimStatus::Failed, "number_mismatch"));
}

#[test]
fn several_claim_figures_are_ambiguous() {
    for text in ["ratio 3:1", "Thailand 2024 revenue 1,250", "1 250 baht"] {
        let c = number(text, q(text));
        assert_figure(&c, (Inconclusive, "claim_number_ambiguous"));
        assert_eq!(
            status(&c),
            (ClaimStatus::Unverified, "claim_number_ambiguous")
        );
    }
}

#[test]
fn single_letter_scale_is_not_a_quantity() {
    let c = number("revenue 1.2 M", q("revenue 1.2 million"));
    assert!(
        checks(&c)
            .iter()
            .all(|o| !FIGURE_METHODS.contains(&o.method))
    );
    assert_eq!(status(&c), (ClaimStatus::Unverified, "needs_review"));
}

#[test]
fn number_claim_without_a_figure_needs_review() {
    let c = number("รายได้รวม", q("รายได้รวม 1,250 ล้านบาท"));
    assert!(
        checks(&c)
            .iter()
            .all(|o| !FIGURE_METHODS.contains(&o.method))
    );
    assert_eq!(status(&c), (ClaimStatus::Unverified, "needs_review"));
}

#[test]
fn uncited_claim_gets_no_checks() {
    let mut c = number("9999", q("9999"));
    c.cited = false;
    assert!(checks(&c).is_empty());
    c.refs.clear();
    assert!(checks(&c).is_empty());
    assert_eq!(status(&c), (ClaimStatus::Unverified, "uncited"));
}

#[test]
fn only_resolved_refs_are_compared() {
    let mut r = q("รายได้รวม 1,254 ล้าน");
    if let ResolvedRef::Ok { quality, .. } = &mut r {
        *quality = RefQuality::Generated;
    }
    let c = number("รายได้รวม 1,300 ล้าน", r);
    assert!(
        checks(&c)
            .iter()
            .all(|o| !FIGURE_METHODS.contains(&o.method))
    );
    assert_eq!(status(&c), (ClaimStatus::Unverified, "source_generated"));
}

// ---------------------------------------------------------------- signs and ratios

#[test]
fn sign_trap_never_passes() {
    for (text, source, want) in [
        (
            "profit 5",
            "profit (5)",
            (ClaimStatus::Unverified, "number_ambiguous"),
        ),
        (
            "profit 5",
            "profit -5",
            (ClaimStatus::Failed, "number_mismatch"),
        ),
        (
            "profit 5",
            "profit negative 5",
            (ClaimStatus::Unverified, "number_ambiguous"),
        ),
        (
            "profit negative 5",
            "profit 5",
            (ClaimStatus::Unverified, "number_ambiguous"),
        ),
        (
            "กำไร 1,250",
            "กำไร (1,250)",
            (ClaimStatus::Failed, "number_mismatch"),
        ),
    ] {
        let c = number(text, q(source));
        assert_eq!(status(&c), want, "{text} / {source}");
    }
    assert_figure(
        &number("profit 5", q("profit negative 5")),
        (Inconclusive, "number_ambiguous"),
    );
    assert_figure(
        &number("กำไร 1,250", q("กำไร (1,250)")),
        (Contradicted, "number_mismatch"),
    );
    // A true loss claim against an accounting negative is never failed.
    let c = number("ขาดทุน 1,250", q("ขาดทุน (1,250)"));
    assert_figure(&c, (Inconclusive, "sign_differs"));
    assert_eq!(status(&c), (ClaimStatus::Unverified, "sign_differs"));
}

#[test]
fn ratio_trap_never_passes() {
    let c = number("ratio 3:1", q("ratio 1:3"));
    assert_eq!(
        status(&c),
        (ClaimStatus::Unverified, "claim_number_ambiguous")
    );
    // A fraction reads as two codes: no figure to check.
    let c = number("share 1/3", q("share 3/1"));
    assert_eq!(status(&c), (ClaimStatus::Unverified, "needs_review"));
}

// ---------------------------------------------------------------- dates

#[test]
fn date_checks_table() {
    let rows = [
        ("ประชุมวันที่ 1 ต.ค. 2568", q("ประชุมวันที่ 1 October 2025"), OK),
        (
            "ประชุมวันที่ 1 ต.ค. 68",
            q("ประชุมวันที่ 1 ต.ค. 2568"),
            (Inconclusive, "date_ambiguous"),
        ),
        ("งบ ปีงบประมาณ 2569", q("งบ FY2026"), OK),
        (
            "ประชุมวันที่ 2 ต.ค. 2568",
            q("ประชุมวันที่ 1 ต.ค. 2568"),
            (Contradicted, "date_mismatch"),
        ),
        (
            "ประชุม 1 ต.ค. 2568",
            in_block("1 ต.ค. 2568 ประชุม", "11 ต.ค. 2568 ประชุม"),
            (Inconclusive, "quote_cuts_number"),
        ),
        (
            "ประชุม ปี 2567-2568",
            q("ประชุม ปี 2567"),
            (Inconclusive, "date_ambiguous"),
        ),
        (
            "ประชุมวันที่ 1 ต.ค. 2568",
            q("ประชุมวันที่ เดือนหน้า"),
            (Unsupported, "date_not_in_source"),
        ),
        (
            "งบ ปีงบประมาณ 2568",
            q("งบ ปีปฏิทิน 2568"),
            (Inconclusive, "period_kind_differs"),
        ),
        (
            "งบ ปีงบประมาณ 2568",
            q("งบ ปี 2568"),
            (Inconclusive, "period_ambiguous"),
        ),
        // A month and a day do not compare.
        (
            "ประชุม ต.ค. 2568",
            q("ประชุม 1 ต.ค. 2568"),
            (Unsupported, "date_not_in_source"),
        ),
        ("ประชุม 1 ต.ค. 2568", q("ประชุมใหญ่ 1 ต.ค. 2568"), OK),
        (
            "ประชุมใหญ่ 1 ต.ค. 2568",
            q("ประชุม 1 ต.ค. 2568"),
            (Inconclusive, "subject_not_in_source"),
        ),
        (
            "1 ต.ค. 2568",
            q("ประชุม 1 ต.ค. 2568"),
            (Inconclusive, "subject_not_in_source"),
        ),
    ];
    for (text, r, want) in rows {
        assert_figure(&date(text, r), want);
    }
    let c = number("รายได้ ค.ศ. 2567 1,250", q("รายได้ พ.ศ. 2567 1,250"));
    assert_figure(&c, (Contradicted, "period_mismatch"));
}

#[test]
fn comparator_holds_a_date_mismatch_open() {
    let c = date("ประชุมไม่เกินวันที่ 2 ต.ค. 2568", q("ประชุมไม่เกินวันที่ 1 ต.ค. 2568"));
    assert_figure(&c, (Inconclusive, "date_mismatch"));
    assert_eq!(status(&c), (ClaimStatus::Unverified, "date_mismatch"));
}

#[test]
fn date_claim_finds_its_date_among_several() {
    let mut c = facts(CellKind::Text, Some("ประชุมวันที่ 1 ต.ค. 2568"));
    c.row_header = Some("5 ต.ค. 2568".into());
    let r = cell(c);
    assert_figure(&date("ประชุมวันที่ 1 ต.ค. 2568", r), OK);
}

fn serial(y: i32, m: u32, d: u32, date1904: bool) -> String {
    let day = chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap();
    let s = date_to_serial(day.and_hms_opt(0, 0, 0).unwrap(), date1904).unwrap();
    format!("{s}")
}

#[test]
fn date_cell_compares_as_a_date() {
    let date_cell = |cached: &str, date1904: bool| {
        let mut c = row(fmt(number_cell(cached), "d/m/yyyy"), "ประชุมวันที่");
        c.date1904 = date1904;
        cell(c)
    };
    let s = serial(2025, 10, 1, false);
    assert_figure(&date("ประชุมวันที่ 1 ต.ค. 2568", date_cell(&s, false)), OK);
    assert_figure(
        &date("ประชุมวันที่ 1 ต.ค. 2568", date_cell(&s, true)),
        (Contradicted, "date_mismatch"),
    );
    let s1904 = serial(2025, 10, 1, true);
    assert_figure(&date("ประชุมวันที่ 1 ต.ค. 2568", date_cell(&s1904, true)), OK);
    // Excel's 29 February 1900 is no day: never a pass, never a failure.
    let (verdict, _) = figure(&date("ประชุมวันที่ 1 ต.ค. 2568", date_cell("60", false)));
    assert!(matches!(verdict, Unsupported | Inconclusive), "{verdict:?}");
}

// ---------------------------------------------------------------- cells

#[test]
fn formula_without_cache_and_untrusted_cache_are_inconclusive() {
    let mut not_calculated = row(facts(CellKind::Number, None), "รายได้");
    not_calculated.formula = Some("SUM(B1:B3)".into());
    let r = cell_quoting("", not_calculated);
    let c = number("รายได้ 9,999", r);
    assert_figure(&c, (Inconclusive, "cell_not_calculated"));
    assert_eq!(status(&c).0, ClaimStatus::Unverified);

    let mut untrusted = row(number_cell("9999"), "รายได้");
    untrusted.formula = Some("SUM(B1:B3)".into());
    untrusted.cache_untrusted = true;
    let c = number("รายได้ 9,999", cell(untrusted));
    assert_figure(&c, (Inconclusive, "cell_cache_untrusted"));
    assert_eq!(status(&c).0, ClaimStatus::Unverified);

    // A value cell of a recalculated workbook is trusted.
    let c = number("รายได้ 9,999", cell(row(number_cell("9999"), "รายได้")));
    assert_eq!(status(&c), (ClaimStatus::Passed, "checked"));
}

// ---------------------------------------------------------------- arithmetic

#[test]
fn derived_sum_rolls_up_in_one_comparison() {
    let refs = || vec![q("travel 10"), q("food 20")];
    let c = derived("total 30", DeriveOp::Sum, refs());
    assert_eq!(status(&c), (ClaimStatus::Passed, "checked"));
    let arith = of_method(&c, CheckMethod::Arithmetic);
    assert_eq!(arith.len(), 1);
    assert_eq!(arith[0].basis, Basis::Derived);
    let c = derived("total 31", DeriveOp::Sum, refs());
    assert_eq!(status(&c), (ClaimStatus::Failed, "arithmetic_mismatch"));
    assert!(of_method(&c, CheckMethod::Number).is_empty());
    assert!(checks(&c).iter().all(|o| o.code != "number_mismatch"));
}

#[test]
fn percent_change_and_division_by_zero() {
    let c = derived(
        "growth 50%",
        DeriveOp::PercentChange,
        vec![q("growth base 10"), q("growth now 15")],
    );
    assert_figure(&c, (Supported, "checked"));
    let c = derived(
        "growth 50%",
        DeriveOp::PercentChange,
        vec![q("growth base 0"), q("growth now 15")],
    );
    assert_figure(&c, (Inconclusive, "division_by_zero"));
}

#[test]
fn derived_operations() {
    let pair = || vec![q("sales 15"), q("sales 10")];
    let rows = [
        ("sales difference 5", DeriveOp::Difference, OK),
        ("sales 1.5", DeriveOp::Ratio, OK),
        ("sales share 150%", DeriveOp::Share, OK),
        (
            "sales difference 6",
            DeriveOp::Difference,
            (Contradicted, "arithmetic_mismatch"),
        ),
    ];
    for (text, op, want) in rows {
        assert_figure(&derived(text, op, pair()), want);
    }
}

#[test]
fn direction_word_must_match_the_sign() {
    let ops = || vec![q("ยอดขาย 15"), q("ยอดขาย 10")];
    let up = derived("ยอดขาย เพิ่มขึ้น 33.3%", DeriveOp::PercentChange, ops());
    assert_figure(&up, (Contradicted, "arithmetic_mismatch"));
    let down = derived("ยอดขาย ลดลง 33.3%", DeriveOp::PercentChange, ops());
    assert_figure(&down, OK);
    let unsaid = derived("ยอดขาย 33.3%", DeriveOp::PercentChange, ops());
    assert_figure(&unsaid, (Inconclusive, "arithmetic_mismatch"));
}

#[test]
fn operands_must_agree_and_resolve() {
    let c = derived(
        "total 1,210",
        DeriveOp::Sum,
        vec![q("travel 1,200 บาท"), q("food 10 USD")],
    );
    assert_figure(&c, (Inconclusive, "operand_units_differ"));
    let mut hidden = q("food 20");
    if let ResolvedRef::Ok { quality, .. } = &mut hidden {
        *quality = RefQuality::Hidden;
    }
    let c = derived("total 30", DeriveOp::Sum, vec![q("travel 10"), hidden]);
    assert_figure(&c, (Inconclusive, "operand_missing"));
    assert_eq!(status(&c), (ClaimStatus::Unverified, "hidden_source"));
    let c = derived(
        "total 30",
        DeriveOp::Sum,
        vec![q("travel 10"), q("food none")],
    );
    assert_figure(&c, (Inconclusive, "operand_missing"));
    // An operand's subject must hold the claim's words.
    let c = derived(
        "Entity B total 30",
        DeriveOp::Sum,
        vec![q("Entity A travel 10"), q("Entity A food 20")],
    );
    assert_figure(&c, (Inconclusive, "subject_not_in_source"));
    let c = derived(
        "Entity B total 31",
        DeriveOp::Sum,
        vec![q("Entity A travel 10"), q("Entity A food 20")],
    );
    assert_eq!(
        status(&c),
        (ClaimStatus::Unverified, "subject_not_in_source")
    );
}

#[test]
fn derived_claim_unit_must_match_its_operands() {
    let baht = || vec![q("travel 10 บาท"), q("food 20 บาท")];
    let c = derived("total 30 USD", DeriveOp::Sum, baht());
    assert_figure(&c, (Inconclusive, "operand_units_differ"));
    assert_eq!(
        status(&c),
        (ClaimStatus::Unverified, "operand_units_differ")
    );
    assert_figure(&derived("total 30 บาท", DeriveOp::Sum, baht()), OK);
    let c = derived(
        "growth 50 บาท",
        DeriveOp::PercentChange,
        vec![q("growth base 10"), q("growth now 15")],
    );
    assert_figure(&c, (Inconclusive, "operand_units_differ"));
    let c = derived(
        "sales 1.5%",
        DeriveOp::Ratio,
        vec![q("sales 15"), q("sales 10")],
    );
    assert_figure(&c, (Inconclusive, "operand_units_differ"));
}

/// A derive that names one ref twice counts one figure twice: refused, never computed.
#[test]
fn repeated_operand_is_refused() {
    let repeated = |op, args: Vec<u8>, text: &str, source: &str| ClaimInput {
        derive: Some(Derive { op, args }),
        ..claim(ClaimKind::Number, text, vec![q(source)])
    };
    for c in [
        repeated(DeriveOp::Sum, vec![0; 2000], "travel 20000", "travel 10"),
        repeated(
            DeriveOp::Sum,
            vec![0, 0],
            "budget total 2,500",
            "budget 1,250",
        ),
        repeated(
            DeriveOp::Share,
            vec![0, 0],
            "ส่วนแบ่งตลาด 100%",
            "ส่วนแบ่งตลาด 1,250",
        ),
        repeated(
            DeriveOp::PercentChange,
            vec![0, 0],
            "sales change 0%",
            "sales 1,250",
        ),
    ] {
        assert_figure(&c, (Inconclusive, "operand_missing"));
        assert_eq!(status(&c), (ClaimStatus::Unverified, "operand_missing"));
    }
    assert!(
        !Derive {
            op: DeriveOp::Sum,
            args: vec![0, 0]
        }
        .indices_in(1)
    );
    assert!(
        !Derive {
            op: DeriveOp::Share,
            args: vec![1, 1]
        }
        .indices_in(2)
    );
    assert!(
        Derive {
            op: DeriveOp::Sum,
            args: vec![0, 1]
        }
        .indices_in(2)
    );
}

// ---------------------------------------------------------------- fix round 1

fn with_header_unit(mut c: CellFacts, unit: &str) -> CellFacts {
    c.header_unit = Some(unit.into());
    c
}

#[test]
fn header_negation_or_comparator_is_not_passed() {
    let mismatch = (ClaimStatus::Unverified, "qualifier_mismatch");
    let rows = [
        (
            "อนุมัติงบประมาณ 1,250 ล้านบาท",
            cell(with_header_unit(
                row(number_cell("1250"), "ไม่อนุมัติงบประมาณ"),
                "ล้านบาท",
            )),
        ),
        (
            "approved budget 1,250",
            cell(row(number_cell("1250"), "budget not approved")),
        ),
        (
            "budget 1,250",
            cell(row(number_cell("1250"), "budget at most")),
        ),
        (
            "budget 1,250",
            cell(col(row(number_cell("1250"), "budget"), "at least")),
        ),
        (
            "งบประมาณ 1,250 ล้านบาท",
            cell(with_header_unit(
                row(number_cell("1250"), "งบประมาณไม่เกิน"),
                "ล้านบาท",
            )),
        ),
    ];
    for (text, r) in rows {
        let c = number(text, r);
        assert_eq!(status(&c), mismatch, "{text}");
        assert!(
            of_method(&c, CheckMethod::Qualifier)
                .iter()
                .any(|o| o.code == "qualifier_mismatch")
        );
    }
    // A header with no qualifier passes.
    let c = number("budget 1,250", cell(row(number_cell("1250"), "budget")));
    assert_eq!(status(&c), (ClaimStatus::Passed, "checked"));
    // Dates.
    let s = serial(2025, 10, 1, false);
    let r = cell(row(fmt(number_cell(&s), "d/m/yyyy"), "ไม่ได้ประชุมวันที่"));
    assert_eq!(status(&date("ประชุมวันที่ 1 ต.ค. 2568", r)), mismatch);
    // Operands.
    let c = derived(
        "total 30",
        DeriveOp::Sum,
        vec![
            cell(row(number_cell("10"), "travel not approved")),
            q("food 20"),
        ],
    );
    assert_eq!(status(&c), mismatch);
}

#[test]
fn profit_or_loss_line_needs_a_negative_figure_for_a_loss() {
    let held = (Inconclusive, "sign_differs");
    for (text, r) in [
        ("net loss 1,250", q("net profit (loss) 1,250")),
        ("net loss 1,250", q("net profit/(loss) 1,250")),
        ("ขาดทุน 1,250 ล้านบาท", q("กำไร (ขาดทุน) 1,250 ล้านบาท")),
        ("ขาดทุน 1,250", q("กำไร(ขาดทุน)สุทธิ 1,250")),
        (
            "ขาดทุน 1,250 ล้านบาท",
            cell(with_header_unit(
                row(number_cell("1250"), "กำไร (ขาดทุน)"),
                "ล้านบาท",
            )),
        ),
    ] {
        let c = number(text, r);
        assert_figure(&c, held);
        assert_eq!(
            status(&c),
            (ClaimStatus::Unverified, "sign_differs"),
            "{text}"
        );
    }
    // A negative figure on the line is the loss.
    for (text, r) in [
        (
            "ขาดทุน 1,250 ล้านบาท",
            cell(with_header_unit(
                row(number_cell("-1250"), "กำไร (ขาดทุน)"),
                "ล้านบาท",
            )),
        ),
        ("net loss 1,250", q("net profit (loss) (1,250)")),
    ] {
        assert_figure(&number(text, r), OK);
    }
    // A profit claim on the line keeps its plain reading.
    assert_figure(&number("กำไร 1,250", q("กำไร (ขาดทุน) 1,250")), OK);
}

#[test]
fn figure_binds_to_its_own_words() {
    let other_row = (Inconclusive, "subject_not_in_source");
    for (text, source) in [
        ("revenue 1,250", "revenue 1,300 cost 1,250"),
        (
            "รายได้ 1,250 ล้านบาท",
            "รายได้ 1,300 ล้านบาท ค่าใช้จ่าย 1,250 ล้านบาท",
        ),
        ("revenue 1,250", "cost 1,250 revenue 900"),
    ] {
        assert_figure(&number(text, q(source)), other_row);
    }
    for (text, source) in [
        ("revenue 1,250", "revenue 1,250 cost 1,300"),
        // A quote that opens with a figure puts each figure's words after it.
        ("staff 5", "5 staff and 3 managers"),
        ("managers 3", "5 staff and 3 managers"),
        ("revenue 1,250", "cost 900, revenue 1,250"),
        ("total 3,450", "total 3,450 and 200"),
    ] {
        assert_figure(&number(text, q(source)), OK);
    }
    // A range or a factor qualifies both its figures.
    for (text, source) in [
        ("revenue 1,300", "revenue 1,250 to 1,300"),
        ("revenue 1,250", "revenue 1,250 to 1,300"),
        ("revenue 1,300", "revenue 1,250–1,300"),
        ("revenue 1,250", "revenue 1,000-1,250"),
        ("รายได้ 1,300 ล้านบาท", "รายได้ 1,250 ถึง 1,300 ล้านบาท"),
        ("revenue 1,250", "revenue between 1,250 and 1,300"),
        ("revenue 1,250", "revenue 1,250 x 2"),
    ] {
        let c = number(text, q(source));
        assert_eq!(
            status(&c),
            (ClaimStatus::Unverified, "qualifier_mismatch"),
            "{text} / {source}"
        );
    }
    // A sum's period and entity words must hold for every operand.
    let c = derived(
        "รายได้ปี 2567 รวม 30",
        DeriveOp::Sum,
        vec![q("รายได้ปี 2566 10"), q("รายได้ปี 2567 20")],
    );
    assert_figure(&c, (Inconclusive, "period_mismatch"));
    let c = derived(
        "Entity B total 30",
        DeriveOp::Sum,
        vec![q("Entity A travel 10"), q("Entity B food 20")],
    );
    assert_figure(&c, other_row);
    let c = derived(
        "รายได้ปี 2567 รวม 30",
        DeriveOp::Sum,
        vec![q("รายได้ปี 2567 10"), q("รายได้ปี 2567 20")],
    );
    assert_figure(&c, OK);
    // A percent change spans two periods: the union holds.
    let c = derived(
        "รายได้ เพิ่มขึ้น 50%",
        DeriveOp::PercentChange,
        vec![q("รายได้ปี 2566 10"), q("รายได้ปี 2567 15")],
    );
    assert_figure(&c, OK);
}

#[test]
fn capital_a_names_an_entity() {
    for (text, source) in [
        ("Entity A revenue 1,250 baht", "Entity B revenue 1,250 baht"),
        ("Plan A budget 1,250", "Plan B budget 1,250"),
        // An article in the source is not the entity letter.
        ("Plan A budget 1,250", "a plan budget 1,250"),
    ] {
        assert_figure(
            &number(text, q(source)),
            (Inconclusive, "subject_not_in_source"),
        );
    }
    assert_figure(
        &number(
            "Entity A revenue 1,250 baht",
            q("Entity A revenue 1,250 baht"),
        ),
        OK,
    );
    // The article stays a stop word.
    assert_figure(&number("a revenue of 1,250", q("revenue 1,250")), OK);
}

#[test]
fn percentage_points_are_not_percent() {
    for (text, source) in [
        ("margin up 2.5%", "margin up 2.5% points"),
        ("margin up 2.5%", "margin up 2.5% pp"),
        ("อัตรากำไร เพิ่มขึ้นร้อยละ 2.5", "อัตรากำไร เพิ่มขึ้นร้อยละ 2.5 จุด"),
    ] {
        assert_figure(&number(text, q(source)), (Inconclusive, "scale_unstated"));
    }
    assert_figure(
        &number("margin up 2.5% points", q("margin up 2.5% points")),
        OK,
    );
}

#[test]
fn operand_scales_are_compared_like_a_number() {
    for (text, refs) in [
        ("total 30 ล้าน", vec![q("travel 10"), q("food 20")]),
        ("total 30 ล้านบาท", vec![q("travel 10 ล้านบาท"), q("food 20")]),
    ] {
        let c = derived(text, DeriveOp::Sum, refs);
        assert_figure(&c, (Inconclusive, "scale_unstated"));
    }
    let c = derived(
        "total 30 ล้าน",
        DeriveOp::Sum,
        vec![q("travel 10 ล้าน"), q("food 20 ล้าน")],
    );
    assert_figure(&c, OK);
    let c = derived(
        "total 30,000,000",
        DeriveOp::Sum,
        vec![q("travel 10 ล้าน"), q("food 20 ล้าน")],
    );
    assert_figure(&c, OK);
}

#[test]
fn refs_past_the_cap_are_not_compared() {
    let refs: Vec<ResolvedRef> = (0..17).map(|_| q("รายได้ 1,250")).collect();
    let c = claim(ClaimKind::Number, "รายได้ 1,250", refs);
    let found = checks(&c);
    let numbers: Vec<_> = found
        .iter()
        .filter(|o| o.method == CheckMethod::Number)
        .collect();
    assert_eq!(numbers.len(), 17);
    assert_eq!(numbers.iter().filter(|o| o.code == "checked").count(), 16);
    let capped: Vec<_> = numbers
        .iter()
        .filter(|o| o.code == "too_many_refs")
        .collect();
    assert_eq!(capped.len(), 1);
    assert_eq!(
        (capped[0].verdict, capped[0].ref_ordinal),
        (Inconclusive, Some(16))
    );
    assert_eq!(status(&c), (ClaimStatus::Unverified, "too_many_refs"));
    let refs: Vec<ResolvedRef> = (0..17).map(|_| q("travel 1")).collect();
    let c = derived("travel 17", DeriveOp::Sum, refs);
    assert_figure(&c, (Inconclusive, "too_many_refs"));
}

#[test]
fn one_significant_digit_does_not_absorb_a_finer_source() {
    for (text, source) in [
        ("รายได้ 2 พันล้านบาท", "รายได้ 1,500 ล้านบาท"),
        ("revenue 1 billion", "revenue 1,499 million"),
        ("revenue 5", "revenue 5.2"),
    ] {
        assert_figure(
            &number(text, q(source)),
            (Inconclusive, "precision_differs"),
        );
    }
    assert_figure(&number("revenue 2 billion", q("revenue 2,000 million")), OK);
    assert_figure(&number("revenue 5", q("revenue 5")), OK);
}

#[test]
fn operands_are_read_in_order() {
    // (b - a) / a, with no absolute value: -10 then -15 is +50%.
    let c = derived(
        "growth 50%",
        DeriveOp::PercentChange,
        vec![q("growth base -10"), q("growth now -15")],
    );
    assert_figure(&c, OK);
    let c = derived(
        "growth -50%",
        DeriveOp::PercentChange,
        vec![q("growth base -10"), q("growth now -15")],
    );
    assert_figure(&c, (Inconclusive, "arithmetic_mismatch"));
    let c = derived(
        "growth 50%",
        DeriveOp::PercentChange,
        vec![q("growth now 15"), q("growth base 10")],
    );
    assert_figure(&c, (Inconclusive, "operand_order"));
    let c = derived(
        "sales difference 5",
        DeriveOp::Difference,
        vec![q("sales 10"), q("sales 15")],
    );
    assert_figure(&c, (Inconclusive, "operand_order"));
    let c = derived(
        "sales 1.5",
        DeriveOp::Ratio,
        vec![q("sales 10"), q("sales 15")],
    );
    assert_figure(&c, (Inconclusive, "operand_order"));
}

#[test]
fn short_thai_sign_word_is_a_whole_word() {
    assert_figure(
        &number("ผลบวก 1,250", q("ผลบวก (1,250)")),
        (Contradicted, "number_mismatch"),
    );
}

#[test]
fn direction_and_ratio_words_are_the_operation() {
    let ops = || vec![q("sales 20"), q("sales 10")];
    assert_figure(
        &derived("sales decreased 50%", DeriveOp::PercentChange, ops()),
        OK,
    );
    assert_figure(
        &derived("sales fell 50%", DeriveOp::PercentChange, ops()),
        OK,
    );
    assert_figure(&derived("sales ratio 2", DeriveOp::Ratio, ops()), OK);
    assert_figure(&derived("sales อัตราส่วน 2", DeriveOp::Ratio, ops()), OK);
    let up = || vec![q("sales 10"), q("sales 20")];
    assert_figure(
        &derived("sales rose 100%", DeriveOp::PercentChange, up()),
        OK,
    );
}

// ---------------------------------------------------------------- lists

#[test]
fn word_lists_are_written_as_normalize_th_writes_them() {
    for word in STOP_WORDS.iter().chain(OP_WORDS) {
        assert_eq!(katwork_thai::normalize_th(word).text, *word, "{word:?}");
    }
}

#[test]
fn every_new_code_is_listed() {
    for code in [
        "number_ambiguous",
        "sign_differs",
        "precision_differs",
        "too_many_refs",
        "operand_order",
    ] {
        assert!(CODES.contains(&code), "{code}");
    }
}
