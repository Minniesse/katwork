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
//!
//! **The clean-context rule** (a founder decision): a figure or a date reads with confidence 0.8
//! or more only when it matches a pattern the reader knows exactly and no signal of a range, a
//! list, a fraction, a period or a sign stands near it. Joining and opening signals (ถึง, จนถึง,
//! ตั้งแต่, จาก, ระหว่าง, และ, หรือ, `to`, `until`, `between`, `and`, `or`, a dash, slash, arrow
//! or comma) count next to the reading, with one space and a filler word such as วันที่ between;
//! sign and period signals (ติดลบ, ลบ, `minus`; ไตรมาส, ปีงบ, งวด, ครึ่ง, เดือนแรก, `Q2`, `H1`)
//! count within 15 code points of it. Near one, a reading is capped at 0.6 and the readings the
//! signal suggests go in `alts`, so a check against it is inconclusive, never a pass. The
//! signals are one table in the crate. A run the reader cannot read at all is one `Code`.
mod context;
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
