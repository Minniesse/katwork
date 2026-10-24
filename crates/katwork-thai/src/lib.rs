//! Thai text for KatWork's evidence checks.
//!
//! - [`normalize_th`] and [`match_key`] normalize text for matching, never with NFKC, and map
//!   every code point back to the input ([`Normalized::original_range`]).
//! - [`find_numbers`] reads figures in Thai, Latin and other digits, Thai number words, scale
//!   words and units, each with its role in the text; [`parse_number_words`] reads Thai number
//!   words alone.
//! - [`find_dates`] reads Buddhist-Era, Common-Era and fiscal dates and periods, and
//!   [`compare_periods`] compares two of them by meaning.
//!
//! Every reading carries its span in code points of the text it came from, its confidence and
//! flags saying how it was written. The crate depends on nothing in KatWork.
mod dates;
mod digits;
mod found;
mod normalize;
mod numbers;

pub use dates::{Ctx, DateValue, Era, PeriodCmp, PeriodKind, compare_periods, find_dates};
pub use digits::fold_digit;
pub use found::{Flags, Found, MAX_SCAN_CODE_POINTS, TooLong};
pub use normalize::{
    NORMALIZE_VERSION, Normalized, match_key, normalize_pass, normalize_th, pythainlp_compat,
};
pub use numbers::{Form, Number, NumberRole, Scale, Unit, find_numbers, parse_number_words};
