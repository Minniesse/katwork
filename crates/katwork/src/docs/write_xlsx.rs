//! The XLSX writer (SP2a Task 9). Trust core: no formulas (every string is written as a
//! string, so `=SUM(..)` stays text), and each claim cell gets a note whose first line is
//! `kw-claim-<n>` (`n` the claim's final ordinal) and whose other lines are its sources'
//! `display`, never `exact` or the source ref.
use rust_xlsxwriter::{Format, Note, Workbook};

use super::{
    DocError, Rendered,
    content::{
        CellSpec, ClaimInline, ClaimSpec, MAX_NUMBER_DIGITS, NumberCell, PlacedClaim, SheetSpec,
        WrittenContent, cell_location, cited_ordinals, claim_specs, significant_digits,
        uncited_claims,
    },
    deadline,
    write_docx::Sources,
};
use crate::evidence::{citation::CitationRow, claim::ClaimKind};

/// Cells written between two deadline checks.
const CHECK_EVERY: usize = 256;

/// The number a claim's text is, with the format that shows it as written: `1250` (`0`),
/// `12.50` (`0.00`), `1,250` (`#,##0`) or `1,250.50` (`#,##0.00`), each with an optional `-`.
/// `None` for any other text, or a figure an `f64` cannot hold exactly.
fn plain_figure(text: &str) -> Option<(f64, String)> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    let (int, frac) = match digits.split_once('.') {
        Some((int, frac)) => (int, Some(frac)),
        None => (digits, None),
    };
    if frac.is_some_and(|f| f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    let grouped = int.contains(',');
    let groups: Vec<&str> = int.split(',').collect();
    let int_ok = if grouped {
        (1..=3).contains(&groups[0].len())
            && !groups[0].starts_with('0')
            && groups[1..].iter().all(|g| g.len() == 3)
            && groups.iter().all(|g| g.bytes().all(|b| b.is_ascii_digit()))
    } else {
        !int.is_empty()
            && int.bytes().all(|b| b.is_ascii_digit())
            && (int == "0" || !int.starts_with('0'))
    };
    if !int_ok {
        return None;
    }
    let plain: String = text.chars().filter(|&c| c != ',').collect();
    if significant_digits(&plain) > MAX_NUMBER_DIGITS {
        return None;
    }
    let value: f64 = plain.parse().ok()?;
    let mut format = if grouped { "#,##0" } else { "0" }.to_owned();
    if let Some(frac) = frac {
        format.push('.');
        format.push_str(&"0".repeat(frac.len()));
    }
    Some((value, format))
}

fn internal<E>(_: E) -> DocError {
    DocError::Internal
}

/// Renders `sheets` as an XLSX workbook. Checks the pool's deadline at each row and every 256
/// cells.
pub fn render_xlsx(sheets: &[SheetSpec], sources: &[CitationRow]) -> Result<Rendered, DocError> {
    let written = WrittenContent::Sheets(sheets.to_vec());
    let specs: Vec<&ClaimSpec> = claim_specs(&written);
    let uncited = uncited_claims(&written).map_err(|_| DocError::TooComplex("text"))?;
    let mut ordinals = cited_ordinals(specs.len(), &uncited).into_iter();
    let sources = Sources::new(specs.iter().copied(), sources)?;
    let mut claims = Vec::with_capacity(specs.len());
    let mut workbook = Workbook::new();
    let mut written_cells = 0usize;
    for sheet in sheets {
        let worksheet = workbook.add_worksheet();
        worksheet.set_name(&sheet.name).map_err(internal)?;
        for (r, row) in sheet.rows.iter().enumerate() {
            deadline::check()?;
            let row_index = u32::try_from(r).map_err(internal)?;
            for (c, cell) in row.iter().enumerate() {
                written_cells += 1;
                if written_cells.is_multiple_of(CHECK_EVERY) {
                    deadline::check()?;
                }
                let col = u16::try_from(c).map_err(internal)?;
                match cell {
                    CellSpec::Empty => {}
                    CellSpec::Text(text) => {
                        worksheet
                            .write_string(row_index, col, text)
                            .map_err(internal)?;
                    }
                    CellSpec::Number(NumberCell { number }) => {
                        if significant_digits(number) > MAX_NUMBER_DIGITS {
                            worksheet
                                .write_string(row_index, col, number)
                                .map_err(internal)?;
                        } else {
                            let value: f64 = number.parse().map_err(internal)?;
                            worksheet
                                .write_number(row_index, col, value)
                                .map_err(internal)?;
                        }
                    }
                    CellSpec::Claim(ClaimInline { claim }) => {
                        let ordinal = ordinals.next().ok_or(DocError::Internal)?;
                        match plain_figure(&claim.text).filter(|_| claim.kind == ClaimKind::Number)
                        {
                            Some((value, format)) => worksheet
                                .write_number_with_format(
                                    row_index,
                                    col,
                                    value,
                                    &Format::new().set_num_format(&format),
                                )
                                .map_err(internal)?,
                            None => worksheet
                                .write_string(row_index, col, &claim.text)
                                .map_err(internal)?,
                        };
                        let mut note = vec![super::write_docx::bookmark_name(ordinal)];
                        note.extend(sources.displays_of(claim));
                        worksheet
                            .insert_note(
                                row_index,
                                col,
                                &Note::new(note.join("\n"))
                                    .set_author("KatWork")
                                    .add_author_prefix(false),
                            )
                            .map_err(internal)?;
                        claims.push(PlacedClaim {
                            ordinal,
                            kind: claim.kind,
                            text: claim.text.clone(),
                            location: cell_location(&sheet.name, r, c),
                            cite: claim.cite.clone(),
                            derive: claim.derive.clone(),
                            cited: true,
                        });
                    }
                }
            }
        }
    }
    deadline::check()?;
    let bytes = workbook.save_to_buffer().map_err(internal)?;
    Ok(Rendered { bytes, claims })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_figures_and_their_formats() {
        for (text, value, format) in [
            ("1250", 1250.0, "0"),
            ("-3", -3.0, "0"),
            ("12.50", 12.5, "0.00"),
            ("1,250", 1250.0, "#,##0"),
            ("1,234,567.25", 1_234_567.25, "#,##0.00"),
            ("0.5", 0.5, "0.0"),
        ] {
            assert_eq!(
                plain_figure(text),
                Some((value, format.to_owned())),
                "{text}"
            );
        }
        for text in [
            "",
            "1,25",
            "01",
            "1,2345",
            ",250",
            "1.",
            "ราว 1,250",
            "1e5",
            "1234567890123456",
            "๑๒",
            "-",
            "0,123",
        ] {
            assert_eq!(plain_figure(text), None, "{text}");
        }
    }
}
