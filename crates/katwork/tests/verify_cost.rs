//! The cost of verifying one claim with many long refs. Ignored by default: run in release with
//! `cargo test --release -p katwork --test verify_cost -- --ignored --nocapture`; it prints the
//! time taken and asserts only on what the checks return, never on time.
use katwork::evidence::claim::{ClaimKind, Derive, DeriveOp};
use katwork::evidence::scan_ctx;
use katwork::evidence::verify::{ClaimInput, RefQuality, ResolvedRef, check_claim};

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
    let derived = ClaimInput {
        text: "รายได้รวม 318,750 ล้านบาท".into(),
        derive: Some(Derive {
            op: DeriveOp::Sum,
            args: (0..REFS as u8).collect(),
        }),
        ..plain
    };
    timed("derived", &derived);
}
