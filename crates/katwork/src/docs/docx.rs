//! KatWork's DOCX reader (`docx-accepted` version 1): the accepted view (tracked changes
//! accepted) of the main part, then its headers, footers, footnotes and endnotes, with hidden
//! runs excluded. Where what Word draws cannot be decided, the text counts as hidden; where a
//! hidden run, a picture or a symbol the reader cannot decode is left out between two visible
//! stretches of a paragraph, U+FFFC stands in the gap, so the stretches never join into a
//! figure or a date Word does not show.
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    rc::Rc,
};

use quick_xml::{
    NsReader,
    events::{BytesStart, Event},
    name::{QName, ResolveResult},
};

use super::{
    Block, DocError, DocLimits, ExtractWarning, Extracted, ExtractorId, Locator,
    colour::{self, Rgb, SchemeSlot, ThemeColours},
    deadline,
    package::{self, Package},
    replace_nul, split_pieces,
    xml_guard::{self, ElementBudget},
};

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const W14: &str = "http://schemas.microsoft.com/office/word/2010/wordml";
const W16SE: &str = "http://schemas.microsoft.com/office/word/2015/wordml/symex";
const MC: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";
const WP: &str = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing";
const WPS: &str = "http://schemas.microsoft.com/office/word/2010/wordprocessingShape";
const WPG: &str = "http://schemas.microsoft.com/office/word/2010/wordprocessingGroup";
const WPC: &str = "http://schemas.microsoft.com/office/word/2010/wordprocessingCanvas";
const DRAWINGML: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const VML: &str = "urn:schemas-microsoft-com:vml";
const OFFICE: &str = "urn:schemas-microsoft-com:office:office";
const PACKAGE_RELATIONSHIPS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const RELATIONSHIP: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/";
const VBA_PROJECT: &str = "http://schemas.microsoft.com/office/2006/relationships/vbaProject";
const STRICT: &str = "http://purl.oclc.org/ooxml/";
/// Namespaces read besides WordprocessingML and markup compatibility: Word's 2010 and 2012
/// extensions, exactly the 2015 and later ones Word writes (`w16se`, `w16cid`, `w16`, `w16cex`,
/// `w16sdtdh`, `w16du`, `w16sdtfl`), the drawing namespaces and VML. Any other URI is unknown,
/// however much it looks like Word's.
const UNDERSTOOD: [&str; 19] = [
    W14,
    "http://schemas.microsoft.com/office/word/2012/wordml",
    W16SE,
    "http://schemas.microsoft.com/office/word/2016/wordml/cid",
    "http://schemas.microsoft.com/office/word/2018/wordml",
    "http://schemas.microsoft.com/office/word/2018/wordml/cex",
    "http://schemas.microsoft.com/office/word/2020/wordml/sdtdatahash",
    "http://schemas.microsoft.com/office/word/2023/wordml/word16du",
    "http://schemas.microsoft.com/office/word/2024/wordml/sdtformatlock",
    WP,
    "http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing",
    WPS,
    WPG,
    WPC,
    DRAWINGML,
    "http://schemas.openxmlformats.org/drawingml/2006/picture",
    VML,
    OFFICE,
    "urn:schemas-microsoft-com:office:word",
];
/// `w:basedOn` chains longer than this are refused rather than cut short.
const MAX_STYLE_CHAIN: usize = 16;
/// Runs kept in one paragraph (runs with text, or whose text was dropped); a paragraph with more
/// is refused before its runs grow further.
const MAX_PARAGRAPH_RUNS: usize = 65_536;
/// Styles one styles part may define; more is refused before the style map grows further.
const MAX_STYLES: usize = 65_536;
/// The characters of a field's instruction kept to read its type.
const MAX_FIELD_CODE: usize = 64;
/// `w:tblLook` when a table has none: Word's default (first row, first column, no column bands).
const WORD_TABLE_LOOK: u16 = 0x04A0;
/// What stands where the reader left out something Word draws between two visible stretches of
/// a paragraph: Task 1's scanners read no figure or date across it, and neither `normalize_th`
/// nor `match_key` removes it.
const GAP: char = '\u{FFFC}';
/// Word's largest page side, 22 inches, in EMU: a shape that far off its anchor is off the page
/// wherever the anchor lands.
const PAGE_MAX_EMU: i64 = 22 * 914_400;
/// `ST_HighlightColor` as Word draws it: the 16-colour palette of [MS-DOC] 2.9.119 `Ico` (the
/// values LibreOffice's DOCX import also uses). ECMA-376's 4th and 5th editions print CSS
/// values for six of the names (`darkBlue` `00008B` ...), which Word does not draw.
const HIGHLIGHTS: [(&str, &str); 16] = [
    ("black", "000000"),
    ("blue", "0000FF"),
    ("cyan", "00FFFF"),
    ("darkBlue", "000080"),
    ("darkCyan", "008080"),
    ("darkGray", "808080"),
    ("darkGreen", "008000"),
    ("darkMagenta", "800080"),
    ("darkRed", "800000"),
    ("darkYellow", "808000"),
    ("green", "00FF00"),
    ("lightGray", "C0C0C0"),
    ("magenta", "FF00FF"),
    ("red", "FF0000"),
    ("white", "FFFFFF"),
    ("yellow", "FFFF00"),
];
/// Fields whose result Word never shows.
const NEVER_SHOWN: [&str; 7] = ["ASK", "SET", "TC", "XE", "TA", "RD", "PRIVATE"];
/// Fields Word computes as it lays the document out or opens it, whatever result the file holds.
const LAYOUT_COMPUTED: [&str; 12] = [
    "PAGE",
    "NUMPAGES",
    "SECTIONPAGES",
    "SECTION",
    "PAGEREF",
    "STYLEREF",
    "DATE",
    "TIME",
    "AUTONUM",
    "AUTONUMLGL",
    "AUTONUMOUT",
    "LISTNUM",
];
/// Why text is hidden when it sits where Word's choice cannot be decided (an unknown namespace,
/// a markup-compatibility branch Word may take differently).
const UNKNOWN_NAMESPACE: &str = "unknown_namespace";
/// Why text is hidden when the markup around it is one Word would not read as the reader does
/// (a property written twice, a run outside a run's place, a style based on another kind).
const UNDECIDED: &str = "undecided";

/// Which story a part tells: field results in headers and footers are recomputed on every page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Story {
    Main,
    HeaderFooter,
    Notes,
}

pub(super) fn read(bytes: &[u8], limits: &DocLimits) -> Result<Extracted, DocError> {
    let package = package::open_package(bytes, limits)?;
    let mut budget = ElementBudget::new(limits);
    let root_rels = relationships(&package, "", limits, &mut budget)?
        .ok_or(DocError::Malformed("docx_rels"))?;
    if root_rels.iter().any(|rel| rel.kind.starts_with(STRICT)) {
        return Err(DocError::Unsupported("strict_ooxml"));
    }
    let office_document = format!("{RELATIONSHIP}officeDocument");
    let mut main = root_rels.iter().filter(|rel| rel.kind == office_document);
    let (Some(main), None) = (main.next(), main.next()) else {
        return Err(DocError::Malformed("docx_main_part"));
    };
    let main_name = main
        .internal_target()
        .filter(|name| package.part(name).is_some())
        .ok_or(DocError::Malformed("docx_main_part"))?;
    let rels = relationships(&package, &main_name, limits, &mut budget)?.unwrap_or_default();

    let mut warnings = Warnings::new(limits.max_blocks);
    if package.part("word/vbaProject.bin").is_some()
        || rels.iter().any(|rel| rel.kind == VBA_PROJECT)
    {
        warnings.once(ExtractWarning::ActiveContentIgnored {
            what: "vba_project",
        })?;
    }
    // The theme, styles and settings come only from the part the main part's relationship names;
    // a second relationship of the kind leaves which one Word uses undecided.
    let target = |kind: &str| -> Result<Option<&[u8]>, DocError> {
        let kind = format!("{RELATIONSHIP}{kind}");
        let mut found = rels.iter().filter(|rel| rel.kind == kind);
        match (found.next(), found.next()) {
            (None, _) => Ok(None),
            (Some(_), Some(_)) => Err(DocError::Malformed("docx_part_ambiguous")),
            (Some(rel), None) if rel.external => Ok(None),
            (Some(rel), None) => rel
                .internal_target()
                .and_then(|name| package.part(&name))
                .map(Some)
                .ok_or(DocError::Malformed("docx_part_missing")),
        }
    };
    let theme = target("theme")?
        .map(|xml| ThemeColours::parse(xml, limits, &mut budget))
        .transpose()?;
    let settings = target("settings")?
        .map(|xml| Settings::parse(xml, limits, &mut budget))
        .transpose()?
        .unwrap_or_default();
    let styles = target("styles")?
        .map(|xml| Styles::parse(xml, limits, &mut budget))
        .transpose()?
        .unwrap_or_default();
    // Headers, footers, footnotes and endnotes, in part-name order, each read once.
    let mut others = BTreeMap::new();
    for rel in &rels {
        let kind = rel.kind.strip_prefix(RELATIONSHIP).unwrap_or_default();
        let story = match kind {
            "header" | "footer" => Story::HeaderFooter,
            "footnotes" | "endnotes" => Story::Notes,
            _ => continue,
        };
        if rel.external {
            continue;
        }
        let name = rel
            .internal_target()
            .filter(|name| package.part(name).is_some())
            .ok_or(DocError::Malformed("docx_part_missing"))?;
        if !name.eq_ignore_ascii_case(&main_name) {
            let entry = others
                .entry(name.to_ascii_lowercase())
                .or_insert((name, story));
            if story == Story::HeaderFooter {
                entry.1 = story;
            }
        }
    }

    let mut doc = Doc {
        limits,
        budget,
        styles,
        theme,
        settings,
        page: Ok(vec![Rgb::WHITE]),
        page_seen: false,
        blocks: Vec::new(),
        warnings,
        last_beneath: None,
    };
    let main_xml = package.part(&main_name).unwrap_or_default();
    doc.read_part(&main_name, main_xml, Story::Main)?;
    for (name, story) in others.values() {
        doc.read_part(name, package.part(name).unwrap_or_default(), *story)?;
    }
    Ok(Extracted {
        extractor: ExtractorId::DOCX,
        blocks: doc.blocks,
        pages: Vec::new(),
        warnings: doc.warnings.list,
        workbook: None,
    })
}

struct Relationship {
    kind: String,
    target: Option<String>,
    external: bool,
    /// The directory of the part whose relationships these are, for relative targets.
    base: String,
}

impl Relationship {
    /// The package part an internal relationship points at, or `None` for an external one, a
    /// missing target or one that climbs above the package root.
    fn internal_target(&self) -> Option<String> {
        if self.external {
            return None;
        }
        let target = self.target.as_deref()?;
        let path = match target.strip_prefix('/') {
            Some(absolute) => absolute.to_owned(),
            None => format!("{}{target}", self.base),
        };
        let mut segments = Vec::new();
        for segment in path.split('/') {
            match segment {
                "" | "." => {}
                ".." => {
                    segments.pop()?;
                }
                segment => segments.push(segment),
            }
        }
        (!segments.is_empty()).then(|| segments.join("/"))
    }
}

/// Whether the reader follows a relationship of type `kind`; others are passed over unread.
fn followed(kind: &str) -> bool {
    kind == VBA_PROJECT
        || kind.starts_with(STRICT)
        || kind.strip_prefix(RELATIONSHIP).is_some_and(|kind| {
            matches!(
                kind,
                "officeDocument"
                    | "theme"
                    | "settings"
                    | "styles"
                    | "header"
                    | "footer"
                    | "footnotes"
                    | "endnotes"
            )
        })
}

/// The relationships of `part` (`""` for the package) that the reader follows, or `None` when
/// it has no rels part. More than `max_zip_entries` of them is `TooComplex`: a package has no
/// more parts to point at.
fn relationships(
    package: &Package,
    part: &str,
    limits: &DocLimits,
    budget: &mut ElementBudget,
) -> Result<Option<Vec<Relationship>>, DocError> {
    let (base, file) = match part.rsplit_once('/') {
        Some((dir, file)) => (format!("{dir}/"), file),
        None => (String::new(), part),
    };
    let Some(xml) = package.part(&format!("{base}_rels/{file}.rels")) else {
        return Ok(None);
    };
    let mut walker = Walker::new(xml, limits, budget, relationships_namespace)?;
    let mut found = Vec::new();
    let mut depth = 0u32;
    loop {
        match walker.next()? {
            Item::Start {
                start,
                namespace,
                skipped,
                hidden,
            } => {
                let name = start.local_name();
                if depth == 1
                    && !skipped
                    && !hidden
                    && namespace == Some(PACKAGE_RELATIONSHIPS)
                    && name.as_ref() == "Relationship"
                {
                    let kind = walker.attr(&start, None, "Type")?.unwrap_or_default();
                    if followed(&kind) {
                        if found.len() as u64 >= u64::from(limits.max_zip_entries) {
                            return Err(DocError::TooComplex("relationships"));
                        }
                        let mode = walker.attr(&start, None, "TargetMode")?;
                        found.push(Relationship {
                            kind,
                            target: walker.attr(&start, None, "Target")?,
                            external: mode.is_some_and(|mode| mode != "Internal"),
                            base: base.clone(),
                        });
                    }
                }
                depth += 1;
            }
            Item::End => depth -= 1,
            Item::Text { .. } => {}
            Item::Eof => return Ok(Some(found)),
        }
    }
}

/// One element or text event of a part as the reader sees it after markup compatibility:
/// `mc:AlternateContent`, its branches and elements of namespaces the reader does not
/// understand are not reported, only their effect on what is inside them.
enum Item<'i> {
    Start {
        start: BytesStart<'i>,
        /// The element's namespace URI, when it has one.
        namespace: Option<&'static str>,
        /// Inside a branch Word does not take: only paragraph starts still count.
        skipped: bool,
        /// Inside an element of an unknown namespace, or a branch whose choice Word may make
        /// differently: text here cannot be shown, and whatever is set here is undecided.
        hidden: bool,
    },
    End,
    Text {
        text: String,
        skipped: bool,
        hidden: bool,
    },
    Eof,
}

/// What an open element did to the walker's state, undone when it ends.
struct Frame {
    reported: bool,
    skip: bool,
    hide: bool,
    alternate: bool,
}

#[derive(Default)]
struct Alternate {
    chosen: bool,
    /// A choice the reader does not understand came first, so Word may take it where the reader
    /// takes another branch.
    uncertain: bool,
    /// The `mc:Fallback` was seen; nothing may follow it.
    fallback: bool,
}

struct Walker<'i> {
    reader: NsReader<&'i [u8]>,
    /// The namespaces this part's reader understands, by URI.
    understood: fn(&str) -> Option<&'static str>,
    frames: Vec<Frame>,
    alternates: Vec<Alternate>,
    skipped: u32,
    hidden: u32,
    events: u64,
    /// An `End` owed for an empty element just reported.
    pending_end: bool,
}

impl<'i> Walker<'i> {
    /// A walk over `xml` once `xml_guard::scan` has passed it, its elements taken from `budget`.
    fn new(
        xml: &'i [u8],
        limits: &DocLimits,
        budget: &mut ElementBudget,
        understood: fn(&str) -> Option<&'static str>,
    ) -> Result<Walker<'i>, DocError> {
        xml_guard::scan(xml, limits, None, budget)?;
        Ok(Walker {
            reader: xml_guard::reader(xml),
            understood,
            frames: Vec::new(),
            alternates: Vec::new(),
            skipped: 0,
            hidden: 0,
            events: 0,
            pending_end: false,
        })
    }

    fn next(&mut self) -> Result<Item<'i>, DocError> {
        if self.pending_end {
            self.pending_end = false;
            self.close();
            return Ok(Item::End);
        }
        loop {
            self.events += 1;
            if self.events.is_multiple_of(1024) {
                deadline::check()?;
            }
            let (resolved, event) = self
                .reader
                .read_resolved_event()
                .map_err(xml_guard::map_error)?;
            let compatibility = matches!(&resolved, ResolveResult::Bound(ns) if ns.0 == MC);
            let namespace = match &resolved {
                ResolveResult::Bound(ns) => (self.understood)(ns.0),
                _ => None,
            };
            match event {
                Event::Start(start) if compatibility => self.open_compatibility(&start)?,
                Event::Empty(start) if compatibility => {
                    self.open_compatibility(&start)?;
                    self.close();
                }
                Event::Start(start) => {
                    if let Some(item) = self.open(start, namespace) {
                        return Ok(item);
                    }
                }
                Event::Empty(start) => {
                    if let Some(item) = self.open(start, namespace) {
                        self.pending_end = true;
                        return Ok(item);
                    }
                    self.close();
                }
                Event::End(_) => {
                    if self.close() {
                        return Ok(Item::End);
                    }
                }
                Event::Text(text) => return Ok(self.text(text.xml10_content().into_owned())),
                Event::CData(data) => return Ok(self.text(data.xml10_content().into_owned())),
                Event::GeneralRef(reference) => {
                    let c = xml_guard::resolve_ref(&reference)?;
                    return Ok(self.text(c.to_string()));
                }
                Event::Eof => return Ok(Item::Eof),
                Event::Decl(_) | Event::PI(_) | Event::Comment(_) | Event::DocType(_) => {}
            }
        }
    }

    fn text(&self, text: String) -> Item<'i> {
        Item::Text {
            text,
            skipped: self.skipped > 0,
            hidden: self.hidden > 0,
        }
    }

    /// Whether the innermost open element is `mc:AlternateContent` itself: what opens there is
    /// outside any branch.
    fn in_alternate(&self) -> bool {
        self.frames.last().is_some_and(|frame| frame.alternate)
    }

    /// Opens a non-compatibility element. One of an unknown namespace hides what is inside it and
    /// is not reported; one directly inside `mc:AlternateContent`, outside its branches, is
    /// markup Word does not read there, and hides what is inside it too.
    fn open(&mut self, start: BytesStart<'i>, namespace: Option<&'static str>) -> Option<Item<'i>> {
        let reported = namespace.is_some();
        let hide = !reported || self.in_alternate();
        self.hidden += u32::from(hide);
        self.frames.push(Frame {
            reported,
            skip: false,
            hide,
            alternate: false,
        });
        reported.then_some(Item::Start {
            start,
            namespace,
            skipped: self.skipped > 0,
            hidden: self.hidden > 0,
        })
    }

    /// `mc:AlternateContent` and its branches: the first `mc:Choice` whose `Requires` names only
    /// understood namespaces is read, else the `mc:Fallback`, and the branches after the one read
    /// are skipped. A choice the reader does not understand is read too, as hidden, because Word
    /// may take it, and so is any branch read after it. Branches out of order are refused.
    fn open_compatibility(&mut self, start: &BytesStart) -> Result<(), DocError> {
        let name = start.local_name();
        let in_alternate = self.in_alternate();
        let mut frame = Frame {
            reported: false,
            skip: false,
            hide: false,
            alternate: false,
        };
        match (name.as_ref(), in_alternate) {
            ("AlternateContent", _) => {
                // Outside any branch of an enclosing one, it is markup Word does not read there.
                frame.hide = in_alternate;
                frame.alternate = true;
                self.alternates.push(Alternate::default());
            }
            ("Choice", true) => {
                let understood = self.requires_understood(start)?;
                let state = self
                    .alternates
                    .last_mut()
                    .expect("an open AlternateContent");
                if state.fallback {
                    return Err(DocError::Malformed("docx_alternate_content"));
                }
                if state.chosen {
                    frame.skip = true;
                } else if understood {
                    state.chosen = true;
                    frame.hide = state.uncertain;
                } else {
                    state.uncertain = true;
                    frame.hide = true;
                }
            }
            ("Fallback", true) => {
                let state = self
                    .alternates
                    .last_mut()
                    .expect("an open AlternateContent");
                if state.fallback {
                    return Err(DocError::Malformed("docx_alternate_content"));
                }
                state.fallback = true;
                if state.chosen {
                    frame.skip = true;
                } else {
                    state.chosen = true;
                    frame.hide = state.uncertain;
                }
            }
            // Any other compatibility element is markup the reader cannot place.
            _ => frame.hide = true,
        }
        self.skipped += u32::from(frame.skip);
        self.hidden += u32::from(frame.hide);
        self.frames.push(frame);
        Ok(())
    }

    fn requires_understood(&self, start: &BytesStart) -> Result<bool, DocError> {
        let Some(requires) = self.attr(start, None, "Requires")? else {
            return Ok(false);
        };
        let mut prefixes = requires.split_ascii_whitespace().peekable();
        if prefixes.peek().is_none() {
            return Ok(false);
        }
        Ok(prefixes.all(|prefix| {
            let name = format!("{prefix}:x");
            !prefix.contains(':')
                && match self.reader.resolver().resolve_element(QName(&name)).0 {
                    ResolveResult::Bound(ns) => ns.0 == MC || (self.understood)(ns.0).is_some(),
                    _ => false,
                }
        }))
    }

    /// Ends the innermost open element; true when it was reported.
    fn close(&mut self) -> bool {
        let Some(frame) = self.frames.pop() else {
            return false;
        };
        self.skipped -= u32::from(frame.skip);
        self.hidden -= u32::from(frame.hide);
        if frame.alternate {
            self.alternates.pop();
        }
        frame.reported
    }

    /// The value of `start`'s attribute `local` in `namespace` (`None`: unprefixed), with its
    /// references resolved. It reads the scope of the event just returned.
    fn attr(
        &self,
        start: &BytesStart,
        namespace: Option<&str>,
        local: &str,
    ) -> Result<Option<String>, DocError> {
        for attribute in start.attributes() {
            let attribute = attribute.map_err(|_| DocError::Malformed("xml_attribute"))?;
            let (resolved, name) = self.reader.resolver().resolve_attribute(attribute.key);
            if name.as_ref() != local {
                continue;
            }
            let matches = match (namespace, resolved) {
                (None, ResolveResult::Unbound) => true,
                (Some(uri), ResolveResult::Bound(found)) => found.0 == uri,
                _ => false,
            };
            if matches {
                return Ok(Some(
                    xml_guard::attribute_value(&attribute.value)?.into_owned(),
                ));
            }
        }
        Ok(None)
    }

    fn w(&self, start: &BytesStart, local: &str) -> Result<Option<String>, DocError> {
        self.attr(start, Some(W), local)
    }
}

/// A WordprocessingML part's namespaces: WordprocessingML itself and `UNDERSTOOD`.
fn document_namespace(uri: &str) -> Option<&'static str> {
    if uri == W {
        return Some(W);
    }
    UNDERSTOOD.iter().copied().find(|known| *known == uri)
}

fn relationships_namespace(uri: &str) -> Option<&'static str> {
    (uri == PACKAGE_RELATIONSHIPS).then_some(PACKAGE_RELATIONSHIPS)
}

/// A boolean property: present with no `w:val`, or `1`, `true` or `on`, is on; `0`, `false` or
/// `off` is off; anything else does not parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Toggle {
    On,
    Off,
    Bad,
}

impl Toggle {
    fn parse(value: Option<&str>) -> Toggle {
        match value {
            None | Some("1" | "true" | "on") => Toggle::On,
            Some("0" | "false" | "off") => Toggle::Off,
            Some(_) => Toggle::Bad,
        }
    }
}

/// A font size in half-points, or one that does not parse.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Size {
    HalfPoints(f64),
    Bad,
}

impl Size {
    /// `ST_HpsMeasure`: a whole number of half-points, or a decimal with a unit.
    fn parse(value: Option<&str>) -> Size {
        let Some(value) = value else {
            return Size::Bad;
        };
        if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
            return value.parse::<f64>().map_or(Size::Bad, Size::HalfPoints);
        }
        match points(value, false) {
            Some(points) if points >= 0.0 => Size::HalfPoints(points * 2.0),
            _ => Size::Bad,
        }
    }
}

/// A decimal with a unit (`mm`, `cm`, `in`, `pt`, `pc` or `pi`, and with `css` VML's `px`), in
/// points.
fn points(value: &str, css: bool) -> Option<f64> {
    let units = [
        ("mm", 72.0 / 25.4),
        ("cm", 72.0 / 2.54),
        ("in", 72.0),
        ("pt", 1.0),
        ("pc", 12.0),
        ("pi", 12.0),
        ("px", 0.75),
    ];
    let (negative, unsigned) = match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value),
    };
    for (unit, points) in units {
        if unit == "px" && !css {
            continue;
        }
        if let Some(number) = unsigned.strip_suffix(unit)
            && !number.is_empty()
            && number
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'.')
            && number.bytes().filter(|&byte| byte == b'.').count() <= 1
            && let Ok(number) = number.parse::<f64>()
        {
            let number = number * points;
            return Some(if negative { -number } else { number });
        }
    }
    None
}

/// A colour attribute: `auto`, an RGB value, or one that does not parse.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Hex {
    Auto,
    Rgb(Rgb),
    Bad,
}

impl Hex {
    fn parse(value: &str) -> Hex {
        if value == "auto" {
            Hex::Auto
        } else {
            colour::parse_hex(value).map_or(Hex::Bad, Hex::Rgb)
        }
    }
}

/// The twelve theme colour names `w:clrSchemeMapping` maps, in order: each with its mapping
/// attribute, its `w:themeColor` name, its DrawingML `a:schemeClr` name, and the slot it names
/// when the mapping is the identity.
const MAPPED: [(&str, &str, &str, SchemeSlot); 12] = [
    ("bg1", "background1", "bg1", SchemeSlot::Lt1),
    ("t1", "text1", "tx1", SchemeSlot::Dk1),
    ("bg2", "background2", "bg2", SchemeSlot::Lt2),
    ("t2", "text2", "tx2", SchemeSlot::Dk2),
    ("accent1", "accent1", "accent1", SchemeSlot::Accent1),
    ("accent2", "accent2", "accent2", SchemeSlot::Accent2),
    ("accent3", "accent3", "accent3", SchemeSlot::Accent3),
    ("accent4", "accent4", "accent4", SchemeSlot::Accent4),
    ("accent5", "accent5", "accent5", SchemeSlot::Accent5),
    ("accent6", "accent6", "accent6", SchemeSlot::Accent6),
    ("hyperlink", "hyperlink", "hlink", SchemeSlot::Hlink),
    (
        "followedHyperlink",
        "followedHyperlink",
        "folHlink",
        SchemeSlot::FolHlink,
    ),
];

/// A theme colour reference: a scheme slot named directly (`dark1` ... `light2`), one of the
/// twelve names `w:clrSchemeMapping` maps (by index into `MAPPED`), or one that does not parse.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ThemeRef {
    Slot(SchemeSlot),
    Mapped(u8),
    Bad,
}

impl ThemeRef {
    /// A `w:themeColor` value; `None` for `none`, which names no theme colour.
    fn parse(value: &str) -> Option<ThemeRef> {
        if value == "none" {
            return None;
        }
        Some(Self::named(value, false))
    }

    /// An `a:schemeClr` value (`phClr` and anything else unknown do not parse).
    fn drawing(value: &str) -> ThemeRef {
        Self::named(value, true)
    }

    fn named(value: &str, drawing: bool) -> ThemeRef {
        let mapped = MAPPED.iter().position(|(_, name, drawing_name, _)| {
            (if drawing { *drawing_name } else { *name }) == value
        });
        if let Some(at) = mapped {
            return ThemeRef::Mapped(at as u8);
        }
        let direct = match (value, drawing) {
            ("dark1", false) | ("dk1", true) => SchemeSlot::Dk1,
            ("light1", false) | ("lt1", true) => SchemeSlot::Lt1,
            ("dark2", false) | ("dk2", true) => SchemeSlot::Dk2,
            ("light2", false) | ("lt2", true) => SchemeSlot::Lt2,
            _ => return ThemeRef::Bad,
        };
        ThemeRef::Slot(direct)
    }
}

/// `ST_ColorSchemeIndex`: the values `w:clrSchemeMapping` maps a name to.
fn scheme_index(name: &str) -> Option<SchemeSlot> {
    Some(match name {
        "dark1" => SchemeSlot::Dk1,
        "light1" => SchemeSlot::Lt1,
        "dark2" => SchemeSlot::Dk2,
        "light2" => SchemeSlot::Lt2,
        "accent1" => SchemeSlot::Accent1,
        "accent2" => SchemeSlot::Accent2,
        "accent3" => SchemeSlot::Accent3,
        "accent4" => SchemeSlot::Accent4,
        "accent5" => SchemeSlot::Accent5,
        "accent6" => SchemeSlot::Accent6,
        "hyperlink" => SchemeSlot::Hlink,
        "followedHyperlink" => SchemeSlot::FolHlink,
        _ => return None,
    })
}

/// One colour of an element: its RGB value and theme attributes. A missing tint or shade is
/// `None`; one that does not parse is `Some(None)`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct ColourSpec {
    value: Option<Hex>,
    theme: Option<ThemeRef>,
    tint: Option<Option<u8>>,
    shade: Option<Option<u8>>,
}

impl ColourSpec {
    /// Reads the attributes named `[value, theme, tint, shade]`.
    fn read(walker: &Walker, start: &BytesStart, names: [&str; 4]) -> Result<ColourSpec, DocError> {
        let byte = |value: String| {
            (value.len() == 2)
                .then(|| u8::from_str_radix(&value, 16).ok())
                .flatten()
        };
        Ok(ColourSpec {
            value: walker.w(start, names[0])?.map(|value| Hex::parse(&value)),
            theme: walker
                .w(start, names[1])?
                .and_then(|value| ThemeRef::parse(&value)),
            tint: walker.w(start, names[2])?.map(byte),
            shade: walker.w(start, names[3])?.map(byte),
        })
    }

    fn rgb(colour: Rgb) -> ColourSpec {
        ColourSpec {
            value: Some(Hex::Rgb(colour)),
            ..ColourSpec::default()
        }
    }
}

/// A `w:shd` pattern.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Pattern {
    Clear,
    Nil,
    Solid,
    Percent(u8),
    /// Stripes, crosses, or a value that is missing or unknown: both of its colours show.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Shading {
    pattern: Pattern,
    colour: ColourSpec,
    fill: ColourSpec,
}

impl Shading {
    fn read(walker: &Walker, start: &BytesStart) -> Result<Shading, DocError> {
        let pattern = match walker.w(start, "val")?.as_deref() {
            Some("clear") => Pattern::Clear,
            Some("nil") => Pattern::Nil,
            Some("solid") => Pattern::Solid,
            Some(value) => value
                .strip_prefix("pct")
                .filter(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|digits| digits.parse::<u8>().ok())
                .filter(|&percent| percent <= 100)
                .map_or(Pattern::Other, Pattern::Percent),
            None => Pattern::Other,
        };
        Ok(Shading {
            pattern,
            colour: ColourSpec::read(
                walker,
                start,
                ["color", "themeColor", "themeTint", "themeShade"],
            )?,
            fill: ColourSpec::read(
                walker,
                start,
                ["fill", "themeFill", "themeFillTint", "themeFillShade"],
            )?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Highlight {
    None,
    Rgb(Rgb),
    Bad,
}

impl Highlight {
    fn parse(value: Option<&str>) -> Highlight {
        match value {
            Some("none") => Highlight::None,
            Some(name) => HIGHLIGHTS
                .iter()
                .find(|(known, _)| *known == name)
                .and_then(|(_, hex)| colour::parse_hex(hex))
                .map_or(Highlight::Bad, Highlight::Rgb),
            None => Highlight::Bad,
        }
    }
}

/// Sets `bit` in `seen`; true when it was set already (a property written twice in one place,
/// which Word's schema does not allow, so which one Word takes is not known).
fn repeated(seen: &mut u32, bit: Option<u32>) -> bool {
    let Some(bit) = bit else {
        return false;
    };
    let mask = 1 << bit;
    let again = *seen & mask != 0;
    *seen |= mask;
    again
}

/// Run properties as one level of the cascade sets them. `undecided`: why a property sits where
/// what Word applies cannot be decided (an unknown namespace, an uncertain branch, a property
/// written twice).
#[derive(Debug, Clone, Default)]
struct RunProps {
    style: Option<String>,
    vanish: Option<Toggle>,
    spec_vanish: Option<Toggle>,
    web_hidden: Option<Toggle>,
    cs: Option<Toggle>,
    rtl: Option<Toggle>,
    size: Option<Size>,
    size_cs: Option<Size>,
    colour: Option<ColourSpec>,
    highlight: Option<Highlight>,
    shading: Option<Shading>,
    undecided: Option<&'static str>,
}

impl RunProps {
    fn overlay(&mut self, later: &RunProps) {
        macro_rules! later_wins {
            ($($field:ident),+) => {$(
                if later.$field.is_some() {
                    self.$field = later.$field.clone();
                }
            )+};
        }
        later_wins!(
            style,
            vanish,
            spec_vanish,
            web_hidden,
            cs,
            rtl,
            size,
            size_cs,
            colour,
            highlight,
            shading
        );
        self.undecided = self.undecided.or(later.undecided);
    }

    fn bit(local: &str) -> Option<u32> {
        [
            "rStyle",
            "vanish",
            "specVanish",
            "webHidden",
            "cs",
            "rtl",
            "sz",
            "szCs",
            "color",
            "highlight",
            "shd",
        ]
        .iter()
        .position(|name| *name == local)
        .map(|at| at as u32)
    }

    /// Reads property `local`; one already set in this element leaves the run undecided.
    fn read(
        &mut self,
        walker: &Walker,
        start: &BytesStart,
        local: &str,
        seen: &mut u32,
    ) -> Result<(), DocError> {
        if repeated(seen, Self::bit(local)) {
            self.undecided = self.undecided.or(Some(UNDECIDED));
        }
        let val = || walker.w(start, "val");
        match local {
            "rStyle" => self.style = val()?,
            "vanish" => self.vanish = Some(Toggle::parse(val()?.as_deref())),
            "specVanish" => self.spec_vanish = Some(Toggle::parse(val()?.as_deref())),
            "webHidden" => self.web_hidden = Some(Toggle::parse(val()?.as_deref())),
            "cs" => self.cs = Some(Toggle::parse(val()?.as_deref())),
            "rtl" => self.rtl = Some(Toggle::parse(val()?.as_deref())),
            "sz" => self.size = Some(Size::parse(val()?.as_deref())),
            "szCs" => self.size_cs = Some(Size::parse(val()?.as_deref())),
            "color" => {
                self.colour = Some(ColourSpec::read(
                    walker,
                    start,
                    ["val", "themeColor", "themeTint", "themeShade"],
                )?)
            }
            "highlight" => self.highlight = Some(Highlight::parse(val()?.as_deref())),
            "shd" => self.shading = Some(Shading::read(walker, start)?),
            _ => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
struct ParaProps {
    style: Option<String>,
    shading: Option<Shading>,
    undecided: Option<&'static str>,
}

impl ParaProps {
    fn overlay(&mut self, later: &ParaProps) {
        if later.style.is_some() {
            self.style = later.style.clone();
        }
        if later.shading.is_some() {
            self.shading = later.shading;
        }
        self.undecided = self.undecided.or(later.undecided);
    }

    fn read(
        &mut self,
        walker: &Walker,
        start: &BytesStart,
        local: &str,
        seen: &mut u32,
    ) -> Result<(), DocError> {
        let bit = ["pStyle", "shd"].iter().position(|name| *name == local);
        if repeated(seen, bit.map(|at| at as u32)) {
            self.undecided = self.undecided.or(Some(UNDECIDED));
        }
        match local {
            "pStyle" => self.style = walker.w(start, "val")?,
            "shd" => self.shading = Some(Shading::read(walker, start)?),
            _ => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
struct CellProps {
    shading: Option<Shading>,
    /// A `w:vMerge` or `w:hMerge` continuation: Word lays out none of the cell's paragraphs.
    merged: bool,
    undecided: Option<&'static str>,
}

impl CellProps {
    fn overlay(&mut self, later: &CellProps) {
        if later.shading.is_some() {
            self.shading = later.shading;
        }
        self.undecided = self.undecided.or(later.undecided);
    }

    fn read(
        &mut self,
        walker: &Walker,
        start: &BytesStart,
        local: &str,
        seen: &mut u32,
    ) -> Result<(), DocError> {
        let bit = ["shd", "vMerge", "hMerge", "cellDel"]
            .iter()
            .position(|name| *name == local);
        if repeated(seen, bit.map(|at| at as u32)) {
            self.undecided = self.undecided.or(Some(UNDECIDED));
        }
        match local {
            "shd" => self.shading = Some(Shading::read(walker, start)?),
            "vMerge" | "hMerge" => match walker.w(start, "val")?.as_deref() {
                None | Some("continue") => self.merged = true,
                Some("restart") => {}
                Some(_) => self.undecided = self.undecided.or(Some(UNDECIDED)),
            },
            _ => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
struct Props {
    run: RunProps,
    para: ParaProps,
    cell: CellProps,
}

impl Props {
    fn overlay(&mut self, later: &Props) {
        self.run.overlay(&later.run);
        self.para.overlay(&later.para);
        self.cell.overlay(&later.cell);
    }

    fn undecided(&mut self, why: &'static str) {
        self.run.undecided = self.run.undecided.or(Some(why));
        self.para.undecided = self.para.undecided.or(Some(why));
        self.cell.undecided = self.cell.undecided.or(Some(why));
    }
}

/// The regions a table's `w:tblLook` turns on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Look {
    first_row: bool,
    last_row: bool,
    first_column: bool,
    last_column: bool,
    no_row_bands: bool,
    no_column_bands: bool,
}

impl Look {
    fn from_bits(bits: u16) -> Look {
        Look {
            first_row: bits & 0x0020 != 0,
            last_row: bits & 0x0040 != 0,
            first_column: bits & 0x0080 != 0,
            last_column: bits & 0x0100 != 0,
            no_row_bands: bits & 0x0200 != 0,
            no_column_bands: bits & 0x0400 != 0,
        }
    }

    /// The attributes when any is present, else the older hexadecimal `w:val`.
    fn read(walker: &Walker, start: &BytesStart) -> Result<Look, DocError> {
        let names = [
            "firstRow",
            "lastRow",
            "firstColumn",
            "lastColumn",
            "noHBand",
            "noVBand",
        ];
        let mut values = Vec::with_capacity(names.len());
        for name in names {
            values.push(walker.w(start, name)?);
        }
        if values.iter().any(Option::is_some) {
            let mut flags = [false; 6];
            for (flag, value) in flags.iter_mut().zip(values) {
                *flag = match value.as_deref().map(|value| Toggle::parse(Some(value))) {
                    None | Some(Toggle::Off) => false,
                    Some(Toggle::On) => true,
                    Some(Toggle::Bad) => return Err(DocError::Malformed("docx_table")),
                };
            }
            let [
                first_row,
                last_row,
                first_column,
                last_column,
                no_row_bands,
                no_column_bands,
            ] = flags;
            return Ok(Look {
                first_row,
                last_row,
                first_column,
                last_column,
                no_row_bands,
                no_column_bands,
            });
        }
        match walker.w(start, "val")? {
            None => Ok(Look::default()),
            Some(value) => u16::from_str_radix(&value, 16)
                .map(Look::from_bits)
                .map_err(|_| DocError::Malformed("docx_table")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum StyleKind {
    Paragraph,
    Character,
    Table,
    Numbering,
}

/// A table style's conditional parts (`w:tblStylePr w:type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Region {
    WholeTable,
    Band1Horz,
    Band2Horz,
    Band1Vert,
    Band2Vert,
    FirstCol,
    LastCol,
    FirstRow,
    LastRow,
    NwCell,
    NeCell,
    SwCell,
    SeCell,
}

impl Region {
    fn parse(value: &str) -> Option<Region> {
        Some(match value {
            "wholeTable" => Region::WholeTable,
            "band1Horz" => Region::Band1Horz,
            "band2Horz" => Region::Band2Horz,
            "band1Vert" => Region::Band1Vert,
            "band2Vert" => Region::Band2Vert,
            "firstCol" => Region::FirstCol,
            "lastCol" => Region::LastCol,
            "firstRow" => Region::FirstRow,
            "lastRow" => Region::LastRow,
            "nwCell" => Region::NwCell,
            "neCell" => Region::NeCell,
            "swCell" => Region::SwCell,
            "seCell" => Region::SeCell,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
struct Style {
    kind: StyleKind,
    based_on: Option<String>,
    props: Props,
    regions: Vec<(Region, Props)>,
    row_band: Option<u32>,
    column_band: Option<u32>,
}

/// A style with its `w:basedOn` chain folded in, outermost first.
#[derive(Debug, Default)]
struct Merged {
    props: Props,
    regions: HashMap<Region, Props>,
    row_band: u32,
    column_band: u32,
}

#[derive(Default)]
struct Styles {
    defaults: Props,
    styles: HashMap<String, Box<Style>>,
    defaults_by_kind: HashMap<StyleKind, String>,
    merged: RefCell<HashMap<(StyleKind, String), Rc<Merged>>>,
}

impl Styles {
    fn parse(
        xml: &[u8],
        limits: &DocLimits,
        budget: &mut ElementBudget,
    ) -> Result<Styles, DocError> {
        #[derive(Clone, Copy, PartialEq)]
        enum Into {
            Defaults,
            Style,
            Region,
        }
        #[derive(Clone, Copy, PartialEq)]
        enum Node {
            Root,
            DocDefaults,
            RunDefault,
            ParaDefault,
            Style,
            Region,
            Run(Into),
            Para(Into),
            Cell(Into),
            TableProps,
            Other,
        }
        let mut styles = Styles::default();
        let mut walker = Walker::new(xml, limits, budget, document_namespace)?;
        let mut nodes: Vec<Node> = Vec::new();
        // The style being read (its id and whether it may be a default), and its region.
        let mut style: Option<(String, Style, bool)> = None;
        let mut region: Option<(Region, Props)> = None;
        // The children seen so far of the defaults, the style, the region and the property
        // element being read: one written twice is undecided.
        let (mut defaults_seen, mut style_seen, mut region_seen, mut props_seen) =
            (0u32, 0u32, 0u32, 0u32);
        loop {
            match walker.next()? {
                Item::Start {
                    start,
                    namespace,
                    skipped,
                    hidden,
                } => {
                    let name = start.local_name();
                    let local = name.as_ref();
                    let parent = nodes.last().copied();
                    let node = if skipped || namespace != Some(W) {
                        Node::Other
                    } else {
                        match (parent, local) {
                            (None, "styles") => Node::Root,
                            (Some(Node::Root), "docDefaults") => Node::DocDefaults,
                            (Some(Node::DocDefaults), "rPrDefault" | "pPrDefault") => {
                                let bit = u32::from(local == "pPrDefault");
                                if repeated(&mut defaults_seen, Some(bit)) {
                                    styles.defaults.undecided(UNDECIDED);
                                }
                                if local == "rPrDefault" {
                                    Node::RunDefault
                                } else {
                                    Node::ParaDefault
                                }
                            }
                            (Some(Node::RunDefault), "rPr") => {
                                props_seen = 0;
                                Node::Run(Into::Defaults)
                            }
                            (Some(Node::ParaDefault), "pPr") => {
                                props_seen = 0;
                                Node::Para(Into::Defaults)
                            }
                            (Some(Node::Root), "style") => {
                                let kind = match walker.w(&start, "type")?.as_deref() {
                                    None | Some("paragraph") => Some(StyleKind::Paragraph),
                                    Some("character") => Some(StyleKind::Character),
                                    Some("table") => Some(StyleKind::Table),
                                    Some("numbering") => Some(StyleKind::Numbering),
                                    Some(_) => None,
                                };
                                let id = walker.w(&start, "styleId")?;
                                let default = walker
                                    .w(&start, "default")?
                                    .is_some_and(|value| Toggle::parse(Some(&value)) == Toggle::On);
                                style_seen = 0;
                                style = kind.zip(id).map(|(kind, id)| {
                                    let mut props = Props::default();
                                    if hidden {
                                        props.undecided(UNKNOWN_NAMESPACE);
                                    }
                                    let style = Style {
                                        kind,
                                        based_on: None,
                                        props,
                                        regions: Vec::new(),
                                        row_band: None,
                                        column_band: None,
                                    };
                                    (id, style, default)
                                });
                                Node::Style
                            }
                            (Some(Node::Style), "basedOn") => {
                                if let Some((_, style, _)) = &mut style {
                                    // A base Word may not apply is never applied, never dropped:
                                    // the style is undecided.
                                    if hidden {
                                        style.props.undecided(UNKNOWN_NAMESPACE);
                                    } else if repeated(&mut style_seen, Some(0)) {
                                        style.props.undecided(UNDECIDED);
                                    } else {
                                        style.based_on = walker.w(&start, "val")?;
                                    }
                                }
                                Node::Other
                            }
                            (Some(Node::Style), "tblStylePr") => {
                                region = walker
                                    .w(&start, "type")?
                                    .and_then(|value| Region::parse(&value))
                                    .map(|found| (found, Props::default()));
                                region_seen = 0;
                                Node::Region
                            }
                            (Some(Node::Style), "rPr" | "pPr" | "tcPr" | "tblPr") => {
                                let bit = ["rPr", "pPr", "tcPr", "tblPr"]
                                    .iter()
                                    .position(|name| *name == local)
                                    .map(|at| at as u32 + 1);
                                if repeated(&mut style_seen, bit)
                                    && let Some((_, style, _)) = &mut style
                                {
                                    style.props.undecided(UNDECIDED);
                                }
                                props_seen = 0;
                                match local {
                                    "rPr" => Node::Run(Into::Style),
                                    "pPr" => Node::Para(Into::Style),
                                    "tcPr" => Node::Cell(Into::Style),
                                    _ => Node::TableProps,
                                }
                            }
                            (Some(Node::Region), "rPr" | "pPr" | "tcPr") => {
                                let bit = ["rPr", "pPr", "tcPr"]
                                    .iter()
                                    .position(|name| *name == local)
                                    .map(|at| at as u32);
                                if repeated(&mut region_seen, bit)
                                    && let Some((_, props)) = &mut region
                                {
                                    props.undecided(UNDECIDED);
                                }
                                props_seen = 0;
                                match local {
                                    "rPr" => Node::Run(Into::Region),
                                    "pPr" => Node::Para(Into::Region),
                                    _ => Node::Cell(Into::Region),
                                }
                            }
                            (Some(Node::Run(into) | Node::Para(into) | Node::Cell(into)), _) => {
                                let props = match into {
                                    Into::Defaults => Some(&mut styles.defaults),
                                    Into::Style => {
                                        style.as_mut().map(|(_, style, _)| &mut style.props)
                                    }
                                    Into::Region => region.as_mut().map(|(_, props)| props),
                                };
                                if let Some(props) = props {
                                    if hidden {
                                        props.undecided(UNKNOWN_NAMESPACE);
                                    }
                                    match parent {
                                        Some(Node::Run(_)) => props.run.read(
                                            &walker,
                                            &start,
                                            local,
                                            &mut props_seen,
                                        )?,
                                        Some(Node::Para(_)) => props.para.read(
                                            &walker,
                                            &start,
                                            local,
                                            &mut props_seen,
                                        )?,
                                        _ => props.cell.read(
                                            &walker,
                                            &start,
                                            local,
                                            &mut props_seen,
                                        )?,
                                    }
                                }
                                Node::Other
                            }
                            (
                                Some(Node::TableProps),
                                "tblStyleRowBandSize" | "tblStyleColBandSize",
                            ) => {
                                let size = walker
                                    .w(&start, "val")?
                                    .map(|value| value.parse::<u32>())
                                    .transpose()
                                    .map_err(|_| DocError::Malformed("docx_table"))?;
                                let row = local == "tblStyleRowBandSize";
                                let again = repeated(&mut props_seen, Some(u32::from(row)));
                                if let Some((_, style, _)) = &mut style {
                                    if hidden {
                                        style.props.undecided(UNKNOWN_NAMESPACE);
                                    } else if again {
                                        style.props.undecided(UNDECIDED);
                                    } else if row {
                                        style.row_band = size;
                                    } else {
                                        style.column_band = size;
                                    }
                                }
                                Node::Other
                            }
                            _ => Node::Other,
                        }
                    };
                    // A property container inside an unknown element or an uncertain branch.
                    if hidden && matches!(node, Node::Run(_) | Node::Para(_) | Node::Cell(_)) {
                        match node {
                            Node::Run(Into::Defaults) | Node::Para(Into::Defaults) => {
                                styles.defaults.undecided(UNKNOWN_NAMESPACE)
                            }
                            Node::Run(Into::Style)
                            | Node::Para(Into::Style)
                            | Node::Cell(Into::Style) => {
                                if let Some((_, style, _)) = &mut style {
                                    style.props.undecided(UNKNOWN_NAMESPACE);
                                }
                            }
                            _ => {
                                if let Some((_, props)) = &mut region {
                                    props.undecided(UNKNOWN_NAMESPACE);
                                }
                            }
                        }
                    }
                    nodes.push(node);
                }
                Item::End => match nodes.pop() {
                    Some(Node::Style) => {
                        if let Some((id, mut style, default)) = style.take() {
                            // Two styles with one id: which one Word uses is not known.
                            if styles.styles.contains_key(&id) {
                                style.props.undecided(UNDECIDED);
                            } else if styles.styles.len() >= MAX_STYLES {
                                return Err(DocError::TooComplex("styles"));
                            }
                            if default {
                                styles.defaults_by_kind.insert(style.kind, id.clone());
                            }
                            styles.styles.insert(id, Box::new(style));
                        }
                    }
                    Some(Node::Region) => {
                        if let (Some((found, props)), Some((_, style, _))) =
                            (region.take(), &mut style)
                        {
                            // A region written twice in one style: which Word uses is not known.
                            if style.regions.iter().any(|(known, _)| *known == found) {
                                style.props.undecided(UNDECIDED);
                            } else {
                                style.regions.push((found, props));
                            }
                        }
                    }
                    _ => {}
                },
                Item::Text { .. } => {}
                Item::Eof => return Ok(styles),
            }
        }
    }

    /// The style `id` names when it is one of `kind`, else the default style of `kind`.
    fn resolve<'a>(&'a self, id: Option<&'a str>, kind: StyleKind) -> Option<&'a str> {
        id.filter(|id| self.styles.get(*id).is_some_and(|style| style.kind == kind))
            .or_else(|| self.defaults_by_kind.get(&kind).map(String::as_str))
    }

    /// `id`'s properties with its `w:basedOn` chain folded in, outermost first. A chain stops at
    /// a cycle or a missing style; one that reaches a style of another kind is undecided, and
    /// one longer than 16 styles is `TooComplex`.
    fn merged(&self, id: &str, kind: StyleKind) -> Result<Rc<Merged>, DocError> {
        let key = (kind, id.to_owned());
        if let Some(found) = self.merged.borrow().get(&key) {
            return Ok(found.clone());
        }
        let mut chain = Vec::new();
        let mut seen = HashSet::new();
        let mut next = Some(id);
        let mut crossed = false;
        while let Some(id) = next {
            let Some(style) = self.styles.get(id) else {
                break;
            };
            if style.kind != kind {
                crossed = true;
                break;
            }
            if !seen.insert(id) {
                break;
            }
            if chain.len() == MAX_STYLE_CHAIN {
                return Err(DocError::TooComplex("style_chain"));
            }
            chain.push(style);
            next = style.based_on.as_deref();
        }
        let mut merged = Merged {
            row_band: 1,
            column_band: 1,
            ..Merged::default()
        };
        for style in chain.iter().rev() {
            merged.props.overlay(&style.props);
            for (region, props) in &style.regions {
                merged.regions.entry(*region).or_default().overlay(props);
            }
            merged.row_band = style.row_band.unwrap_or(merged.row_band).max(1);
            merged.column_band = style.column_band.unwrap_or(merged.column_band).max(1);
        }
        if crossed {
            merged.props.undecided(UNDECIDED);
        }
        let merged = Rc::new(merged);
        self.merged.borrow_mut().insert(key, merged.clone());
        Ok(merged)
    }
}

/// A `w:clrSchemeMapping` entry: the slot a mapped theme colour names, or one that does not
/// parse.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Mapping {
    Slot(SchemeSlot),
    Bad,
}

struct Settings {
    /// `w:displayBackgroundShape`: Word's print layout shows the page colour.
    display_background: bool,
    /// The slot each of the twelve names in `MAPPED` stands for.
    mapping: [Mapping; 12],
    /// `w:updateFields`: Word updates every field when the document opens.
    update_fields: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            display_background: false,
            mapping: MAPPED.map(|(_, _, _, slot)| Mapping::Slot(slot)),
            update_fields: false,
        }
    }
}

impl Settings {
    fn parse(
        xml: &[u8],
        limits: &DocLimits,
        budget: &mut ElementBudget,
    ) -> Result<Settings, DocError> {
        let mut settings = Settings::default();
        let mut walker = Walker::new(xml, limits, budget, document_namespace)?;
        let (mut depth, mut seen) = (0u32, 0u32);
        loop {
            match walker.next()? {
                Item::Start {
                    start,
                    namespace,
                    skipped,
                    hidden,
                } => {
                    let name = start.local_name();
                    if depth == 1 && !skipped && namespace == Some(W) {
                        let local = name.as_ref();
                        let bit = ["displayBackgroundShape", "clrSchemeMapping", "updateFields"]
                            .iter()
                            .position(|known| *known == local);
                        // Written twice, or where Word may not read it: undecided.
                        let undecided = repeated(&mut seen, bit.map(|at| at as u32)) || hidden;
                        match local {
                            // Where it is undecided, both page colours count, as when it is off.
                            "displayBackgroundShape" => {
                                settings.display_background = !undecided
                                    && Toggle::parse(walker.w(&start, "val")?.as_deref())
                                        == Toggle::On;
                            }
                            "clrSchemeMapping" => {
                                for (at, (attribute, ..)) in MAPPED.iter().enumerate() {
                                    if let Some(value) = walker.w(&start, attribute)? {
                                        settings.mapping[at] = match scheme_index(&value) {
                                            Some(slot) if !undecided => Mapping::Slot(slot),
                                            _ => Mapping::Bad,
                                        };
                                    } else if undecided {
                                        settings.mapping[at] = Mapping::Bad;
                                    }
                                }
                            }
                            "updateFields" => {
                                settings.update_fields = undecided
                                    || Toggle::parse(walker.w(&start, "val")?.as_deref())
                                        != Toggle::Off;
                            }
                            _ => {}
                        }
                    }
                    depth += 1;
                }
                Item::End => depth -= 1,
                Item::Text { .. } => {}
                Item::Eof => return Ok(settings),
            }
        }
    }
}

/// Warnings, at most `max_blocks` of them; `ActiveContentIgnored` and `EmbeddedContentIgnored`
/// once per kind.
struct Warnings {
    list: Vec<ExtractWarning>,
    seen: HashSet<&'static str>,
    cap: u32,
}

impl Warnings {
    fn new(cap: u32) -> Warnings {
        Warnings {
            list: Vec::new(),
            seen: HashSet::new(),
            cap,
        }
    }

    fn push(&mut self, warning: ExtractWarning) -> Result<(), DocError> {
        if self.list.len() as u64 >= u64::from(self.cap) {
            return Err(DocError::TooComplex("warnings"));
        }
        self.list.push(warning);
        Ok(())
    }

    fn once(&mut self, warning: ExtractWarning) -> Result<(), DocError> {
        let what = match warning {
            ExtractWarning::ActiveContentIgnored { what }
            | ExtractWarning::EmbeddedContentIgnored { what } => what,
            _ => return self.push(warning),
        };
        if self.seen.insert(what) {
            self.push(warning)?;
        }
        Ok(())
    }
}

/// What a run looks like from where the reader stands: automatic, a set of candidate colours
/// (any of which Word may draw), or a value that does not parse.
enum Paint {
    Auto,
    Colours(Vec<Rgb>),
    Bad,
}

/// One level of what lies beneath a run, nearest first.
#[derive(Clone)]
enum Layer {
    Highlight(Highlight),
    Shading(Shading),
    /// A text box's own fill: the colours it may be, or `Err` when it does not parse.
    Fill(Result<Vec<Rgb>, ()>),
}

/// A paragraph being read: its runs are judged when it ends, or, inside a table, when the
/// outermost table ends and every enclosing table's shape is known.
struct Para {
    ordinal: u32,
    para_id: Option<String>,
    props: ParaProps,
    runs: Vec<Run>,
    run: Option<Box<Run>>,
    /// What lies beneath the paragraph, innermost first, shared by the paragraphs of a cell or
    /// a text box.
    under: Rc<[Under]>,
    /// Something Word draws was left out after the last kept run: the next starts after a gap.
    gap: bool,
    /// The paragraph has children besides its `w:pPr`; a `w:pPr` now is refused.
    content: bool,
    has_props: bool,
    /// Its properties as the text boxes it holds see them, made once for all of them.
    shared: Option<Rc<ParaProps>>,
}

/// A run being read: what it shows, and what was left out of it.
#[derive(Default)]
struct Run {
    props: RunProps,
    text: String,
    /// Why text of this run was dropped as it was read (an unknown namespace, a field result
    /// Word recomputes, a data-bound control ...), the first reason.
    dropped: Option<&'static str>,
    /// Something Word draws was left out before the run's first kept text, and after its last.
    gap_before: bool,
    gap_after: bool,
    has_props: bool,
}

impl Run {
    fn push(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        if self.gap_after {
            self.text.push(GAP);
            self.gap_after = false;
        }
        if self.text.is_empty() {
            self.text = text;
        } else {
            self.text.push_str(&text);
        }
    }

    fn gap(&mut self) {
        if self.text.is_empty() {
            self.gap_before = true;
        } else {
            self.gap_after = true;
        }
    }

    fn drop_text(&mut self, why: &'static str) {
        self.gap();
        self.dropped = self.dropped.or(Some(why));
    }
}

/// A table paragraph's cell: its row and column, and the cell's own properties.
#[derive(Clone)]
struct CellAt {
    row: usize,
    column: usize,
    props: CellProps,
}

/// A table's style, look and rows, complete once the table ends.
#[derive(Default)]
struct TableShape {
    style: Option<String>,
    look: Option<Look>,
    /// Why a table property sits where Word's choice cannot be decided.
    undecided: Option<&'static str>,
    rows: Vec<RowShape>,
}

#[derive(Clone, Copy)]
struct RowShape {
    cells: usize,
    look: Option<Look>,
}

struct Table {
    shape: Rc<RefCell<TableShape>>,
    row: Option<OpenRow>,
    has_props: bool,
}

struct OpenRow {
    /// A tracked deletion of the row (`w:trPr/w:del`): its content is excluded.
    deleted: bool,
    has_props: bool,
    has_cells: bool,
    cell: Option<OpenCell>,
}

struct OpenCell {
    at: CellAt,
    /// The cell as its paragraphs see it, made when its first content starts; a `w:tcPr` after
    /// that is refused (the reader judges a cell's paragraphs before the cell ends).
    shared: Option<Rc<CellAt>>,
    /// A tracked deletion of the cell (`w:cellDel`): its content is excluded.
    deleted: bool,
    has_props: bool,
    /// What lies beneath the cell's paragraphs, made for the first of them.
    chain: Option<Rc<[Under]>>,
}

impl OpenCell {
    fn shared(&mut self) -> Rc<CellAt> {
        self.shared
            .get_or_insert_with(|| Rc::new(self.at.clone()))
            .clone()
    }
}

/// A text box as its paragraphs see it.
struct BoxInfo {
    /// Why its text cannot be shown: a hidden or off-page shape, a fill the reader does not read,
    /// a floating box with no fill over what the reader cannot see.
    hidden: Option<&'static str>,
    /// Its fill, or `None` when it has none and what lies beneath shows through.
    fill: Option<ColourSpec>,
    /// The shape's own text colour (`wps:style/a:fontRef`), a candidate besides the run's.
    text_colour: Option<ColourSpec>,
}

enum Container {
    Table(Table),
    /// A text box: what is inside it is not in the table around it, and its host is the
    /// paragraph its shape sits in.
    TextBox {
        info: Rc<BoxInfo>,
        host: Option<Rc<ParaProps>>,
        /// What lies beneath the box's paragraphs, made for the first of them.
        chain: Option<Rc<[Under]>>,
    },
}

/// One thing beneath a paragraph's text.
#[derive(Clone)]
enum Under {
    /// A table cell, with its table's shape.
    Cell(Rc<RefCell<TableShape>>, Rc<CellAt>),
    Box(Rc<BoxInfo>),
    /// The paragraph an inline text box sits in: its own properties.
    Host(Rc<ParaProps>),
    /// A place Word lays no paragraph out in (in a table, outside any cell).
    Nowhere,
}

/// A fill or text colour a shape's properties set.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ShapeFill {
    None,
    Colour(ColourSpec),
    Unknown,
}

/// A drawing frame or shape being read; a shape inside another takes its frame's position and
/// visibility.
#[derive(Clone, Default)]
struct ShapeState {
    anchored: bool,
    hidden: bool,
    /// A shape property sits where Word's choice cannot be decided.
    undecided: bool,
    /// The fill its own properties set.
    fill: Option<ShapeFill>,
    /// The fill its style reference sets (`a:fillRef`).
    style_fill: Option<ShapeFill>,
    text_colour: Option<ShapeFill>,
    /// `wp:anchor simplePos="1"`: its position is `wp:simplePos`.
    simple: bool,
    /// Horizontal and vertical offsets from where it is anchored, and its extent, in EMU.
    offset: [Option<i64>; 2],
    extent: [i64; 2],
    vml: bool,
    /// The fill a VML shape has when it sets none: its `v:shapetype`'s, else VML's own white;
    /// unknown when it names a shapetype not defined before it.
    vml_default: Option<ShapeFill>,
    /// A `v:shapetype`'s id, under which its fill is kept when it ends.
    shapetype: Option<String>,
    /// Its text box was read: a fill after it is refused.
    text_read: bool,
    /// The fill or font reference just opened, until its first colour is read.
    awaiting: Option<Target>,
}

impl ShapeState {
    /// A shape inside `outer`: the frame's position and visibility carry over.
    fn inside(outer: Option<&ShapeState>) -> ShapeState {
        match outer {
            Some(outer) => ShapeState {
                anchored: outer.anchored,
                hidden: outer.hidden,
                undecided: outer.undecided,
                offset: outer.offset,
                extent: outer.extent,
                ..ShapeState::default()
            },
            None => ShapeState::default(),
        }
    }

    /// Off the page wherever its anchor lands: an offset of a whole page or more.
    fn off_page(&self) -> bool {
        self.anchored
            && (0..2).any(|axis| {
                self.offset[axis].is_some_and(|offset| {
                    offset >= PAGE_MAX_EMU
                        || offset.saturating_add(self.extent[axis]) <= -PAGE_MAX_EMU
                })
            })
    }

    fn set_fill(&mut self, fill: ShapeFill) {
        // Two fills in one place: which Word draws is not known.
        self.fill = Some(match self.fill {
            None => fill,
            Some(_) => ShapeFill::Unknown,
        });
    }

    fn box_info(&self) -> BoxInfo {
        let mut hidden = if self.undecided {
            Some(UNKNOWN_NAMESPACE)
        } else if self.hidden {
            Some("hidden_shape")
        } else if self.off_page() {
            Some("off_page")
        } else {
            None
        };
        let fill = self
            .fill
            .or(self.style_fill)
            .or(self.vml_default)
            .unwrap_or(ShapeFill::None);
        let fill = match fill {
            ShapeFill::Colour(spec) => Some(spec),
            // A floating box with no fill lies over what the reader cannot see.
            ShapeFill::None if !self.anchored => None,
            ShapeFill::None | ShapeFill::Unknown => {
                hidden = hidden.or(Some("unknown_paint"));
                None
            }
        };
        let text_colour = match self.text_colour {
            Some(ShapeFill::Colour(spec)) => Some(spec),
            Some(ShapeFill::Unknown) => {
                hidden = hidden.or(Some("unknown_paint"));
                None
            }
            _ => None,
        };
        BoxInfo {
            hidden,
            fill,
            text_colour,
        }
    }
}

/// An open field.
struct Field {
    /// Still in its instruction (before `separate`).
    code: bool,
    /// The start of its instruction, which names its type.
    instruction: String,
    /// Why its result is not shown, once `separate` has decided it.
    hide: Option<&'static str>,
    /// `w:dirty`: Word updates it when the document opens.
    dirty: bool,
    /// A field character of it sits where Word's choice cannot be decided.
    undecided: bool,
}

/// Where an element's content sits: among runs, paragraphs, rows or cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Run,
    Block,
    Row,
    Cell,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Node {
    Document,
    Background,
    Body,
    /// A header, footer, footnote or endnote.
    Story,
    /// The root of a footnotes or endnotes part.
    Notes,
    Paragraph,
    Run,
    Text,
    InstrText,
    RunProps,
    ParaProps,
    /// A container of runs, paragraphs, rows or cells (`w:hyperlink`, `w:sdtContent` ...).
    Container(Level),
    /// The content of a content control bound to a custom XML part.
    BoundContent(Level),
    /// A content control and where it sits (`None`: nowhere content may be).
    Sdt(Option<Level>),
    SdtProps,
    /// A `w:fldSimple`, and how many fields were open before it.
    SimpleField(usize),
    Table,
    TableProps,
    Row,
    RowProps,
    RowExceptions,
    Cell,
    CellProps,
    /// `w:drawing` or `w:pict`: what it draws stands between the run's text.
    Object,
    Shape,
    ShapeProps,
    ShapeStyle,
    Position(usize),
    PosOffset(usize),
    /// An element whose colour child sets a shape's fill or text colour, and that colour.
    ColourHolder(Target),
    Colour(Target),
    TextBox,
    Excluded,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Target {
    Fill,
    Text,
}

/// Where the content of an element under `parent` sits.
fn level(parent: Option<Node>) -> Option<Level> {
    match parent? {
        Node::Paragraph | Node::SimpleField(_) => Some(Level::Run),
        Node::Body | Node::Story | Node::Cell | Node::TextBox => Some(Level::Block),
        Node::Table => Some(Level::Row),
        Node::Row => Some(Level::Cell),
        Node::Container(level) | Node::BoundContent(level) => Some(level),
        _ => None,
    }
}

/// What the open fields, controls and markup do to text read now.
enum Shown {
    Yes,
    /// Field code, which Word never shows.
    Code,
    Dropped(&'static str),
}

/// One part's walk.
struct Part<'n> {
    name: &'n str,
    story: Story,
    ordinal: u32,
    /// Inside deleted, moved-away or embedded content.
    excluded: u32,
    body_started: bool,
    paragraphs: Vec<Option<Para>>,
    containers: Vec<Container>,
    shapes: Vec<ShapeState>,
    fields: Vec<Field>,
    /// The fills of the `v:shapetype`s defined so far, by id.
    shapetypes: HashMap<String, Option<ShapeFill>>,
    /// The open content controls: whether each is bound to a custom XML part.
    sdts: Vec<bool>,
    /// Open content of bound content controls.
    bound: u32,
    /// Paragraphs of the open tables, judged once the outermost one ends. Boxed, so the queue
    /// of a large table grows by pointers rather than copies of whole paragraphs.
    #[allow(clippy::vec_box)]
    pending: Vec<Box<Para>>,
    /// The chain of a paragraph in no container.
    no_chain: Rc<[Under]>,
    /// The properties seen in the open `w:rPr`, `w:pPr`, `w:tcPr`, `w:trPr` or `w:tblPr`.
    seen: u32,
    out: Vec<(u32, String, Option<String>)>,
    hidden: Vec<(u32, &'static str, Option<String>)>,
}

impl Part<'_> {
    fn shown(&self, hidden: bool) -> Shown {
        if self
            .fields
            .iter()
            .any(|field| field.code && !field.undecided)
        {
            return Shown::Code;
        }
        if hidden || self.fields.iter().any(|field| field.undecided) {
            return Shown::Dropped(UNKNOWN_NAMESPACE);
        }
        if self.bound > 0 {
            return Shown::Dropped("data_binding");
        }
        match self.fields.iter().find_map(|field| field.hide) {
            Some(why) => Shown::Dropped(why),
            None => Shown::Yes,
        }
    }

    /// Adds a run element's text (`None`: something Word draws that is not text) to the open run,
    /// as the open fields, controls and markup allow.
    fn put(&mut self, hidden: bool, text: Option<String>) {
        let shown = self.shown(hidden);
        let Some(Some(para)) = self.paragraphs.last_mut() else {
            return;
        };
        let Some(run) = &mut para.run else {
            return;
        };
        match (shown, text) {
            (Shown::Code, _) => {}
            (Shown::Dropped(_), Some(text)) if text.is_empty() => {}
            (Shown::Dropped(why), _) => run.drop_text(why),
            (Shown::Yes, Some(text)) => run.push(text),
            (Shown::Yes, None) => run.gap(),
        }
    }

    /// Text of a field's instruction, kept for the innermost field still in its instruction.
    fn instruction(&mut self, text: &str, hidden: bool) {
        if let Some(field) = self.fields.iter_mut().rev().find(|field| field.code) {
            field.undecided |= hidden;
            let room = MAX_FIELD_CODE.saturating_sub(field.instruction.chars().count());
            field.instruction.extend(text.chars().take(room));
        }
    }

    fn para(&mut self) -> Option<&mut Para> {
        self.paragraphs.last_mut().and_then(Option::as_mut)
    }

    fn run(&mut self) -> Option<&mut Run> {
        self.para().and_then(|para| para.run.as_deref_mut())
    }

    fn table(&mut self) -> Option<&mut Table> {
        match self.containers.last_mut() {
            Some(Container::Table(table)) => Some(table),
            _ => None,
        }
    }

    fn cell(&mut self) -> Option<&mut OpenCell> {
        self.table()
            .and_then(|table| table.row.as_mut())
            .and_then(|row| row.cell.as_mut())
    }

    /// What lies beneath a paragraph starting now, innermost first, shared by the paragraphs of
    /// one cell or text box. Every open cell it reaches is fixed, so its properties can no longer
    /// change.
    fn chain(&mut self) -> Rc<[Under]> {
        match self.containers.last_mut() {
            None => return self.no_chain.clone(),
            Some(Container::Table(table)) => {
                match table.row.as_mut().and_then(|row| row.cell.as_mut()) {
                    // In a table, outside any cell: Word lays it out nowhere.
                    None => return Rc::from(vec![Under::Nowhere]),
                    Some(cell) => {
                        if let Some(chain) = &cell.chain {
                            return chain.clone();
                        }
                    }
                }
            }
            Some(Container::TextBox {
                chain: Some(chain), ..
            }) => return chain.clone(),
            Some(Container::TextBox { .. }) => {}
        }
        let mut under = Vec::new();
        for container in self.containers.iter_mut().rev() {
            match container {
                Container::Table(table) => {
                    match table.row.as_mut().and_then(|row| row.cell.as_mut()) {
                        Some(cell) => under.push(Under::Cell(table.shape.clone(), cell.shared())),
                        None => under.push(Under::Nowhere),
                    }
                }
                Container::TextBox { info, host, .. } => {
                    under.push(Under::Box(info.clone()));
                    if let Some(host) = host {
                        under.push(Under::Host(host.clone()));
                    }
                }
            }
        }
        let chain: Rc<[Under]> = Rc::from(under);
        match self.containers.last_mut() {
            Some(Container::Table(table)) => {
                if let Some(cell) = table.row.as_mut().and_then(|row| row.cell.as_mut()) {
                    cell.chain = Some(chain.clone());
                }
            }
            Some(Container::TextBox { chain: slot, .. }) => *slot = Some(chain.clone()),
            None => {}
        }
        chain
    }

    fn in_table(&self) -> bool {
        self.containers
            .iter()
            .any(|container| matches!(container, Container::Table(_)))
    }
}

struct Doc<'l> {
    limits: &'l DocLimits,
    budget: ElementBudget,
    styles: Styles,
    theme: Option<ThemeColours>,
    settings: Settings,
    /// The page colour's candidates under the run (white among them unless Word's print layout
    /// shows the page colour), or `Err` when it does not parse or is undecided.
    page: Result<Vec<Rgb>, ()>,
    page_seen: bool,
    blocks: Vec<Block>,
    warnings: Warnings,
    /// The last chain judged, and what it puts beneath its paragraphs.
    last_beneath: Option<(Rc<[Under]>, Rc<Beneath>)>,
}

impl Doc<'_> {
    fn read_part(&mut self, name: &str, xml: &[u8], story: Story) -> Result<(), DocError> {
        let mut walker = Walker::new(xml, self.limits, &mut self.budget, document_namespace)?;
        if story == Story::Main {
            check_root(xml)?;
        }
        let mut part = Part {
            name,
            story,
            ordinal: 0,
            excluded: 0,
            body_started: false,
            paragraphs: Vec::new(),
            containers: Vec::new(),
            shapes: Vec::new(),
            shapetypes: HashMap::new(),
            fields: Vec::new(),
            sdts: Vec::new(),
            bound: 0,
            pending: Vec::new(),
            no_chain: Rc::from(Vec::new()),
            seen: 0,
            out: Vec::new(),
            hidden: Vec::new(),
        };
        let mut nodes: Vec<Node> = Vec::new();
        loop {
            match walker.next()? {
                Item::Start {
                    start,
                    namespace,
                    skipped,
                    hidden,
                } => {
                    let node = self.open(
                        &mut part,
                        &walker,
                        &start,
                        nodes.last().copied(),
                        namespace,
                        skipped,
                        hidden,
                    )?;
                    nodes.push(node);
                }
                Item::End => {
                    let node = nodes.pop().unwrap_or(Node::Other);
                    self.close(&mut part, node)?;
                }
                Item::Text {
                    text,
                    skipped,
                    hidden,
                } => {
                    if skipped || part.excluded > 0 {
                        continue;
                    }
                    match nodes.last() {
                        Some(Node::Text) => part.put(hidden, Some(text)),
                        Some(Node::InstrText) => part.instruction(&text, hidden),
                        Some(&Node::PosOffset(axis)) => {
                            if let Some(shape) = part.shapes.last_mut() {
                                match text.trim().parse::<i64>() {
                                    Ok(offset) if !hidden => shape.offset[axis] = Some(offset),
                                    _ => shape.undecided = true,
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Item::Eof => break,
            }
        }
        self.finish_part(part)
    }

    #[allow(clippy::too_many_arguments)]
    fn open(
        &mut self,
        part: &mut Part,
        walker: &Walker,
        start: &BytesStart,
        parent: Option<Node>,
        namespace: Option<&'static str>,
        skipped: bool,
        hidden: bool,
    ) -> Result<Node, DocError> {
        let name = start.local_name();
        let local = name.as_ref();
        let w = namespace == Some(W);
        // Every paragraph counts toward the ordinals, read or not.
        if w && local == "p" {
            part.ordinal += 1;
        }
        if skipped || part.excluded > 0 {
            return Ok(Node::Other);
        }
        // Anything but a first `w:pPr` is the paragraph's content.
        if parent == Some(Node::Paragraph)
            && !(w && local == "pPr")
            && let Some(para) = part.para()
        {
            para.content = true;
        }
        if !w {
            return self.open_drawing(part, walker, start, parent, namespace, local, hidden);
        }
        Ok(match (parent, local) {
            (None, "document") if part.story == Story::Main => Node::Document,
            (None, "hdr" | "ftr") if part.story != Story::Main => Node::Story,
            (None, "footnotes" | "endnotes") if part.story != Story::Main => Node::Notes,
            (Some(Node::Notes), "footnote" | "endnote") => Node::Story,
            (Some(Node::Document), "background") => {
                if part.body_started {
                    return Err(DocError::Malformed("docx_structure"));
                }
                let spec = ColourSpec::read(
                    walker,
                    start,
                    ["color", "themeColor", "themeTint", "themeShade"],
                )?;
                self.page = if hidden || self.page_seen {
                    Err(())
                } else {
                    self.page_colours(&spec)
                };
                self.page_seen = true;
                Node::Background
            }
            (Some(Node::Document), "body") => {
                part.body_started = true;
                Node::Body
            }
            (_, "p") => {
                let under = part.chain();
                let para_id = walker
                    .attr(start, Some(W14), "paraId")?
                    .filter(|id| id.len() == 8 && id.bytes().all(|b| b.is_ascii_hexdigit()));
                let undecided = if hidden {
                    Some(UNKNOWN_NAMESPACE)
                } else if level(parent) != Some(Level::Block) {
                    Some(UNDECIDED)
                } else {
                    None
                };
                part.paragraphs.push(Some(Para {
                    ordinal: part.ordinal,
                    para_id,
                    props: ParaProps {
                        undecided,
                        ..ParaProps::default()
                    },
                    runs: Vec::new(),
                    run: None,
                    under,
                    gap: false,
                    content: false,
                    has_props: false,
                    shared: None,
                }));
                Node::Paragraph
            }
            (_, "r") => match part.para() {
                Some(para) if para.run.is_none() => {
                    let mut run = Box::<Run>::default();
                    // A run outside a run's place, which Word would not read as the reader does.
                    if level(parent) != Some(Level::Run) {
                        run.props.undecided = Some(UNDECIDED);
                    }
                    para.run = Some(run);
                    Node::Run
                }
                _ => Node::Other,
            },
            (Some(Node::Run), "t") => Node::Text,
            // Text not directly in its run.
            (_, "t") => {
                if let Some(run) = part.run() {
                    run.drop_text(UNDECIDED);
                }
                Node::Other
            }
            (Some(Node::Run), "tab" | "ptab") => {
                part.put(hidden, Some("\t".into()));
                Node::Other
            }
            (Some(Node::Run), "br" | "cr") => {
                part.put(hidden, Some("\n".into()));
                Node::Other
            }
            (Some(Node::Run), "noBreakHyphen") => {
                part.put(hidden, Some("-".into()));
                Node::Other
            }
            (Some(Node::Run), "sym") => {
                let font = walker.w(start, "font")?;
                let code = walker.w(start, "char")?;
                let text = symbol(font.as_deref(), code.as_deref()).map(String::from);
                part.put(hidden, text);
                Node::Other
            }
            (Some(Node::Run), "drawing" | "pict") => {
                part.put(hidden, None);
                Node::Object
            }
            // Ruby text, note marks, page numbers and date blocks: Word draws something there.
            (
                Some(Node::Run),
                "ruby"
                | "footnoteReference"
                | "endnoteReference"
                | "footnoteRef"
                | "endnoteRef"
                | "pgNum"
                | "separator"
                | "continuationSeparator"
                | "dayShort"
                | "dayLong"
                | "monthShort"
                | "monthLong"
                | "yearShort"
                | "yearLong"
                | "contentPart",
            ) => {
                part.put(hidden, None);
                part.excluded += 1;
                Node::Excluded
            }
            (Some(Node::Run), "fldChar") => {
                self.field_char(part, walker, start, hidden)?;
                Node::Other
            }
            (Some(Node::Run), "instrText") => Node::InstrText,
            (Some(Node::Run), "rPr") => {
                if let Some(run) = part.run() {
                    if run.has_props {
                        run.props.undecided = run.props.undecided.or(Some(UNDECIDED));
                    }
                    if hidden {
                        run.props.undecided = run.props.undecided.or(Some(UNKNOWN_NAMESPACE));
                    }
                    run.has_props = true;
                }
                part.seen = 0;
                Node::RunProps
            }
            (Some(Node::RunProps), _) => {
                let mut seen = part.seen;
                if let Some(run) = part.run() {
                    if hidden {
                        run.props.undecided = run.props.undecided.or(Some(UNKNOWN_NAMESPACE));
                    }
                    run.props.read(walker, start, local, &mut seen)?;
                }
                part.seen = seen;
                Node::Other
            }
            (Some(Node::Paragraph), "pPr") => {
                if let Some(para) = part.para() {
                    if para.content {
                        return Err(DocError::Malformed("docx_structure"));
                    }
                    if para.has_props {
                        para.props.undecided = para.props.undecided.or(Some(UNDECIDED));
                    }
                    if hidden {
                        para.props.undecided = para.props.undecided.or(Some(UNKNOWN_NAMESPACE));
                    }
                    para.has_props = true;
                }
                part.seen = 0;
                Node::ParaProps
            }
            (Some(Node::ParaProps), _) => {
                let mut seen = part.seen;
                if let Some(para) = part.para() {
                    if hidden {
                        para.props.undecided = para.props.undecided.or(Some(UNKNOWN_NAMESPACE));
                    }
                    para.props.read(walker, start, local, &mut seen)?;
                }
                part.seen = seen;
                Node::Other
            }
            (Some(Node::RowProps), "del") => {
                let mut excluded = false;
                if let Some(table) = part.table() {
                    if hidden {
                        let mut shape = table.shape.borrow_mut();
                        shape.undecided = shape.undecided.or(Some(UNKNOWN_NAMESPACE));
                    } else if let Some(row) = &mut table.row
                        && !row.deleted
                    {
                        // The accepted view has no such row: not its content, not its place.
                        row.deleted = true;
                        table.shape.borrow_mut().rows.pop();
                        excluded = true;
                    }
                }
                part.excluded += u32::from(excluded);
                Node::Other
            }
            (_, "del" | "moveFrom") => {
                part.excluded += 1;
                Node::Excluded
            }
            (_, "object") => {
                part.put(hidden, None);
                self.warnings
                    .once(ExtractWarning::EmbeddedContentIgnored { what: "ole_object" })?;
                part.excluded += 1;
                Node::Excluded
            }
            (_, "altChunk") => {
                self.warnings
                    .once(ExtractWarning::EmbeddedContentIgnored { what: "alt_chunk" })?;
                part.excluded += 1;
                Node::Excluded
            }
            (_, "txbxContent") => {
                let info = match part.shapes.last_mut() {
                    Some(shape) => {
                        shape.text_read = true;
                        shape.box_info()
                    }
                    // Text in no shape the reader read: where Word draws it is not known.
                    None => BoxInfo {
                        hidden: Some("unknown_paint"),
                        fill: None,
                        text_colour: None,
                    },
                };
                let host = match part.paragraphs.last_mut() {
                    Some(Some(host)) => Some(
                        host.shared
                            .get_or_insert_with(|| Rc::new(host.props.clone()))
                            .clone(),
                    ),
                    _ => None,
                };
                part.paragraphs.push(None);
                part.containers.push(Container::TextBox {
                    info: Rc::new(info),
                    host,
                    chain: None,
                });
                Node::TextBox
            }
            (_, "sdt") => {
                part.sdts.push(false);
                Node::Sdt(level(parent))
            }
            (Some(Node::Sdt(_)), "sdtPr") => Node::SdtProps,
            (Some(Node::SdtProps), "dataBinding") => {
                // Word refills the control from its custom XML part when the document opens.
                if let Some(bound) = part.sdts.last_mut() {
                    *bound = true;
                }
                Node::Other
            }
            (Some(Node::Sdt(Some(level))), "sdtContent") => {
                if part.sdts.last() == Some(&true) {
                    part.bound += 1;
                    Node::BoundContent(level)
                } else {
                    Node::Container(level)
                }
            }
            (_, "customXml") => level(parent).map_or(Node::Other, Node::Container),
            (_, "hyperlink" | "smartTag" | "dir" | "bdo" | "ins" | "moveTo")
                if level(parent) == Some(Level::Run) =>
            {
                Node::Container(Level::Run)
            }
            (_, "fldSimple") if level(parent) == Some(Level::Run) => {
                if part.fields.len() as u64 >= u64::from(self.limits.max_xml_depth) {
                    return Err(DocError::TooComplex("field_depth"));
                }
                let instruction = walker.w(start, "instr")?.unwrap_or_default();
                let dirty = walker
                    .w(start, "dirty")?
                    .is_some_and(|value| Toggle::parse(Some(&value)) != Toggle::Off);
                let depth = part.fields.len();
                part.fields.push(Field {
                    code: false,
                    instruction: String::new(),
                    hide: self.field_hide(&instruction, dirty, part.story),
                    dirty,
                    undecided: hidden,
                });
                Node::SimpleField(depth)
            }
            (_, "tbl") => {
                if let Some(cell) = part.cell() {
                    cell.shared();
                }
                let undecided = if hidden {
                    Some(UNKNOWN_NAMESPACE)
                } else if level(parent) != Some(Level::Block) {
                    Some(UNDECIDED)
                } else {
                    None
                };
                part.containers.push(Container::Table(Table {
                    shape: Rc::new(RefCell::new(TableShape {
                        undecided,
                        ..TableShape::default()
                    })),
                    row: None,
                    has_props: false,
                }));
                Node::Table
            }
            (Some(Node::Table), "tblPr") => {
                if let Some(table) = part.table() {
                    let mut shape = table.shape.borrow_mut();
                    if !shape.rows.is_empty() {
                        return Err(DocError::Malformed("docx_structure"));
                    }
                    if table.has_props {
                        shape.undecided = shape.undecided.or(Some(UNDECIDED));
                    }
                    drop(shape);
                    table.has_props = true;
                }
                part.seen = 0;
                Node::TableProps
            }
            (Some(Node::TableProps), "tblStyle" | "tblLook") => {
                let style = (local == "tblStyle")
                    .then(|| walker.w(start, "val"))
                    .transpose()?
                    .flatten();
                let look = (local == "tblLook")
                    .then(|| Look::read(walker, start))
                    .transpose()?;
                let again = repeated(&mut part.seen, Some(u32::from(local == "tblLook")));
                if let Some(table) = part.table() {
                    let mut shape = table.shape.borrow_mut();
                    if hidden {
                        shape.undecided = shape.undecided.or(Some(UNKNOWN_NAMESPACE));
                    }
                    if again {
                        shape.undecided = shape.undecided.or(Some(UNDECIDED));
                    }
                    shape.style = style.or(shape.style.take());
                    shape.look = look.or(shape.look);
                }
                Node::Other
            }
            (_, "tr") => match part.table() {
                Some(table) => {
                    let mut shape = table.shape.borrow_mut();
                    if hidden {
                        shape.undecided = shape.undecided.or(Some(UNKNOWN_NAMESPACE));
                    } else if level(parent) != Some(Level::Row) {
                        shape.undecided = shape.undecided.or(Some(UNDECIDED));
                    }
                    shape.rows.push(RowShape {
                        cells: 0,
                        look: None,
                    });
                    drop(shape);
                    table.row = Some(OpenRow {
                        deleted: false,
                        has_props: false,
                        has_cells: false,
                        cell: None,
                    });
                    Node::Row
                }
                None => Node::Other,
            },
            (Some(Node::Row), "tblPrEx") => Node::RowExceptions,
            (Some(Node::RowExceptions), "tblLook") => {
                let look = Look::read(walker, start)?;
                if let Some(table) = part.table() {
                    let mut shape = table.shape.borrow_mut();
                    if hidden {
                        shape.undecided = shape.undecided.or(Some(UNKNOWN_NAMESPACE));
                    }
                    if let Some(row) = shape.rows.last_mut() {
                        row.look = Some(look);
                    }
                }
                Node::Other
            }
            (Some(Node::Row), "trPr") => {
                if let Some(table) = part.table()
                    && let Some(row) = &mut table.row
                {
                    if row.has_cells {
                        return Err(DocError::Malformed("docx_structure"));
                    }
                    if row.has_props {
                        let mut shape = table.shape.borrow_mut();
                        shape.undecided = shape.undecided.or(Some(UNDECIDED));
                    }
                    row.has_props = true;
                }
                part.seen = 0;
                Node::RowProps
            }
            (_, "tc") => match part.table() {
                Some(table) if table.row.is_some() => {
                    let mut shape = table.shape.borrow_mut();
                    if hidden {
                        shape.undecided = shape.undecided.or(Some(UNKNOWN_NAMESPACE));
                    } else if level(parent) != Some(Level::Cell) {
                        shape.undecided = shape.undecided.or(Some(UNDECIDED));
                    }
                    let row_index = shape.rows.len().saturating_sub(1);
                    let column = match shape.rows.last_mut() {
                        Some(row) => {
                            row.cells += 1;
                            row.cells - 1
                        }
                        None => 0,
                    };
                    drop(shape);
                    let row = table.row.as_mut().expect("an open row");
                    row.has_cells = true;
                    row.cell = Some(OpenCell {
                        at: CellAt {
                            row: row_index,
                            column,
                            props: CellProps::default(),
                        },
                        shared: None,
                        deleted: false,
                        has_props: false,
                        chain: None,
                    });
                    Node::Cell
                }
                _ => Node::Other,
            },
            (Some(Node::Cell), "tcPr") => {
                if let Some(cell) = part.cell() {
                    if cell.shared.is_some() {
                        return Err(DocError::Malformed("docx_structure"));
                    }
                    if cell.has_props {
                        cell.at.props.undecided = cell.at.props.undecided.or(Some(UNDECIDED));
                    }
                    cell.has_props = true;
                }
                part.seen = 0;
                Node::CellProps
            }
            (Some(Node::CellProps), _) => {
                let mut seen = part.seen;
                let mut deleted = false;
                if let Some(cell) = part.cell() {
                    if hidden {
                        cell.at.props.undecided =
                            cell.at.props.undecided.or(Some(UNKNOWN_NAMESPACE));
                    }
                    cell.at.props.read(walker, start, local, &mut seen)?;
                    if local == "cellDel" && !hidden && !cell.deleted {
                        cell.deleted = true;
                        deleted = true;
                    }
                }
                part.seen = seen;
                if deleted {
                    // The accepted view has no such cell: the cells after it move up a column.
                    if let Some(table) = part.table()
                        && let Some(row) = table.shape.borrow_mut().rows.last_mut()
                    {
                        row.cells -= 1;
                    }
                    part.excluded += 1;
                }
                Node::Other
            }
            _ => Node::Other,
        })
    }

    /// Elements outside WordprocessingML: OLE objects, Word's symbol extension, and the frames,
    /// shapes and fills that decide what a text box's text lies on.
    #[allow(clippy::too_many_arguments)]
    fn open_drawing(
        &mut self,
        part: &mut Part,
        walker: &Walker,
        start: &BytesStart,
        parent: Option<Node>,
        namespace: Option<&'static str>,
        local: &str,
        hidden: bool,
    ) -> Result<Node, DocError> {
        let Some(namespace) = namespace else {
            return Ok(Node::Other);
        };
        // A shape property where Word's choice cannot be decided leaves the shape undecided.
        if hidden && let Some(shape) = part.shapes.last_mut() {
            shape.undecided = true;
        }
        let toggle = |value: Option<String>| value.map(|value| Toggle::parse(Some(&value)));
        Ok(match (namespace, local, parent) {
            (OFFICE, "OLEObject", _) => {
                self.warnings
                    .once(ExtractWarning::EmbeddedContentIgnored { what: "ole_object" })?;
                part.excluded += 1;
                Node::Excluded
            }
            (W16SE, "symEx", Some(Node::Run)) => {
                let symbol = walker
                    .attr(start, Some(W16SE), "char")?
                    .and_then(|hex| u32::from_str_radix(&hex, 16).ok())
                    .and_then(char::from_u32)
                    .filter(|c| !c.is_control());
                part.put(hidden, symbol.map(String::from));
                Node::Other
            }
            (WP, "inline" | "anchor", _) => {
                let mut shape = ShapeState::inside(part.shapes.last());
                shape.anchored = local == "anchor";
                shape.undecided |= hidden;
                match toggle(walker.attr(start, None, "simplePos")?) {
                    Some(Toggle::On) => shape.simple = true,
                    Some(Toggle::Bad) => shape.undecided = true,
                    _ => {}
                }
                part.shapes.push(shape);
                Node::Shape
            }
            (WP, "docPr", _) | (WPS | WPG | WPC, "cNvPr", _) => {
                if let Some(shape) = part.shapes.last_mut() {
                    match toggle(walker.attr(start, None, "hidden")?) {
                        Some(Toggle::On) => shape.hidden = true,
                        Some(Toggle::Bad) => shape.undecided = true,
                        _ => {}
                    }
                }
                Node::Other
            }
            (WP, "simplePos", _) => {
                if let Some(shape) = part.shapes.last_mut()
                    && shape.simple
                {
                    for (axis, name) in ["x", "y"].into_iter().enumerate() {
                        match walker.attr(start, None, name)?.map(|v| v.parse::<i64>()) {
                            Some(Ok(offset)) => shape.offset[axis] = Some(offset),
                            Some(Err(_)) => shape.undecided = true,
                            None => {}
                        }
                    }
                }
                Node::Other
            }
            (WP, "positionH" | "positionV", _) => Node::Position(usize::from(local == "positionV")),
            (WP, "posOffset", Some(Node::Position(axis))) => {
                if part.shapes.last().is_some_and(|shape| shape.simple) {
                    Node::Other
                } else {
                    Node::PosOffset(axis)
                }
            }
            (WP, "extent", _) => {
                if let Some(shape) = part.shapes.last_mut() {
                    for (axis, name) in ["cx", "cy"].into_iter().enumerate() {
                        match walker.attr(start, None, name)?.map(|v| v.parse::<i64>()) {
                            Some(Ok(extent)) => shape.extent[axis] = extent,
                            Some(Err(_)) => shape.undecided = true,
                            None => {}
                        }
                    }
                }
                Node::Other
            }
            (WPS, "wsp", _) | (WPG, "wgp", _) | (WPC, "wpc", _) => {
                let mut shape = ShapeState::inside(part.shapes.last());
                shape.undecided |= hidden;
                part.shapes.push(shape);
                Node::Shape
            }
            (WPS, "spPr", _) | (WPG, "grpSpPr", _) => {
                if part.shapes.last().is_some_and(|shape| shape.text_read) {
                    return Err(DocError::Malformed("docx_structure"));
                }
                Node::ShapeProps
            }
            (DRAWINGML, fill, Some(Node::ShapeProps)) => {
                let set = match fill {
                    "noFill" => Some(ShapeFill::None),
                    // Unknown until its colour is read.
                    "solidFill" => Some(ShapeFill::Unknown),
                    "gradFill" | "blipFill" | "pattFill" | "grpFill" => Some(ShapeFill::Unknown),
                    _ => None,
                };
                match (set, part.shapes.last_mut()) {
                    (Some(set), Some(shape)) => {
                        let first = shape.fill.is_none();
                        shape.set_fill(set);
                        if fill == "solidFill" {
                            shape.awaiting = first.then_some(Target::Fill);
                            Node::ColourHolder(Target::Fill)
                        } else {
                            Node::Other
                        }
                    }
                    _ => Node::Other,
                }
            }
            (WPS, "style", _) => {
                if part.shapes.last().is_some_and(|shape| shape.text_read) {
                    return Err(DocError::Malformed("docx_structure"));
                }
                Node::ShapeStyle
            }
            (DRAWINGML, "fillRef", Some(Node::ShapeStyle)) => {
                if let Some(shape) = part.shapes.last_mut() {
                    // Style fill 0 is no fill; the others come from the theme's fill styles,
                    // which the reader does not read.
                    shape.style_fill = Some(match walker.attr(start, None, "idx")?.as_deref() {
                        Some("0") => ShapeFill::None,
                        _ => ShapeFill::Unknown,
                    });
                }
                Node::Other
            }
            (DRAWINGML, "fontRef", Some(Node::ShapeStyle)) => {
                if let Some(shape) = part.shapes.last_mut() {
                    let first = shape.text_colour.is_none();
                    // No colour child: the theme's font colour, which the run's own colour
                    // cascade already stands for.
                    shape.text_colour = Some(if first {
                        ShapeFill::None
                    } else {
                        ShapeFill::Unknown
                    });
                    shape.awaiting = first.then_some(Target::Text);
                }
                Node::ColourHolder(Target::Text)
            }
            (DRAWINGML, colour, Some(Node::ColourHolder(target))) => {
                let read = drawing_colour(walker, start, colour)?;
                if let Some(shape) = part.shapes.last_mut() {
                    // Only the holder's first colour, read whole, is the colour.
                    let first = shape.awaiting == Some(target);
                    shape.awaiting = None;
                    let value = match (first, read) {
                        (true, Some(spec)) => ShapeFill::Colour(spec),
                        _ => ShapeFill::Unknown,
                    };
                    match target {
                        Target::Fill => shape.fill = Some(value),
                        Target::Text => shape.text_colour = Some(value),
                    }
                }
                Node::Colour(target)
            }
            // A colour transform (`a:lumMod`, `a:alpha` ...) changes the colour.
            (_, _, Some(Node::Colour(target))) => {
                if let Some(shape) = part.shapes.last_mut() {
                    match target {
                        Target::Fill => shape.fill = Some(ShapeFill::Unknown),
                        Target::Text => shape.text_colour = Some(ShapeFill::Unknown),
                    }
                }
                Node::Other
            }
            (
                VML,
                "shape" | "rect" | "roundrect" | "oval" | "line" | "polyline" | "curve" | "arc"
                | "image" | "group" | "shapetype",
                _,
            ) => {
                // A shape inside a VML group is placed in the group's own coordinates.
                let in_group = part.shapes.last().is_some_and(|outer| outer.vml);
                let mut shape = ShapeState::inside(part.shapes.last());
                shape.vml = true;
                shape.undecided |= hidden;
                let white = ShapeFill::Colour(ColourSpec::rgb(Rgb::WHITE));
                shape.vml_default = Some(match walker.attr(start, None, "type")? {
                    None => white,
                    Some(reference) => match part.shapetypes.get(reference.trim_start_matches('#'))
                    {
                        Some(fill) => fill.unwrap_or(white),
                        None => ShapeFill::Unknown,
                    },
                });
                if local == "shapetype" {
                    shape.shapetype = walker.attr(start, None, "id")?;
                }
                vml_style(
                    &mut shape,
                    &walker.attr(start, None, "style")?.unwrap_or_default(),
                    in_group,
                );
                let filled = toggle(walker.attr(start, None, "filled")?);
                match (filled, walker.attr(start, None, "fillcolor")?) {
                    (Some(Toggle::Off), _) => shape.fill = Some(ShapeFill::None),
                    (Some(Toggle::Bad), _) => shape.fill = Some(ShapeFill::Unknown),
                    (_, Some(colour)) => {
                        shape.fill = Some(vml_colour(&colour).map_or(ShapeFill::Unknown, |rgb| {
                            ShapeFill::Colour(ColourSpec::rgb(rgb))
                        }))
                    }
                    _ => {}
                }
                part.shapes.push(shape);
                Node::Shape
            }
            (VML, "fill", Some(Node::Shape)) => {
                if let Some(shape) = part.shapes.last_mut() {
                    if shape.text_read {
                        return Err(DocError::Malformed("docx_structure"));
                    }
                    let on = toggle(walker.attr(start, None, "on")?);
                    let kind = walker.attr(start, None, "type")?;
                    let opacity = walker.attr(start, None, "opacity")?;
                    let colour = walker.attr(start, None, "color")?;
                    shape.fill = Some(match on {
                        Some(Toggle::Off) => ShapeFill::None,
                        Some(Toggle::Bad) => ShapeFill::Unknown,
                        _ if !matches!(kind.as_deref(), None | Some("solid"))
                            || opacity.as_deref().is_some_and(|value| value != "1") =>
                        {
                            ShapeFill::Unknown
                        }
                        _ => match colour {
                            Some(colour) => vml_colour(&colour).map_or(ShapeFill::Unknown, |rgb| {
                                ShapeFill::Colour(ColourSpec::rgb(rgb))
                            }),
                            None => shape
                                .fill
                                .or(shape.vml_default)
                                .unwrap_or(ShapeFill::Colour(ColourSpec::rgb(Rgb::WHITE))),
                        },
                    });
                }
                Node::Other
            }
            _ => Node::Other,
        })
    }

    /// A `w:fldChar`: a field begins, its result starts, or it ends. One that sits where Word's
    /// choice cannot be decided leaves its field undecided (its text hidden) until it ends.
    fn field_char(
        &self,
        part: &mut Part,
        walker: &Walker,
        start: &BytesStart,
        hidden: bool,
    ) -> Result<(), DocError> {
        let dirty = walker
            .w(start, "dirty")?
            .is_some_and(|value| Toggle::parse(Some(&value)) != Toggle::Off);
        match walker.w(start, "fldCharType")?.as_deref() {
            Some("begin") => {
                if part.fields.len() as u64 >= u64::from(self.limits.max_xml_depth) {
                    return Err(DocError::TooComplex("field_depth"));
                }
                part.fields.push(Field {
                    code: true,
                    instruction: String::new(),
                    hide: None,
                    dirty,
                    undecided: hidden,
                });
            }
            Some("separate") => {
                if let Some(field) = part.fields.last_mut() {
                    if hidden || !field.code {
                        field.undecided = true;
                    } else {
                        field.hide = self.field_hide(&field.instruction, field.dirty, part.story);
                    }
                    field.code = false;
                }
            }
            Some("end") => {
                if hidden {
                    if let Some(field) = part.fields.last_mut() {
                        field.undecided = true;
                    }
                } else {
                    part.fields.pop();
                }
            }
            _ => {
                if let Some(field) = part.fields.last_mut() {
                    field.undecided = true;
                }
            }
        }
        Ok(())
    }

    /// Why a field's result is not what Word shows: a field Word never shows, one it computes as
    /// it lays the document out or opens it, any field of a header or footer (recomputed on
    /// every page), and any field Word updates on open (`w:updateFields`, `w:dirty`). A
    /// hyperlink keeps its text when updated.
    fn field_hide(&self, instruction: &str, dirty: bool, story: Story) -> Option<&'static str> {
        let kind = instruction
            .split(|c: char| c.is_whitespace() || c == '\\')
            .find(|word| !word.is_empty())
            .unwrap_or_default()
            .to_ascii_uppercase();
        if kind == "HYPERLINK" {
            return None;
        }
        (NEVER_SHOWN.contains(&kind.as_str())
            || LAYOUT_COMPUTED.contains(&kind.as_str())
            || story == Story::HeaderFooter
            || self.settings.update_fields
            || dirty)
            .then_some("field_result")
    }

    fn close(&mut self, part: &mut Part, node: Node) -> Result<(), DocError> {
        match node {
            Node::Paragraph => {
                if let Some(Some(para)) = part.paragraphs.pop() {
                    // A paragraph with no kept run shows nothing and warns of nothing.
                    if para.runs.is_empty() {
                    } else if part.in_table() {
                        let mut para = Box::new(para);
                        para.runs.shrink_to_fit();
                        part.pending.push(para);
                    } else {
                        self.judge(part, para)?;
                    }
                }
            }
            Node::Run => {
                if let Some(para) = part.para()
                    && let Some(mut run) = para.run.take()
                {
                    if !run.text.is_empty() || run.dropped.is_some() {
                        if para.runs.len() >= MAX_PARAGRAPH_RUNS {
                            return Err(DocError::TooComplex("paragraph_runs"));
                        }
                        run.gap_before |= para.gap;
                        para.gap = false;
                        para.runs.push(*run);
                    } else if run.gap_before || run.gap_after {
                        para.gap = true;
                    }
                }
            }
            Node::Excluded => part.excluded -= 1,
            Node::TextBox => {
                part.paragraphs.pop();
                part.containers.pop();
            }
            Node::Table => {
                part.containers.pop();
                if !part.in_table() {
                    for para in std::mem::take(&mut part.pending) {
                        self.judge(part, *para)?;
                    }
                }
            }
            Node::Row => {
                let row = part.table().and_then(|table| table.row.take());
                if row.is_some_and(|row| row.deleted) {
                    part.excluded -= 1;
                }
            }
            Node::Cell => {
                let cell = part
                    .table()
                    .and_then(|table| table.row.as_mut())
                    .and_then(|row| row.cell.take());
                if cell.is_some_and(|cell| cell.deleted) {
                    part.excluded -= 1;
                }
            }
            Node::Shape => {
                if let Some(shape) = part.shapes.pop()
                    && let Some(id) = shape.shapetype
                {
                    part.shapetypes.insert(id, shape.fill);
                }
            }
            Node::Sdt(_) => {
                part.sdts.pop();
            }
            Node::BoundContent(_) => part.bound -= 1,
            Node::SimpleField(depth) => part.fields.truncate(depth),
            _ => {}
        }
        Ok(())
    }
}

/// The character a `w:sym` stands for, when the reader can tell: the ASCII-range codes of the
/// Symbol font (in their private-use form too) as Adobe's Symbol encoding gives them. Any other
/// symbol is a glyph the reader cannot decode.
fn symbol(font: Option<&str>, code: Option<&str>) -> Option<char> {
    if !font?.eq_ignore_ascii_case("Symbol") {
        return None;
    }
    let code = u32::from_str_radix(code?, 16).ok()?;
    let code = if (0xF020..=0xF03F).contains(&code) {
        code - 0xF000
    } else {
        code
    };
    Some(match code {
        0x22 => '\u{2200}',
        0x24 => '\u{2203}',
        0x27 => '\u{220B}',
        0x2A => '\u{2217}',
        0x2D => '\u{2212}',
        0x20..=0x3F => char::from_u32(code)?,
        _ => return None,
    })
}

/// An `a:srgbClr`, `a:schemeClr`, `a:prstClr` (black or white) or `a:sysClr` (its `lastClr`);
/// `None` for any other colour form or a value that does not parse.
fn drawing_colour(
    walker: &Walker,
    start: &BytesStart,
    local: &str,
) -> Result<Option<ColourSpec>, DocError> {
    let val = walker.attr(start, None, "val")?;
    Ok(match local {
        "srgbClr" => val
            .and_then(|hex| colour::parse_hex(&hex))
            .map(ColourSpec::rgb),
        "schemeClr" => match val.as_deref().map(ThemeRef::drawing) {
            Some(ThemeRef::Bad) | None => None,
            Some(theme) => Some(ColourSpec {
                theme: Some(theme),
                ..ColourSpec::default()
            }),
        },
        "prstClr" => match val.as_deref() {
            Some("black") => Some(ColourSpec::rgb(Rgb::BLACK)),
            Some("white") => Some(ColourSpec::rgb(Rgb::WHITE)),
            _ => None,
        },
        "sysClr" => walker
            .attr(start, None, "lastClr")?
            .and_then(|hex| colour::parse_hex(&hex))
            .map(ColourSpec::rgb),
        _ => None,
    })
}

/// A VML colour: `#RRGGBB`, `#RGB` or one of the sixteen HTML colour names, with anything after
/// a space (`white [3212]`) ignored.
fn vml_colour(value: &str) -> Option<Rgb> {
    let value = value.split_whitespace().next()?;
    if let Some(hex) = value.strip_prefix('#') {
        return match hex.len() {
            6 => colour::parse_hex(hex),
            3 => colour::parse_hex(&hex.chars().flat_map(|c| [c, c]).collect::<String>()),
            _ => None,
        };
    }
    let named = [
        ("black", "000000"),
        ("silver", "C0C0C0"),
        ("gray", "808080"),
        ("white", "FFFFFF"),
        ("maroon", "800000"),
        ("red", "FF0000"),
        ("purple", "800080"),
        ("fuchsia", "FF00FF"),
        ("green", "008000"),
        ("lime", "00FF00"),
        ("olive", "808000"),
        ("yellow", "FFFF00"),
        ("navy", "000080"),
        ("blue", "0000FF"),
        ("teal", "008080"),
        ("aqua", "00FFFF"),
    ];
    named
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(value))
        .and_then(|(_, hex)| colour::parse_hex(hex))
}

/// A VML shape's `style`: hidden, floating, and its offsets and size (not for a shape in a group,
/// whose offsets are in the group's own coordinates).
fn vml_style(shape: &mut ShapeState, style: &str, in_group: bool) {
    const EMU_PER_POINT: f64 = 12_700.0;
    for declaration in style.split(';') {
        let Some((name, value)) = declaration.split_once(':') else {
            continue;
        };
        let (name, value) = (name.trim().to_ascii_lowercase(), value.trim());
        // A bare number is in pixels, as CSS reads one.
        let emu = || {
            points(value, true)
                .or_else(|| value.parse::<f64>().ok().map(|pixels| pixels * 0.75))
                .filter(|points| points.is_finite())
                .map(|points| (points * EMU_PER_POINT) as i64)
        };
        match name.as_str() {
            "visibility" if value.eq_ignore_ascii_case("hidden") => shape.hidden = true,
            "display" if value.eq_ignore_ascii_case("none") => shape.hidden = true,
            "position" if value.eq_ignore_ascii_case("absolute") => shape.anchored = true,
            "margin-left" | "left" | "margin-top" | "top" if !in_group => {
                let axis = usize::from(name.ends_with("top"));
                match emu() {
                    Some(offset) => shape.offset[axis] = Some(offset),
                    None => shape.undecided = true,
                }
            }
            "width" | "height" if !in_group => {
                if let Some(extent) = emu() {
                    shape.extent[usize::from(name == "height")] = extent;
                }
            }
            _ => {}
        }
    }
}

impl Doc<'_> {
    /// A paragraph's effective properties: the defaults, its cell's table level, its style and
    /// its own; with the merged paragraph style.
    fn para_props(
        &self,
        own: &ParaProps,
        level: Option<&TableLevel>,
    ) -> Result<(ParaProps, Option<Rc<Merged>>), DocError> {
        let styles = &self.styles;
        let style = styles
            .resolve(own.style.as_deref(), StyleKind::Paragraph)
            .map(|id| styles.merged(id, StyleKind::Paragraph))
            .transpose()?;
        let mut props = styles.defaults.para.clone();
        if let Some(level) = level {
            props.overlay(&level.props.para);
        }
        if let Some(style) = &style {
            props.overlay(&style.props.para);
        }
        props.overlay(own);
        Ok((props, style))
    }

    /// What a chain of containers puts beneath its paragraphs: its innermost cell's table level
    /// (table style formatting reaches a paragraph only in its own cell, not in a text box), the
    /// layers outward to the page, and what hides or leaves undecided every run.
    fn beneath(&self, under: &[Under]) -> Result<Beneath, DocError> {
        let mut levels = Vec::with_capacity(under.len());
        for item in under {
            levels.push(match item {
                Under::Cell(shape, cell) => Some(self.table_level(&shape.borrow(), cell)?),
                _ => None,
            });
        }
        let mut beneath = Beneath::default();
        for (at, item) in under.iter().enumerate() {
            match item {
                Under::Cell(_, cell) => {
                    let level = levels[at].as_ref().expect("a cell's table level");
                    if cell.props.merged {
                        beneath.whole = beneath.whole.or(Some("merged_cell"));
                    }
                    beneath.undecided = beneath
                        .undecided
                        .or(cell.props.undecided)
                        .or(level.undecided);
                    let layers = &mut beneath.layers;
                    layers.extend(cell.props.shading.map(Layer::Shading));
                    layers.extend(level.region_shading.map(Layer::Shading));
                    layers.extend(level.props.cell.shading.map(Layer::Shading));
                }
                Under::Box(info) => {
                    beneath.whole = beneath.whole.or(info.hidden);
                    if at == 0 {
                        beneath.box_colour = info.text_colour;
                    }
                    if let Some(fill) = &info.fill {
                        beneath.layers.push(Layer::Fill(match self.paint(fill) {
                            Paint::Colours(colours) => Ok(colours),
                            Paint::Auto | Paint::Bad => Err(()),
                        }));
                    }
                }
                Under::Host(props) => {
                    let host_level = match under.get(at + 1) {
                        Some(Under::Cell(..)) => levels[at + 1].as_ref(),
                        _ => None,
                    };
                    let (host, _) = self.para_props(props, host_level)?;
                    beneath.undecided = beneath.undecided.or(host.undecided);
                    beneath.layers.extend(host.shading.map(Layer::Shading));
                }
                Under::Nowhere => beneath.whole = beneath.whole.or(Some(UNDECIDED)),
            }
        }
        beneath.level = levels.into_iter().next().flatten();
        Ok(beneath)
    }

    /// Judges a finished paragraph's runs and records its visible text and hidden reason. The
    /// text of a run left out between two visible stretches leaves `GAP` in their place.
    fn judge(&mut self, part: &mut Part, para: Para) -> Result<(), DocError> {
        deadline::check()?;
        // The paragraphs of one cell or text box share their chain, judged once for all of them.
        let beneath = match &self.last_beneath {
            Some((chain, beneath)) if Rc::ptr_eq(chain, &para.under) => beneath.clone(),
            _ => {
                let beneath = Rc::new(self.beneath(&para.under)?);
                self.last_beneath = Some((para.under.clone(), beneath.clone()));
                beneath
            }
        };
        let level = beneath.level.as_ref();
        let (para_props, para_style) = self.para_props(&para.props, level)?;
        let whole = beneath.whole;
        let undecided = para_props.undecided.or(beneath.undecided);
        let own: Vec<Layer> = para_props.shading.map(Layer::Shading).into_iter().collect();

        let styles = &self.styles;
        let mut text = String::new();
        let (mut gap, mut why) = (false, None::<&'static str>);
        for run in para.runs {
            if let Some(reason) = whole {
                why = why.or(Some(reason));
                continue;
            }
            let char_style = styles
                .resolve(run.props.style.as_deref(), StyleKind::Character)
                .map(|id| styles.merged(id, StyleKind::Character))
                .transpose()?;
            let run_levels = [
                level.map(|level| &level.props.run),
                para_style.as_ref().map(|style| &style.props.run),
                char_style.as_ref().map(|style| &style.props.run),
            ];
            let mut effective = styles.defaults.run.clone();
            for props in run_levels.iter().flatten() {
                effective.overlay(props);
            }
            effective.overlay(&run.props);
            let hidden = if run.text.is_empty() {
                None
            } else {
                self.hidden_why(
                    &run.props,
                    &run_levels,
                    &effective,
                    &run.text,
                    undecided.or(effective.undecided),
                    (&own, &beneath.layers),
                    beneath.box_colour.as_ref(),
                )
            };
            match hidden {
                Some(reason) => {
                    why = why.or(Some(reason));
                    gap = true;
                }
                None => {
                    if let Some(reason) = run.dropped {
                        why = why.or(Some(reason));
                    }
                    if run.text.is_empty() {
                        gap = true;
                        continue;
                    }
                    // Neighbours never join across what was left out.
                    if (gap || run.gap_before) && !text.is_empty() {
                        text.push(GAP);
                    }
                    if text.is_empty() {
                        text = run.text;
                    } else {
                        text.push_str(&run.text);
                    }
                    gap = run.gap_after;
                }
            }
        }
        if let Some(why) = why {
            if (part.hidden.len() + self.warnings.list.len()) as u64
                >= u64::from(self.limits.max_blocks)
            {
                return Err(DocError::TooComplex("warnings"));
            }
            part.hidden.push((para.ordinal, why, para.para_id.clone()));
        }
        if text.chars().any(|c| !c.is_whitespace() && c != GAP) {
            if (part.out.len() + self.blocks.len()) as u64 >= u64::from(self.limits.max_blocks) {
                return Err(DocError::TooComplex("blocks"));
            }
            part.out.push((para.ordinal, text, para.para_id));
        }
        Ok(())
    }

    /// Why a run is hidden, if it is: `vanish`, `tiny`, `low_contrast`, or why its properties
    /// sit where Word's choice cannot be decided.
    #[allow(clippy::too_many_arguments)]
    fn hidden_why(
        &self,
        direct: &RunProps,
        levels: &[Option<&RunProps>; 3],
        effective: &RunProps,
        text: &str,
        undecided: Option<&'static str>,
        beneath: (&[Layer], &[Layer]),
        box_colour: Option<&ColourSpec>,
    ) -> Option<&'static str> {
        if self.vanished(direct, levels, effective) {
            return Some("vanish");
        }
        if self.tiny(effective, text) {
            return Some("tiny");
        }
        if self.low_contrast(effective, direct, beneath, box_colour) {
            return Some("low_contrast");
        }
        undecided
    }

    /// `w:vanish` is a toggle (ECMA-376 Part 1 §17.7.3): the run's own value wins; otherwise the
    /// run is hidden when either reading of the toggle rule hides it. `w:specVanish` and
    /// `w:webHidden` hide too (over-hiding is the safe direction).
    fn vanished(
        &self,
        direct: &RunProps,
        levels: &[Option<&RunProps>; 3],
        effective: &RunProps,
    ) -> bool {
        let defaults = self.styles.defaults.run.vanish;
        let simple = |toggle: Option<Toggle>| matches!(toggle, Some(Toggle::On | Toggle::Bad));
        if simple(effective.spec_vanish) || simple(effective.web_hidden) {
            return true;
        }
        if let Some(own) = direct.vanish {
            return own != Toggle::Off;
        }
        let values: Vec<Option<Toggle>> = levels
            .iter()
            .map(|level| level.and_then(|props| props.vanish))
            .collect();
        if values.contains(&Some(Toggle::Bad)) || defaults == Some(Toggle::Bad) {
            return true;
        }
        let on = |toggle: Option<Toggle>| toggle == Some(Toggle::On);
        let set: Vec<bool> = values
            .iter()
            .flatten()
            .map(|&toggle| toggle == Toggle::On)
            .collect();
        let first = if set.is_empty() {
            on(defaults)
        } else {
            set.iter().fold(false, |acc, &value| acc ^ value)
        };
        let second = values
            .iter()
            .map(|&value| {
                if value.is_some() {
                    on(value)
                } else {
                    on(defaults)
                }
            })
            .fold(false, |acc, value| acc ^ value);
        first || second
    }

    /// Below `min_visible_pt`: `w:szCs` for a complex-script run (`w:cs` or `w:rtl`), the smaller
    /// of the two sizes for a run holding Thai, else `w:sz`.
    fn tiny(&self, effective: &RunProps, text: &str) -> bool {
        let complex = [effective.cs, effective.rtl];
        let sizes = if complex.contains(&Some(Toggle::On)) {
            vec![effective.size_cs.or(effective.size)]
        } else if complex.contains(&Some(Toggle::Bad))
            || text.chars().any(|c| ('\u{0E00}'..='\u{0E7F}').contains(&c))
        {
            vec![effective.size, effective.size_cs]
        } else {
            vec![effective.size]
        };
        let floor = f64::from(self.limits.min_visible_pt) * 2.0;
        sizes.into_iter().flatten().any(|size| match size {
            Size::HalfPoints(half_points) => half_points < floor,
            Size::Bad => true,
        })
    }

    /// Below `min_contrast` against what lies beneath, for any candidate colour and background.
    /// In a shape, the shape's own text colour counts too wherever the run itself sets none.
    fn low_contrast(
        &self,
        effective: &RunProps,
        direct: &RunProps,
        (own, beneath): (&[Layer], &[Layer]),
        box_colour: Option<&ColourSpec>,
    ) -> bool {
        let mut colours = match effective.colour.as_ref().map(|spec| self.paint(spec)) {
            None | Some(Paint::Auto) => Vec::new(),
            Some(Paint::Bad) => return true,
            Some(Paint::Colours(colours)) => colours,
        };
        let own_colour = direct
            .colour
            .as_ref()
            .is_some_and(|spec| !matches!(self.paint(spec), Paint::Auto));
        if !own_colour && let Some(spec) = box_colour {
            match self.paint(spec) {
                Paint::Colours(more) => colours.extend(more),
                Paint::Bad => return true,
                Paint::Auto => {}
            }
        }
        // Automatic text takes `readable_on` of its background, never low-contrast by itself.
        if colours.is_empty() {
            return false;
        }
        let mut near = Vec::with_capacity(own.len() + 2);
        if let Some(highlight) = effective.highlight {
            near.push(Layer::Highlight(highlight));
        }
        if let Some(shading) = effective.shading {
            near.push(Layer::Shading(shading));
        }
        near.extend(own.iter().cloned());
        let Ok(backgrounds) = self.background(&near, beneath) else {
            return true;
        };
        let floor = self.limits.min_contrast;
        colours.iter().any(|&colour| {
            backgrounds
                .iter()
                .any(|&background| colour::contrast_ratio(colour, background) < floor)
        })
    }

    /// The colours that may lie beneath a run: the first layer of `near`, then of `far`, that
    /// shows one, else the page.
    fn background(&self, near: &[Layer], far: &[Layer]) -> Result<Vec<Rgb>, ()> {
        let (layer, near, far) = match (near.split_first(), far.split_first()) {
            (Some((layer, rest)), _) => (layer, rest, far),
            (None, Some((layer, rest))) => (layer, &[][..], rest),
            (None, None) => return self.page.clone(),
        };
        match layer {
            Layer::Highlight(Highlight::None) => self.background(near, far),
            Layer::Highlight(Highlight::Rgb(colour)) => Ok(vec![*colour]),
            Layer::Highlight(Highlight::Bad) => Err(()),
            Layer::Fill(colours) => colours.clone(),
            Layer::Shading(shading) => {
                // An automatic pattern colour is black (Word draws it so); an automatic fill
                // lets the layer beneath show through.
                let pattern = || match self.paint(&shading.colour) {
                    Paint::Auto => Ok(vec![Rgb::BLACK]),
                    Paint::Colours(colours) => Ok(colours),
                    Paint::Bad => Err(()),
                };
                let fill = || match self.paint(&shading.fill) {
                    Paint::Auto => self.background(near, far),
                    Paint::Colours(colours) => Ok(colours),
                    Paint::Bad => Err(()),
                };
                match shading.pattern {
                    Pattern::Clear | Pattern::Nil => fill(),
                    Pattern::Solid => pattern(),
                    Pattern::Percent(percent) => {
                        let (over, under) = (pattern()?, fill()?);
                        Ok(over
                            .iter()
                            .flat_map(|&over| {
                                under
                                    .iter()
                                    .map(move |&under| colour::mix(over, under, percent))
                            })
                            .collect())
                    }
                    Pattern::Other => {
                        let mut colours = pattern()?;
                        colours.extend(fill()?);
                        Ok(colours)
                    }
                }
            }
        }
    }

    /// The colours a colour attribute set may stand for. A theme colour resolves through the
    /// scheme mapping and the theme part. Where the mapping of an accent or hyperlink colour is
    /// not the identity, what Word draws is not known (Word writes only the identity), so the
    /// slot it names directly counts too. A slot the theme cannot resolve has two candidates, the
    /// RGB value beside it and the default Office theme's colour.
    fn paint(&self, spec: &ColourSpec) -> Paint {
        let Some(theme_ref) = spec.theme else {
            return match spec.value {
                None | Some(Hex::Auto) => Paint::Auto,
                Some(Hex::Rgb(colour)) => Paint::Colours(vec![colour]),
                Some(Hex::Bad) => Paint::Bad,
            };
        };
        let tint = match (spec.tint, spec.shade) {
            (None, None) => 0.0,
            (Some(Some(tint)), None) => 1.0 - f64::from(tint) / 255.0,
            (None, Some(Some(shade))) => -(1.0 - f64::from(shade) / 255.0),
            // Both, or one that does not parse.
            _ => return Paint::Bad,
        };
        let slots = match theme_ref {
            ThemeRef::Slot(slot) => vec![slot],
            ThemeRef::Mapped(at) => {
                let at = usize::from(at);
                match self.settings.mapping[at] {
                    Mapping::Slot(slot) => {
                        let direct = MAPPED[at].3;
                        if at >= 4 && slot != direct {
                            vec![slot, direct]
                        } else {
                            vec![slot]
                        }
                    }
                    Mapping::Bad => return Paint::Bad,
                }
            }
            ThemeRef::Bad => Vec::new(),
        };
        let mut candidates = Vec::new();
        let mut unresolved = slots.is_empty();
        for slot in slots {
            match &self.theme {
                Some(theme) if theme.slot(slot).is_some() => {
                    candidates.push(theme.resolve(slot, tint))
                }
                _ => {
                    unresolved = true;
                    candidates.push(ThemeColours::office_default().resolve(slot, tint));
                }
            }
        }
        if unresolved && let Some(Hex::Rgb(colour)) = spec.value {
            candidates.push(colour);
        }
        if candidates.is_empty() {
            Paint::Bad
        } else {
            Paint::Colours(candidates)
        }
    }

    /// `w:background`'s colour as the background of every run that reaches the page: Word's
    /// print layout shows it only with `w:displayBackgroundShape`, other views either way.
    fn page_colours(&self, spec: &ColourSpec) -> Result<Vec<Rgb>, ()> {
        let mut colours = match self.paint(spec) {
            Paint::Auto => return Ok(vec![Rgb::WHITE]),
            Paint::Colours(colours) => colours,
            Paint::Bad => return Err(()),
        };
        if !self.settings.display_background {
            colours.push(Rgb::WHITE);
        }
        Ok(colours)
    }

    /// The table style level of a cell's paragraph: the table's style (else the default table
    /// style) with the regions its look turns on for the cell, in Word's order.
    fn table_level(&self, shape: &TableShape, cell: &CellAt) -> Result<TableLevel, DocError> {
        let Some(row) = shape.rows.get(cell.row) else {
            return Ok(TableLevel {
                undecided: Some(UNDECIDED),
                ..TableLevel::default()
            });
        };
        let Some(merged) = self
            .styles
            .resolve(shape.style.as_deref(), StyleKind::Table)
            .map(|id| self.styles.merged(id, StyleKind::Table))
            .transpose()?
        else {
            return Ok(TableLevel {
                undecided: shape.undecided,
                ..TableLevel::default()
            });
        };
        let table_look = shape.look.unwrap_or(Look::from_bits(WORD_TABLE_LOOK));
        let look = row.look.unwrap_or(table_look);
        let (rows, columns) = (shape.rows.len(), row.cells);
        let (last_row, last_column) = (cell.row + 1 == rows, cell.column + 1 == columns);
        let (first_row, first_column) = (cell.row == 0, cell.column == 0);
        let mut regions = vec![Region::WholeTable];
        let header_rows = usize::from(table_look.first_row);
        if !look.no_row_bands && cell.row >= header_rows {
            let band = (cell.row - header_rows) / merged.row_band as usize;
            regions.push(if band.is_multiple_of(2) {
                Region::Band1Horz
            } else {
                Region::Band2Horz
            });
        }
        let header_columns = usize::from(table_look.first_column);
        if !look.no_column_bands && cell.column >= header_columns {
            let band = (cell.column - header_columns) / merged.column_band as usize;
            regions.push(if band.is_multiple_of(2) {
                Region::Band1Vert
            } else {
                Region::Band2Vert
            });
        }
        let on = [
            (look.first_column && first_column, Region::FirstCol),
            (look.last_column && last_column, Region::LastCol),
            (look.first_row && first_row, Region::FirstRow),
            (look.last_row && last_row, Region::LastRow),
            (
                look.first_row && look.first_column && first_row && first_column,
                Region::NwCell,
            ),
            (
                look.first_row && look.last_column && first_row && last_column,
                Region::NeCell,
            ),
            (
                look.last_row && look.first_column && last_row && first_column,
                Region::SwCell,
            ),
            (
                look.last_row && look.last_column && last_row && last_column,
                Region::SeCell,
            ),
        ];
        regions.extend(
            on.into_iter()
                .filter(|(on, _)| *on)
                .map(|(_, region)| region),
        );
        let mut level = TableLevel {
            props: merged.props.clone(),
            region_shading: None,
            undecided: shape.undecided.or(merged.props.cell.undecided),
        };
        for region in regions {
            if let Some(props) = merged.regions.get(&region) {
                level.props.run.overlay(&props.run);
                level.props.para.overlay(&props.para);
                if props.cell.shading.is_some() {
                    level.region_shading = props.cell.shading;
                }
                level.undecided = level.undecided.or(props.cell.undecided);
            }
        }
        Ok(level)
    }

    fn finish_part(&mut self, mut part: Part) -> Result<(), DocError> {
        part.out.sort_by_key(|(ordinal, _, _)| *ordinal);
        part.hidden.sort_by_key(|(ordinal, _, _)| *ordinal);
        for (ordinal, why, para_id) in part.hidden {
            self.warnings.push(ExtractWarning::HiddenContent {
                at: Locator::DocxParagraph {
                    part: part.name.to_owned(),
                    ordinal,
                    para_id,
                },
                why,
            })?;
        }
        for (ordinal, text, para_id) in part.out {
            for (piece, range) in split_pieces(&text)?.into_iter().enumerate() {
                if self.blocks.len() as u64 >= u64::from(self.limits.max_blocks) {
                    return Err(DocError::TooComplex("blocks"));
                }
                self.blocks.push(Block {
                    ordinal,
                    piece: piece as u32,
                    text: replace_nul(&text[range]),
                    locator: Locator::DocxParagraph {
                        part: part.name.to_owned(),
                        ordinal,
                        para_id: para_id.clone(),
                    },
                    lines: Vec::new(),
                    cell: None,
                    unit_context: None,
                    hidden: None,
                    suspect: None,
                });
            }
        }
        Ok(())
    }
}

/// What a chain of containers puts beneath its paragraphs.
#[derive(Default)]
struct Beneath {
    level: Option<TableLevel>,
    layers: Vec<Layer>,
    whole: Option<&'static str>,
    undecided: Option<&'static str>,
    box_colour: Option<ColourSpec>,
}

/// The table style level of one cell.
#[derive(Default)]
struct TableLevel {
    props: Props,
    /// The shading of the last region that sets one.
    region_shading: Option<Shading>,
    undecided: Option<&'static str>,
}

/// The main part's root must be a transitional `w:document`; a Strict one is unsupported. The
/// part has passed `xml_guard::scan` already.
fn check_root(xml: &[u8]) -> Result<(), DocError> {
    let mut reader = xml_guard::reader(xml);
    for events in 0u64.. {
        if events % 1024 == 1023 {
            deadline::check()?;
        }
        let (resolved, event) = reader.read_resolved_event().map_err(xml_guard::map_error)?;
        match event {
            Event::Start(start) | Event::Empty(start) => {
                let namespace = match &resolved {
                    ResolveResult::Bound(ns) => ns.0,
                    _ => "",
                };
                if namespace.starts_with(STRICT) {
                    return Err(DocError::Unsupported("strict_ooxml"));
                }
                if namespace != W || start.local_name().as_ref() != "document" {
                    return Err(DocError::Malformed("docx_main_part"));
                }
                return Ok(());
            }
            Event::Eof => return Err(DocError::Malformed("docx_main_part")),
            _ => {}
        }
    }
    unreachable!("the event loop returns at the root or at Eof")
}
