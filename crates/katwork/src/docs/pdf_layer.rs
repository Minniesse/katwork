//! Whether a page's text layer can be trusted, from what a person would see of it: the visible
//! text after `pdf_visibility::classify` and the images on the page. A suspect or absent page
//! still yields its blocks; the reader warns, and OCR (Task 41) reads such pages instead.
use super::TextLayer;

/// What `check_page` reads of one page.
#[derive(Debug, Clone, Default)]
pub struct PageText {
    /// The visible text of each line, in reading order.
    pub lines: Vec<String>,
    /// Visible glyphs that a person sees as a mark: any but known spaces, so a glyph whose
    /// code has no text still counts.
    pub visible_glyphs: usize,
    /// A visible glyph came from a font with no `/ToUnicode` map: a composite font (whose codes
    /// then have no text at all), or a simple font whose text is not ASCII.
    pub unmapped_font: bool,
    /// Image XObjects and inline images painted on the page.
    pub images: usize,
}

/// Thai above and below marks: MAI HAN-AKAT, SARA I to PHINTHU, MAITAIKHU to YAMAKKAN.
fn is_thai_mark(c: char) -> bool {
    matches!(c, '\u{0E31}' | '\u{0E34}'..='\u{0E3A}' | '\u{0E47}'..='\u{0E4E}')
}

fn is_thai_consonant(c: char) -> bool {
    matches!(c, '\u{0E01}'..='\u{0E2E}')
}

fn is_private(c: char) -> bool {
    matches!(c, '\u{E000}'..='\u{F8FF}' | '\u{F0000}'..='\u{FFFFD}' | '\u{100000}'..='\u{10FFFD}')
}

/// A Thai mark whose base, skipping other marks and SARA AM, is not a Thai consonant, or that
/// starts its line.
fn stray_mark(line: &str) -> bool {
    let chars: Vec<char> = line.chars().collect();
    chars.iter().enumerate().any(|(at, c)| {
        is_thai_mark(*c)
            && !chars[..at]
                .iter()
                .rev()
                .find(|before| !is_thai_mark(**before) && **before != '\u{0E33}')
                .is_some_and(|base| is_thai_consonant(*base))
    })
}

/// `Absent` when nothing visible is on the page but an image is; else `Suspect("fffd")` when
/// more than 1% of the visible characters are U+FFFD or private-use, `Suspect("marks")` for a
/// Thai mark with no base consonant, `Suspect("tounicode")` for a font with no ToUnicode map;
/// else `Ok`.
pub fn check_page(page: &PageText) -> TextLayer {
    if page.visible_glyphs == 0 {
        return if page.images > 0 {
            TextLayer::Absent
        } else {
            TextLayer::Ok
        };
    }
    let total: usize = page.lines.iter().map(|line| line.chars().count()).sum();
    let bad: usize = page
        .lines
        .iter()
        .map(|line| {
            line.chars()
                .filter(|c| *c == '\u{FFFD}' || is_private(*c))
                .count()
        })
        .sum();
    if total > 0 && bad * 100 > total {
        return TextLayer::Suspect("fffd");
    }
    if page.lines.iter().any(|line| stray_mark(line)) {
        return TextLayer::Suspect("marks");
    }
    if page.unmapped_font {
        return TextLayer::Suspect("tounicode");
    }
    TextLayer::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(lines: &[&str]) -> PageText {
        PageText {
            lines: lines.iter().map(|line| line.to_string()).collect(),
            visible_glyphs: lines
                .iter()
                .map(|line| line.chars().filter(|c| !c.is_whitespace()).count())
                .sum(),
            ..PageText::default()
        }
    }

    #[test]
    fn marks_need_a_consonant() {
        assert_eq!(check_page(&page(&["น้ำ กำไร ไม่"])), TextLayer::Ok);
        assert_eq!(
            check_page(&page(&["น\u{E49}\u{E4D}\u{E32}"])),
            TextLayer::Ok
        );
        assert_eq!(
            check_page(&page(&["\u{E48}ก"])),
            TextLayer::Suspect("marks")
        );
        assert_eq!(
            check_page(&page(&["a\u{E49}"])),
            TextLayer::Suspect("marks")
        );
    }

    #[test]
    fn replacement_characters_past_one_percent() {
        let clean = "x".repeat(200);
        assert_eq!(
            check_page(&page(&[&format!("{clean}\u{FFFD}")])),
            TextLayer::Ok
        );
        assert_eq!(
            check_page(&page(&[&format!("{clean}\u{FFFD}\u{E000}\u{FFFD}")])),
            TextLayer::Suspect("fffd")
        );
    }

    #[test]
    fn nothing_visible() {
        assert_eq!(
            check_page(&PageText {
                images: 1,
                ..page(&[" "])
            }),
            TextLayer::Absent
        );
        assert_eq!(check_page(&page(&[])), TextLayer::Ok);
        // Glyphs with no text are still marks on the page.
        let unmapped = PageText {
            visible_glyphs: 3,
            unmapped_font: true,
            images: 1,
            ..page(&[])
        };
        assert_eq!(check_page(&unmapped), TextLayer::Suspect("tounicode"));
    }
}
