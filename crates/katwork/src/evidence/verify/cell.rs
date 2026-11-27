//! What a cell's own facts say before its value is compared: whether the value may be read at
//! all, and what it is (a number with its format's scaling, a date, or only the quoted text).
use chrono::NaiveDate;
use rust_decimal::{Decimal, RoundingStrategy};

use crate::docs::numfmt::{decimal, is_date_format, scaling, serial_to_date};
use crate::docs::{CellFacts, CellKind};

/// The significant digits Excel keeps of a number.
const PRECISION: u32 = 15;

/// The code of a `Cell` check that stops the cell's value from being compared, first match:
/// a hidden cell (`Resolve` should already have stopped it; this is the second line), a formula
/// with no cached value, a cached value the workbook asks to recalculate.
pub(super) fn gate(cell: &CellFacts) -> Option<&'static str> {
    if cell.hidden {
        Some("hidden_source")
    } else if cell.formula.is_some() && cell.cached.is_none() {
        Some("cell_not_calculated")
    } else if cell.cache_untrusted {
        Some("cell_cache_untrusted")
    } else {
        None
    }
}

/// A cell's value as the checks read it.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum CellValue {
    /// A `Number` cell: its cached value to Excel's 15 significant digits, then its format's
    /// percent signs (each ×100) and trailing commas (each ÷1,000, as it is shown).
    Number {
        value: Decimal,
        percent: bool,
        thousands: u32,
    },
    /// A date-formatted `Number` cell's day, `date1904` honoured; `None` for a serial that names
    /// no day.
    Date(Option<NaiveDate>),
    /// A `Text` cell: its quantities are those of the cited quote, as for any span.
    Quote,
    /// A `Bool` or `Error` cell, or a number that cannot be read: never a number (`TRUE` is not
    /// 1).
    Nothing,
}

pub(super) fn value(cell: &CellFacts) -> CellValue {
    match cell.kind {
        CellKind::Text => return CellValue::Quote,
        CellKind::Bool | CellKind::Error => return CellValue::Nothing,
        CellKind::Number => {}
    }
    let Some(cached) = cell.cached.as_deref() else {
        return CellValue::Nothing;
    };
    let format = cell.number_format.as_deref().unwrap_or("General");
    if is_date_format(format) {
        let day = cached
            .trim()
            .parse::<f64>()
            .ok()
            .and_then(|serial| serial_to_date(serial, cell.date1904));
        return CellValue::Date(day);
    }
    let Some(exact) = decimal(cached)
        .and_then(|d| d.round_sf_with_strategy(PRECISION, RoundingStrategy::MidpointAwayFromZero))
    else {
        return CellValue::Nothing;
    };
    let Some((percent, thousands)) = scaling(cached, format) else {
        return CellValue::Nothing;
    };
    let mut value = exact;
    for _ in 0..percent {
        let Some(scaled) = value.checked_mul(Decimal::ONE_HUNDRED) else {
            return CellValue::Nothing;
        };
        value = scaled;
    }
    CellValue::Number {
        value,
        percent: percent > 0,
        thousands,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn number(cached: &str, format: Option<&str>) -> CellFacts {
        CellFacts {
            kind: CellKind::Number,
            cached: Some(cached.into()),
            displayed: None,
            formula: None,
            number_format: format.map(Into::into),
            date1904: false,
            hidden: false,
            row_header: None,
            col_header: None,
            header_unit: None,
            cache_untrusted: false,
        }
    }

    #[test]
    fn number_cells_read_their_format_scaling() {
        let read = |cached, format| value(&number(cached, format));
        assert_eq!(
            read("0.125", Some("0.0%")),
            CellValue::Number {
                value: Decimal::new(125, 1),
                percent: true,
                thousands: 0
            }
        );
        assert_eq!(
            read("1250000000", Some("#,##0,,")),
            CellValue::Number {
                value: Decimal::new(1_250_000_000, 0),
                percent: false,
                thousands: 2
            }
        );
        assert_eq!(
            read("0.30000000000000004", None),
            CellValue::Number {
                value: Decimal::new(3, 1),
                percent: false,
                thousands: 0
            }
        );
        assert!(matches!(read("1.25E-05", None), CellValue::Number { .. }));
        assert_eq!(read("abc", None), CellValue::Nothing);
        assert_eq!(read("60", Some("d/m/yyyy")), CellValue::Date(None));
    }
}
