//! Read-back: a rendered file re-read with KatWork's own readers, and each cited claim found
//! at the place the renderer put it (SP2a Task 9). Trust core.
use std::collections::HashMap;

use katwork_thai::match_key;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{DOCUMENT_PART, internal, xlsx_notes::xlsx_note_markers};
use crate::{
    agent::{ToolDeps, ToolError},
    artifacts::{MediaType, tools::from_doc},
    docs::{
        Block, DocError, Extracted, Locator, content::PlacedClaim, deadline, package, xml_guard,
    },
    evidence::claim::MAX_CLAIM_CHARS,
    ids::WorkspaceId,
};

/// Claim bookmarks one paragraph may hold open at once.
const MAX_OPEN_BOOKMARKS: usize = 16;

/// Appends `text` to a bookmark's text, refusing more than a claim can hold.
fn push_bounded(mark: &mut String, text: &str) -> Result<(), DocError> {
    mark.push_str(text);
    if mark.chars().count() > MAX_CLAIM_CHARS {
        return Err(DocError::TooComplex("bookmark_text"));
    }
    Ok(())
}

/// A `kw-claim-<n>` bookmark of a DOCX main part: its name, the 1-based ordinal of the `w:p` it
/// starts in (the reader's paragraph ordinal), and the text of the `w:t` elements between its
/// start and its end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimBookmark {
    pub name: String,
    pub paragraph: u32,
    pub text: String,
}

/// The bookmarks whose name starts `kw-claim-`, in document order of their ends. KatWork's
/// readers do not expose bookmarks; Task 25's claim markers should reuse this scanner.
pub fn docx_claim_bookmarks(document_xml: &[u8]) -> Result<Vec<ClaimBookmark>, DocError> {
    use quick_xml::{
        events::{BytesStart, Event},
        name::ResolveResult,
    };
    const WORD_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
    let mut reader = xml_guard::reader(document_xml);
    let mut buf = Vec::new();
    let mut paragraphs = 0u32;
    let mut in_text = 0u32;
    // Open bookmarks by `w:id`.
    let mut open: HashMap<String, ClaimBookmark> = HashMap::new();
    let mut done = Vec::new();
    let attribute = |start: &BytesStart, local: &str| -> Result<Option<String>, DocError> {
        for attr in start.attributes() {
            let attr = attr.map_err(|_| DocError::Malformed("xml_attribute"))?;
            if attr.key.local_name().as_ref() == local {
                return Ok(Some(xml_guard::attribute_value(&attr.value)?.into_owned()));
            }
        }
        Ok(None)
    };
    let mut events = 0u32;
    loop {
        events = events.wrapping_add(1);
        if events.is_multiple_of(1024) {
            deadline::check()?;
        }
        buf.clear();
        let (resolved, event) = reader
            .read_resolved_event_into(&mut buf)
            .map_err(xml_guard::map_error)?;
        // Only WordprocessingML elements count: a DrawingML `a:p` is no paragraph of the reader.
        let word = matches!(resolved, ResolveResult::Bound(ns) if ns.0 == WORD_NS);
        match event {
            Event::Start(ref start) | Event::Empty(ref start) if word => {
                let empty = matches!(event, Event::Empty(_));
                match start.local_name().as_ref() {
                    "p" => paragraphs += 1,
                    "t" if !empty => in_text += 1,
                    "bookmarkStart" => {
                        let (Some(id), Some(name)) =
                            (attribute(start, "id")?, attribute(start, "name")?)
                        else {
                            continue;
                        };
                        if name.starts_with("kw-claim-") {
                            // KatWork's renderers close each bookmark in its paragraph; a file
                            // that keeps many open would make each text run cost them all.
                            if open.len() >= MAX_OPEN_BOOKMARKS {
                                return Err(DocError::TooComplex("open_bookmarks"));
                            }
                            open.insert(
                                id,
                                ClaimBookmark {
                                    name,
                                    paragraph: paragraphs,
                                    text: String::new(),
                                },
                            );
                        }
                    }
                    "bookmarkEnd" => {
                        if let Some(id) = attribute(start, "id")?
                            && let Some(mark) = open.remove(&id)
                        {
                            done.push(mark);
                        }
                    }
                    _ => {}
                }
            }
            Event::End(ref end) if word => {
                if end.local_name().as_ref() == "t" {
                    in_text = in_text.saturating_sub(1);
                }
            }
            Event::Text(text) if in_text > 0 && !open.is_empty() => {
                let text = text.xml10_content();
                for mark in open.values_mut() {
                    push_bounded(&mut mark.text, &text)?;
                }
            }
            Event::GeneralRef(reference) if in_text > 0 && !open.is_empty() => {
                let c = xml_guard::resolve_ref(&reference)?;
                for mark in open.values_mut() {
                    push_bounded(&mut mark.text, c.encode_utf8(&mut [0; 4]))?;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(done)
}

/// The 1-based row and column of an A1 reference.
fn parse_a1(a1: &str) -> Option<(u32, u32)> {
    let split = a1.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = a1.split_at(split);
    if letters.is_empty() || !letters.bytes().all(|b| b.is_ascii_uppercase()) {
        return None;
    }
    let col = letters.bytes().try_fold(0u32, |n, b| {
        n.checked_mul(26)?.checked_add(u32::from(b - b'A' + 1))
    })?;
    let row: u32 = digits.parse().ok()?;
    (row > 0 && !digits.starts_with('0')).then_some((row, col))
}

fn visible(block: &Block) -> bool {
    block.hidden.is_none()
        && block.suspect.is_none()
        && block.cell.as_ref().is_none_or(|cell| !cell.hidden)
}

/// The blocks of one extraction, indexed by where a claim may be.
#[derive(Default)]
struct Placed<'a> {
    /// A DOCX main-part paragraph's pieces joined, and whether every piece is visible.
    paragraphs: HashMap<u32, (String, bool)>,
    cells: HashMap<(&'a str, u32, u32), (String, bool)>,
    /// Each PDF page's visible blocks, in order.
    pages: HashMap<u32, Vec<&'a str>>,
}

impl<'a> Placed<'a> {
    fn new(extracted: &'a Extracted) -> Result<Self, DocError> {
        let mut placed = Placed::default();
        for (index, block) in extracted.blocks.iter().enumerate() {
            if index % 1024 == 1023 {
                deadline::check()?;
            }
            let seen = visible(block);
            let join = |entry: &mut (String, bool)| {
                entry.0.push_str(&block.text);
                entry.1 &= seen;
            };
            match &block.locator {
                Locator::DocxParagraph { part, ordinal, .. } if &**part == DOCUMENT_PART => {
                    join(
                        placed
                            .paragraphs
                            .entry(*ordinal)
                            .or_insert((String::new(), true)),
                    );
                }
                Locator::XlsxCell {
                    sheet, row, col, ..
                } => join(
                    placed
                        .cells
                        .entry((sheet.as_str(), *row, *col))
                        .or_insert((String::new(), true)),
                ),
                Locator::PdfPage { page } if seen => {
                    placed.pages.entry(*page).or_default().push(&block.text);
                }
                _ => {}
            }
        }
        Ok(placed)
    }

    /// Whether `claim`'s text, after `match_key`, is where its location says.
    fn holds(&self, claim: &PlacedClaim, marks: &HashMap<&str, Option<&ClaimBookmark>>) -> bool {
        let want = match_key(&claim.text).text;
        if want.is_empty() {
            return false;
        }
        if let Some(name) = claim.location.strip_prefix("bookmark:") {
            // The bookmark wraps exactly the claim, in a paragraph the reader shows whole.
            let Some(Some(mark)) = marks.get(name) else {
                return false;
            };
            return match_key(&mark.text).text == want
                && self
                    .paragraphs
                    .get(&mark.paragraph)
                    .is_some_and(|(text, seen)| *seen && match_key(text).text.contains(&want));
        }
        if let Some(cell) = claim.location.strip_prefix("cell:") {
            // A sheet name may hold `!`: the reference follows the last one.
            let Some((sheet, a1)) = cell.rsplit_once('!') else {
                return false;
            };
            let Some((row, col)) = parse_a1(a1) else {
                return false;
            };
            return self
                .cells
                .get(&(sheet, row, col))
                .is_some_and(|(text, seen)| *seen && match_key(text).text == want);
        }
        if let Some(page) = claim.location.strip_prefix("page:") {
            let Ok(page) = page.parse::<u32>() else {
                return false;
            };
            let Some(blocks) = self.pages.get(&page) else {
                return false;
            };
            let next = page
                .checked_add(1)
                .and_then(|n| self.pages.get(&n))
                .and_then(|b| b.first());
            // A claim that breaks across the page starts in a block of this page and ends in the
            // next page's first block, whatever blocks (footnotes) follow it on this page.
            return blocks.iter().any(|block| {
                let own = match_key(block).text;
                if own.contains(&want) {
                    return true;
                }
                let Some(next) = next else {
                    return false;
                };
                let joined = match_key(&format!("{block} {next}")).text;
                let own_len = own.chars().count();
                joined.match_indices(&want).any(|(at, _)| {
                    let start = joined[..at].chars().count();
                    start < own_len && start + want.chars().count() > own_len
                })
            });
        }
        false
    }
}

/// Re-extracts `bytes` with KatWork's own reader, in the pool and never from the extraction
/// cache (the bytes are not committed), and checks that each cited claim's text is at its
/// location: in its `bookmark:` (DOCX), its `cell:` (XLSX) or on its `page:` (PDF). Any claim
/// that is not is `read_back_failed`.
pub async fn read_back(
    media: MediaType,
    bytes: &[u8],
    claims: &[PlacedClaim],
    deps: &ToolDeps,
    workspace: WorkspaceId,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<(), ToolError> {
    if !matches!(media, MediaType::Docx | MediaType::Xlsx | MediaType::Pdf) {
        return Err(internal());
    }
    let extract = deps.extract.clone();
    let bytes = bytes.to_vec();
    let claims = claims.to_vec();
    let held = deps
        .docs
        .run(workspace, cancel, Some(deadline), move |limits| {
            let extracted = extract(media.kind(), &bytes, limits)?;
            let (marks, notes) = match media {
                MediaType::Docx => {
                    let package = package::open_package(&bytes, limits)?;
                    let part = package
                        .part(DOCUMENT_PART)
                        .ok_or(DocError::Malformed("docx_main_part"))?;
                    (docx_claim_bookmarks(part)?, HashMap::new())
                }
                MediaType::Xlsx => {
                    let package = package::open_package(&bytes, limits)?;
                    (Vec::new(), xlsx_note_markers(&package)?)
                }
                _ => (Vec::new(), HashMap::new()),
            };
            // A name used twice locates nothing.
            let mut by_name: HashMap<&str, Option<&ClaimBookmark>> = HashMap::new();
            for mark in &marks {
                by_name
                    .entry(mark.name.as_str())
                    .and_modify(|m| *m = None)
                    .or_insert(Some(mark));
            }
            let placed = Placed::new(&extracted)?;
            for (index, claim) in claims.iter().enumerate() {
                if index % 256 == 255 {
                    deadline::check()?;
                }
                if !placed.holds(claim, &by_name) {
                    return Ok(false);
                }
                // An XLSX claim cell's note starts with its marker, which Task 25 reads.
                if let Some(cell) = claim.location.strip_prefix("cell:")
                    && notes.get(cell) != Some(&format!("kw-claim-{}", claim.ordinal))
                {
                    return Ok(false);
                }
            }
            Ok(true)
        })
        .await
        .map_err(from_doc)?;
    if held {
        Ok(())
    } else {
        Err(ToolError::Coded("read_back_failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a1_references_parse() {
        assert_eq!(parse_a1("A1"), Some((1, 1)));
        assert_eq!(parse_a1("B2"), Some((2, 2)));
        assert_eq!(parse_a1("AA10"), Some((10, 27)));
        assert_eq!(parse_a1("XFD1048576"), Some((1_048_576, 16_384)));
        for bad in ["", "A", "1", "a1", "A0", "A01", "A1B"] {
            assert_eq!(parse_a1(bad), None, "{bad}");
        }
    }

    #[test]
    fn bookmarks_scan_names_paragraphs_and_text() {
        let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
            <w:p><w:r><w:t>a</w:t></w:r></w:p>
            <a:p xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:t>no</a:t></a:p>
            <w:p><w:r><w:t>b </w:t></w:r><w:bookmarkStart w:id="2" w:name="kw-claim-2"/><w:r><w:t>x &amp; &#3585;</w:t></w:r><w:r><w:t>y</w:t></w:r><w:bookmarkEnd w:id="2"/></w:p>
            <w:p><w:bookmarkStart w:id="9" w:name="_Other"/><w:r><w:t>z</w:t></w:r><w:bookmarkEnd w:id="9"/></w:p>
            </w:body></w:document>"#;
        assert_eq!(
            docx_claim_bookmarks(xml).unwrap(),
            vec![ClaimBookmark {
                name: "kw-claim-2".into(),
                paragraph: 2,
                text: "x & กy".into(),
            }]
        );
    }

    #[test]
    fn bookmarks_are_bounded() {
        let w = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p>"#;
        let open: String = (0..=MAX_OPEN_BOOKMARKS)
            .map(|i| format!(r#"<w:bookmarkStart w:id="{i}" w:name="kw-claim-{i}"/>"#))
            .collect();
        let xml = format!("{w}{open}<w:r><w:t>x</w:t></w:r></w:p></w:body></w:document>");
        assert_eq!(
            docx_claim_bookmarks(xml.as_bytes()),
            Err(DocError::TooComplex("open_bookmarks"))
        );
        let long = format!(
            r#"{w}<w:bookmarkStart w:id="1" w:name="kw-claim-1"/><w:r><w:t>{}</w:t></w:r><w:bookmarkEnd w:id="1"/></w:p></w:body></w:document>"#,
            "ก".repeat(MAX_CLAIM_CHARS + 1)
        );
        assert_eq!(
            docx_claim_bookmarks(long.as_bytes()),
            Err(DocError::TooComplex("bookmark_text"))
        );
        let fits = long.replace(
            &"ก".repeat(MAX_CLAIM_CHARS + 1),
            &"ก".repeat(MAX_CLAIM_CHARS),
        );
        assert_eq!(docx_claim_bookmarks(fits.as_bytes()).unwrap().len(), 1);
    }
}
