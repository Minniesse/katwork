//! The detail codes a claim check or a roll-up can carry (ADR-0007). The web shows one line of
//! copy per code (Task 29), and `claim_checks` stores them, so the list only grows.

/// Every detail code of the verification ladder (Tasks 10 and 11), plus `checked`. Task 12
/// appends `claim_cap` and `verification_unavailable`, Task 25 `claim_lost_in_edit`, Task 31 its
/// other judge codes and Task 41 `ocr_number_capped`.
pub const CODES: &[&str] = &[
    // Roll-up.
    "checked",
    "uncited",
    "needs_review",
    // Resolve (Task 10).
    "stale",
    "source_generated",
    "hidden_source",
    "instruction_like_source",
    "suspect_text_layer",
    "source_deleted",
    "ocr_unaligned",
    "ocr_low_confidence",
    // Quote and qualifiers (Task 10).
    "paraphrase",
    "needs_judge",
    "qualifier_mismatch",
    // The judge outcomes `roll_up` returns (Task 10; Task 31 adds the judge's own codes).
    "judge_contradicted",
    "judge_unsupported",
    "judged_supported",
    // Numbers, dates, cells and arithmetic (Task 11).
    "text_too_long",
    "claim_number_ambiguous",
    "subject_not_in_source",
    "quote_cuts_number",
    "period_mismatch",
    "period_kind_differs",
    "period_ambiguous",
    "date_ambiguous",
    "scale_unstated",
    "unit_mismatch",
    "number_mismatch",
    "number_not_in_source",
    "amount_words_mismatch",
    "date_mismatch",
    "date_not_in_source",
    "cell_not_calculated",
    "cell_cache_untrusted",
    "operand_units_differ",
    "arithmetic_mismatch",
    "operand_missing",
    "division_by_zero",
    // Task 11's own: a figure with another reading, a sign that only the wording explains, a
    // claim finer than its source.
    "number_ambiguous",
    "sign_differs",
    "precision_differs",
    // Fix round 1: refs past the per-claim cap, operands that match only in the other order.
    "too_many_refs",
    "operand_order",
];

/// The visible flag "AI found a possible mismatch" (R4, R18). It never changes a status.
pub const FLAG_AI_POSSIBLE_MISMATCH: &str = "ai_possible_mismatch";
