//! The cost of verifying one claim with many long refs. Ignored by default: run in release with
//! `cargo test --release -p katwork --test verify_cost -- --ignored --nocapture`; it prints the
//! time taken and asserts only on what the checks return, never on time.
use katwork::evidence::claim::{ClaimKind, Derive, DeriveOp};
use katwork::evidence::scan_ctx;
use katwork::evidence::verify::{
    ClaimInput, RefQuality, ResolvedRef, check_claim, check_qualifiers, check_quote, check_resolve,
};

const REFS: usize = 255;

/// A block of about 200,000 code points whose first 19 are the quote.
fn long_ref() -> ResolvedRef {
    let line = "รายได้ 1,250 ล้านบาท ";
    let block = line.repeat(200_000 / line.chars().count());
    ResolvedRef::Ok {
        exact: "รายได้ 1,250 ล้านบาท".into(),
        block_text: block,
        quote_start: 0,
        quote_end: 19,
        unit_context: None,
        cell: None,
        sheet: None,
        quality: RefQuality::Full,
    }
}

/// The same block quoted whole.
fn whole_ref() -> ResolvedRef {
    let ResolvedRef::Ok { block_text, .. } = long_ref() else {
        unreachable!()
    };
    let len = block_text.chars().count() as u32;
    ResolvedRef::Ok {
        exact: block_text.clone(),
        block_text,
        quote_start: 0,
        quote_end: len,
        unit_context: None,
        cell: None,
        sheet: None,
        quality: RefQuality::Full,
    }
}

/// Task 10's checks alone, for the share of the time they take.
fn task_10(label: &str, claim: &ClaimInput) {
    let ctx = scan_ctx();
    let start = std::time::Instant::now();
    let mut checks = check_resolve(&claim.refs);
    checks.extend(check_quote(claim, &ctx));
    checks.extend(check_qualifiers(claim));
    eprintln!(
        "{label} (Task 10's checks only): {} checks, {:.1} s",
        checks.len(),
        start.elapsed().as_secs_f64()
    );
}

fn timed(label: &str, claim: &ClaimInput) {
    let start = std::time::Instant::now();
    let checks = check_claim(claim, &scan_ctx());
    eprintln!(
        "{label}: {} refs, {} checks, {:.1} s",
        claim.refs.len(),
        checks.len(),
        start.elapsed().as_secs_f64()
    );
    assert!(!checks.is_empty());
}

#[test]
#[ignore = "a measurement, run by hand in release"]
fn many_long_refs() {
    let refs: Vec<ResolvedRef> = (0..REFS).map(|_| long_ref()).collect();
    let plain = ClaimInput {
        kind: ClaimKind::Number,
        text: "รายได้ 1,250 ล้านบาท".into(),
        cited: true,
        derive: None,
        refs: refs.clone(),
    };
    timed("plain", &plain);
    task_10("plain", &plain);
    let derived = ClaimInput {
        text: "รายได้รวม 318,750 ล้านบาท".into(),
        derive: Some(Derive {
            op: DeriveOp::Sum,
            args: (0..REFS as u8).collect(),
        }),
        ..plain
    };
    timed("derived", &derived);
    let whole = ClaimInput {
        text: "รายได้ 1,250 ล้านบาท".into(),
        derive: None,
        refs: (0..REFS).map(|_| whole_ref()).collect(),
        ..derived
    };
    timed("whole-block quotes", &whole);
    task_10("whole-block quotes", &whole);
}
