//! Evidence: source refs into a file's extracted text, the spans the harness mints over them,
//! the claim types writers and the verifier share, and the instruction detector (ADR-0007,
//! ADR-0010). Kats never supply a locator: they cite span ids the harness minted.
pub mod claim;
pub mod instruction;
pub mod source_ref;
pub mod span;
