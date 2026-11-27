//! Evidence: source refs into a file's extracted text, the spans the harness mints over them,
//! the citations `cite` makes over spans, the claims writers record, and the instruction
//! detector (ADR-0007, ADR-0010). Kats never supply a locator: they cite span ids the harness
//! minted.
pub mod citation;
pub mod cite;
pub mod claim;
pub mod codes;
pub mod instruction;
pub mod source_ref;
pub mod span;
pub mod verify;

/// The context every evidence scan of figures and dates uses: two-digit years are read around a
/// fixed day, so where `cite` widens a quote, and what the verifier finds in it later, never
/// depends on today's date.
pub fn scan_ctx() -> katwork_thai::Ctx {
    katwork_thai::Ctx {
        anchor: chrono::NaiveDate::from_ymd_opt(2024, 1, 1),
    }
}
