//! KatWork's DOCX reader (`docx-accepted` version 1): the accepted view (tracked changes
//! accepted) of the main part, then its headers, footers, footnotes and endnotes, with hidden
//! runs excluded. Where what Word draws cannot be decided, the text counts as hidden; where
//! markup is written twice, each reading of it is judged and the text is hidden if any hides
//! it. Where a hidden run, a picture, an equation or a symbol the reader cannot decode is left
//! out between two visible stretches of a paragraph, or text Word draws is left out at either
//! end, U+FFFC stands in the gap, so no figure or date Word does not show is formed.
use std::{
    cell::{OnceCell, RefCell},
    collections::{BTreeMap, HashMap, HashSet},
    rc::Rc,
    sync::Arc,
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
const W15: &str = "http://schemas.microsoft.com/office/word/2012/wordml";
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
    W15,
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
/// The longest field type read: a longer first word names no type the reader knows, and leaves
/// the field undecided.
const MAX_FIELD_CODE: usize = 64;
/// `w:tblLook` when a table has none: Word's default (first row, first column, no column bands).
const WORD_TABLE_LOOK: u16 = 0x04A0;
/// What stands where the reader left out something Word draws: Task 1's scanners read no figure
/// or date across it, and neither `normalize_th` nor `match_key` removes it.
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
/// Fields Word draws from their code or form data rather than from the result the file holds.
const DRAWN_FROM_CODE: [&str; 9] = [
    "EQ",
    "MACROBUTTON",
    "GOTOBUTTON",
    "SYMBOL",
    "FORMDROPDOWN",
    "FORMCHECKBOX",
    "DISPLAYBARCODE",
    "MERGEBARCODE",
    "BARCODE",
];
/// Why text is hidden when it sits where Word's choice cannot be decided (an unknown namespace,
/// a markup-compatibility branch Word may take differently).
const UNKNOWN_NAMESPACE: &str = "unknown_namespace";
/// Why text is hidden when the markup around it is one Word would not read as the reader does
/// (a container written twice, a run outside a run's place, a style based on another kind).
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
    let strict = |rels: &[Relationship]| rels.iter().any(|rel| rel.kind.starts_with(STRICT));
    if strict(&root_rels) {
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
    // A Strict part named from a transitional package: the reader would pass it over.
    if strict(&rels) {
        return Err(DocError::Unsupported("strict_ooxml"));
    }

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
        props_twice: false,
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
                        // A mode that is neither leaves where the part is undecided.
                        let external = match walker.attr(&start, None, "TargetMode")?.as_deref() {
                            None | Some("Internal") => false,
                            Some("External") => true,
                            Some(_) => return Err(DocError::Malformed("docx_rels")),
                        };
                        found.push(Relationship {
                            kind,
                            target: walker.attr(&start, None, "Target")?,
                            external,
                            base: base.clone(),
                        });
                    }
                }
                depth += 1;
            }
            Item::End => depth -= 1,
            Item::Text { .. } | Item::Unread { .. } => {}
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
    /// An element of an unknown namespace, inside no other, ended; it held content or named
    /// something in an attribute, so Word may draw something where it stood (an equation's
    /// delimiter, say).
    Unread {
        skipped: bool,
    },
    Eof,
}

/// What an open element did to the walker's state, undone when it ends.
struct Frame {
    reported: bool,
    skip: bool,
    hide: bool,
    alternate: bool,
    /// An element of an unknown namespace, and whether it is inside no other.
    unknown: bool,
    outermost: bool,
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
    /// Open elements of unknown namespaces.
    unknown: u32,
    /// The outermost open one holds an element, text or a named attribute.
    unknown_content: bool,
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
            unknown: 0,
            unknown_content: false,
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
                    if let Some(item) = self.open(start, namespace)? {
                        return Ok(item);
                    }
                }
                Event::Empty(start) => {
                    if let Some(item) = self.open(start, namespace)? {
                        self.pending_end = true;
                        return Ok(item);
                    }
                    if let Some(item) = self.close() {
                        return Ok(item);
                    }
                }
                Event::End(_) => {
                    if let Some(item) = self.close() {
                        return Ok(item);
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

    /// Character data; directly in `mc:AlternateContent`, outside its branches, it is markup Word
    /// does not read there.
    fn text(&mut self, text: String) -> Item<'i> {
        self.unknown_content |= self.unknown > 0 && !text.chars().all(char::is_whitespace);
        Item::Text {
            text,
            skipped: self.skipped > 0,
            hidden: self.hidden > 0 || self.in_alternate(),
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
    fn open(
        &mut self,
        start: BytesStart<'i>,
        namespace: Option<&'static str>,
    ) -> Result<Option<Item<'i>>, DocError> {
        let reported = namespace.is_some();
        // Whatever opens inside an element of an unknown namespace is its content.
        self.unknown_content |= self.unknown > 0;
        let outermost = !reported && self.unknown == 0;
        if outermost {
            self.unknown_content = named_attribute(&start)?;
        }
        self.unknown += u32::from(!reported);
        let hide = !reported || self.in_alternate();
        self.hidden += u32::from(hide);
        self.frames.push(Frame {
            reported,
            skip: false,
            hide,
            alternate: false,
            unknown: !reported,
            outermost,
        });
        Ok(reported.then_some(Item::Start {
            start,
            namespace,
            skipped: self.skipped > 0,
            hidden: self.hidden > 0,
        }))
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
            unknown: false,
            outermost: false,
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

    /// Ends the innermost open element: `End` when it was reported, `Unread` when it is an
    /// element of an unknown namespace, inside no other, that held something.
    fn close(&mut self) -> Option<Item<'i>> {
        let frame = self.frames.pop()?;
        self.skipped -= u32::from(frame.skip);
        self.hidden -= u32::from(frame.hide);
        self.unknown -= u32::from(frame.unknown);
        if frame.alternate {
            self.alternates.pop();
        }
        if frame.reported {
            Some(Item::End)
        } else if frame.outermost && self.unknown_content {
            Some(Item::Unread {
                skipped: self.skipped > 0,
            })
        } else {
            None
        }
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

/// Whether `start` has an attribute besides namespace declarations.
fn named_attribute(start: &BytesStart) -> Result<bool, DocError> {
    for attribute in start.attributes() {
        let attribute = attribute.map_err(|_| DocError::Malformed("xml_attribute"))?;
        let key = attribute.key.into_inner();
        if key != "xmlns" && !key.starts_with("xmlns:") {
            return Ok(true);
        }
    }
    Ok(false)
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
    /// Stripes, crosses, or a value that is unknown: both of its colours show.
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
            // Word draws the fill of a `w:shd` with no pattern, as for `clear`.
            None => Pattern::Clear,
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

/// Sets `bit` in `seen`; true when it was set already: a property written twice in one place,
/// which Word's schema does not allow.
fn repeated(seen: &mut u32, bit: Option<u32>) -> bool {
    let Some(bit) = bit else {
        return false;
    };
    let mask = 1 << bit;
    let again = *seen & mask != 0;
    *seen |= mask;
    again
}

/// A style id, interned when the styles part is read. Ids the styles part does not define are
/// never interned: a reference to one names no style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct StyleId(u32);

/// The longest style id the styles part may define; a longer one is refused.
const MAX_STYLE_ID: usize = 256;

/// How the reader takes what Word's choice is not known for, each way judged: a style id defined
/// twice (`styles_first`: its first definition, else its last), a property written twice in one
/// place (`props_first`: the first value, else the last), and a table style band size that is
/// missing or 0 (`bands_off`: no banding, else bands of one). Text is hidden when any reading
/// hides it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
struct Reading {
    styles_first: bool,
    props_first: bool,
    bands_off: bool,
}

/// One value as each reading of a value written twice sees it: `first` is kept only when the
/// first value differs from the last.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Twice<T> {
    last: T,
    first: Option<T>,
}

impl<T: Copy + PartialEq> Twice<T> {
    /// Sets the value; `again` when it was written before in the same place. True when the
    /// readings now differ.
    fn set(&mut self, value: T, again: bool) -> bool {
        if again && self.first.is_none() && self.last != value {
            self.first = Some(self.last);
        }
        self.last = value;
        self.first.is_some()
    }

    fn at(&self, reading: Reading) -> T {
        match self.first {
            Some(first) if reading.props_first => first,
            _ => self.last,
        }
    }
}

/// Properties that keep, besides the last value of each, the first value of any written twice.
trait TwoReadings: Clone + PartialEq + Sized {
    fn first_mut(&mut self) -> &mut Option<Box<Self>>;
    fn first_ref(&self) -> Option<&Self>;

    /// The properties as `reading` takes them.
    fn at(&self, reading: Reading) -> &Self {
        match self.first_ref() {
            Some(first) if reading.props_first => first,
            _ => self,
        }
    }

    /// Applies `apply` to both readings, or, for a property written again (`again`), to the last
    /// only, keeping what the first reading saw. True when the readings now differ.
    fn write(&mut self, again: bool, apply: impl Fn(&mut Self)) -> bool {
        if again && self.first_ref().is_none() {
            let mut first = self.clone();
            *first.first_mut() = None;
            apply(self);
            let mut last = self.clone();
            *last.first_mut() = None;
            if last != first {
                *self.first_mut() = Some(Box::new(first));
            }
        } else {
            apply(self);
            if !again && let Some(first) = self.first_mut() {
                apply(first);
            }
        }
        self.first_ref().is_some()
    }
}

/// Run properties as one level of the cascade sets them. `undecided`: why a property sits where
/// what Word applies cannot be decided (an unknown namespace, an uncertain branch).
#[derive(Debug, Clone, Default, PartialEq)]
struct RunProps {
    style: Option<StyleId>,
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
    first: Option<Box<RunProps>>,
}

impl TwoReadings for RunProps {
    fn first_mut(&mut self) -> &mut Option<Box<Self>> {
        &mut self.first
    }

    fn first_ref(&self) -> Option<&Self> {
        self.first.as_deref()
    }
}

impl RunProps {
    fn overlay(&mut self, later: &RunProps) {
        macro_rules! later_wins {
            ($($field:ident),+) => {$(
                if later.$field.is_some() {
                    self.$field = later.$field;
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

    fn undecide(&mut self, why: &'static str) {
        self.undecided = self.undecided.or(Some(why));
        if let Some(first) = &mut self.first {
            first.undecided = first.undecided.or(Some(why));
        }
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

    /// Reads property `local`. One written twice in this element keeps both values, one per
    /// reading; true when the readings now differ.
    fn read(
        &mut self,
        walker: &Walker,
        start: &BytesStart,
        local: &str,
        seen: &mut u32,
        ids: &mut dyn FnMut(String) -> Result<Option<StyleId>, DocError>,
    ) -> Result<bool, DocError> {
        let again = repeated(seen, Self::bit(local));
        let val = || walker.w(start, "val");
        let mut value = RunProps::default();
        match local {
            "rStyle" => value.style = val()?.map(&mut *ids).transpose()?.flatten(),
            "vanish" => value.vanish = Some(Toggle::parse(val()?.as_deref())),
            "specVanish" => value.spec_vanish = Some(Toggle::parse(val()?.as_deref())),
            "webHidden" => value.web_hidden = Some(Toggle::parse(val()?.as_deref())),
            "cs" => value.cs = Some(Toggle::parse(val()?.as_deref())),
            "rtl" => value.rtl = Some(Toggle::parse(val()?.as_deref())),
            "sz" => value.size = Some(Size::parse(val()?.as_deref())),
            "szCs" => value.size_cs = Some(Size::parse(val()?.as_deref())),
            "color" => {
                value.colour = Some(ColourSpec::read(
                    walker,
                    start,
                    ["val", "themeColor", "themeTint", "themeShade"],
                )?)
            }
            "highlight" => value.highlight = Some(Highlight::parse(val()?.as_deref())),
            "shd" => value.shading = Some(Shading::read(walker, start)?),
            _ => return Ok(self.first.is_some()),
        }
        // A style the styles part does not define is written, and names none.
        let unnamed = local == "rStyle" && value.style.is_none();
        Ok(self.write(again, |props| {
            if unnamed {
                props.style = None;
            }
            props.overlay(&value);
        }))
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
struct ParaProps {
    style: Option<StyleId>,
    shading: Option<Shading>,
    undecided: Option<&'static str>,
    first: Option<Box<ParaProps>>,
}

impl TwoReadings for ParaProps {
    fn first_mut(&mut self) -> &mut Option<Box<Self>> {
        &mut self.first
    }

    fn first_ref(&self) -> Option<&Self> {
        self.first.as_deref()
    }
}

impl ParaProps {
    fn overlay(&mut self, later: &ParaProps) {
        if later.style.is_some() {
            self.style = later.style;
        }
        if later.shading.is_some() {
            self.shading = later.shading;
        }
        self.undecided = self.undecided.or(later.undecided);
    }

    fn undecide(&mut self, why: &'static str) {
        self.undecided = self.undecided.or(Some(why));
        if let Some(first) = &mut self.first {
            first.undecided = first.undecided.or(Some(why));
        }
    }

    fn read(
        &mut self,
        walker: &Walker,
        start: &BytesStart,
        local: &str,
        seen: &mut u32,
        ids: &mut dyn FnMut(String) -> Result<Option<StyleId>, DocError>,
    ) -> Result<bool, DocError> {
        let bit = ["pStyle", "shd"].iter().position(|name| *name == local);
        let again = repeated(seen, bit.map(|at| at as u32));
        let mut value = ParaProps::default();
        match local {
            "pStyle" => value.style = walker.w(start, "val")?.map(ids).transpose()?.flatten(),
            "shd" => value.shading = Some(Shading::read(walker, start)?),
            _ => return Ok(self.first.is_some()),
        }
        let unnamed = local == "pStyle" && value.style.is_none();
        Ok(self.write(again, |props| {
            if unnamed {
                props.style = None;
            }
            props.overlay(&value);
        }))
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
struct CellProps {
    shading: Option<Shading>,
    /// A `w:vMerge` or `w:hMerge` continuation, or a tracked merge into the cell above
    /// (`w:cellMerge w:vMerge="cont"`): Word lays out none of the cell's paragraphs.
    merged: bool,
    undecided: Option<&'static str>,
    first: Option<Box<CellProps>>,
}

impl TwoReadings for CellProps {
    fn first_mut(&mut self) -> &mut Option<Box<Self>> {
        &mut self.first
    }

    fn first_ref(&self) -> Option<&Self> {
        self.first.as_deref()
    }
}

impl CellProps {
    fn overlay(&mut self, later: &CellProps) {
        if later.shading.is_some() {
            self.shading = later.shading;
        }
        self.undecided = self.undecided.or(later.undecided);
    }

    fn undecide(&mut self, why: &'static str) {
        self.undecided = self.undecided.or(Some(why));
        if let Some(first) = &mut self.first {
            first.undecided = first.undecided.or(Some(why));
        }
    }

    fn read(
        &mut self,
        walker: &Walker,
        start: &BytesStart,
        local: &str,
        seen: &mut u32,
    ) -> Result<bool, DocError> {
        let bit = ["shd", "vMerge", "hMerge", "cellDel", "cellMerge"]
            .iter()
            .position(|name| *name == local);
        let again = repeated(seen, bit.map(|at| at as u32));
        // What the property sets: a shading, a merge continuation, nothing (a merge restart),
        // or a value that does not parse.
        enum Set {
            Shading(Shading),
            Continued,
            Keep,
            Bad,
        }
        let merge = |value: Option<&str>, continued: &[Option<&str>], restart: &[Option<&str>]| {
            if continued.contains(&value) {
                Set::Continued
            } else if restart.contains(&value) {
                Set::Keep
            } else {
                Set::Bad
            }
        };
        let set = match local {
            "shd" => Set::Shading(Shading::read(walker, start)?),
            "vMerge" | "hMerge" => merge(
                walker.w(start, "val")?.as_deref(),
                &[None, Some("continue")],
                &[Some("restart")],
            ),
            // A tracked change to a vertical merge: the accepted view takes its revised state.
            "cellMerge" => merge(
                walker.w(start, "vMerge")?.as_deref(),
                &[Some("cont")],
                &[None, Some("rest")],
            ),
            _ => return Ok(self.first.is_some()),
        };
        Ok(self.write(again, |props| match &set {
            Set::Shading(shading) => props.shading = Some(*shading),
            Set::Continued => props.merged = true,
            Set::Keep => {}
            Set::Bad => props.undecided = props.undecided.or(Some(UNDECIDED)),
        }))
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Props {
    run: RunProps,
    para: ParaProps,
    cell: CellProps,
}

impl Props {
    fn overlay(&mut self, later: &Props, reading: Reading) {
        self.run.overlay(later.run.at(reading));
        self.para.overlay(later.para.at(reading));
        self.cell.overlay(later.cell.at(reading));
    }

    fn undecided(&mut self, why: &'static str) {
        self.run.undecide(why);
        self.para.undecide(why);
        self.cell.undecide(why);
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

#[derive(Debug, Clone, PartialEq)]
struct Style {
    kind: StyleKind,
    based_on: Twice<Option<StyleId>>,
    props: Props,
    /// The regions that set something; `region_seen` has a bit for each region written.
    regions: Vec<(Region, Props)>,
    region_seen: u16,
    row_band: Twice<Option<u32>>,
    column_band: Twice<Option<u32>>,
}

/// A style with its `w:basedOn` chain folded in, outermost first, as one reading takes it.
#[derive(Debug, Default, PartialEq)]
struct Merged {
    props: Props,
    regions: HashMap<Region, Props>,
    /// Band sizes; 0 when none is set or it is 0, which `Reading::bands_off` reads.
    row_band: u32,
    column_band: u32,
}

/// The definitions of one style id: its last, and its first when it was defined twice
/// differently.
#[derive(Debug)]
struct Defined {
    last: Box<Style>,
    first: Option<Box<Style>>,
}

#[derive(Default)]
struct Styles {
    defaults: Props,
    ids: HashMap<Box<str>, StyleId>,
    styles: HashMap<StyleId, Defined>,
    /// The default style of each kind: by its first `w:default` and by its last.
    defaults_by_kind: HashMap<StyleKind, (StyleId, StyleId)>,
    /// A style id is defined twice differently, or two styles of a kind are its default.
    twice: bool,
    /// A property is written twice in one place with two values.
    props_twice: bool,
    /// A table style sets a band region, which a missing band size leaves undecided.
    banded: bool,
    merged: RefCell<HashMap<(StyleKind, StyleId, Reading), Rc<Merged>>>,
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
        let mut ids = std::mem::take(&mut styles.ids);
        let mut intern = |id: String| -> Result<Option<StyleId>, DocError> {
            if id.len() > MAX_STYLE_ID {
                return Err(DocError::TooComplex("style_id"));
            }
            let next = StyleId(ids.len() as u32);
            Ok(Some(*ids.entry(id.into_boxed_str()).or_insert(next)))
        };
        let mut walker = Walker::new(xml, limits, budget, document_namespace)?;
        let mut nodes: Vec<Node> = Vec::new();
        // The style being read (its id and whether it may be a default), and its region.
        let mut style: Option<(StyleId, Style, bool)> = None;
        let mut region: Option<(Region, Props)> = None;
        // The children seen so far of the defaults, the style, the region and the property
        // element being read: a container written twice is undecided, a property written twice
        // read both ways.
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
                            (Some(Node::RunDefault), "rPr") | (Some(Node::ParaDefault), "pPr") => {
                                let run = local == "rPr";
                                if repeated(&mut defaults_seen, Some(2 + u32::from(run))) {
                                    styles.defaults.undecided(UNDECIDED);
                                }
                                props_seen = 0;
                                if run {
                                    Node::Run(Into::Defaults)
                                } else {
                                    Node::Para(Into::Defaults)
                                }
                            }
                            (Some(Node::Root), "style") => {
                                let kind = match walker.w(&start, "type")?.as_deref() {
                                    None | Some("paragraph") => Some(StyleKind::Paragraph),
                                    Some("character") => Some(StyleKind::Character),
                                    Some("table") => Some(StyleKind::Table),
                                    Some("numbering") => Some(StyleKind::Numbering),
                                    Some(_) => None,
                                };
                                let id = walker
                                    .w(&start, "styleId")?
                                    .map(&mut intern)
                                    .transpose()?
                                    .flatten();
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
                                        based_on: Twice::default(),
                                        props,
                                        regions: Vec::new(),
                                        region_seen: 0,
                                        row_band: Twice::default(),
                                        column_band: Twice::default(),
                                    };
                                    (id, style, default)
                                });
                                Node::Style
                            }
                            (Some(Node::Style), "basedOn") => {
                                let base = walker
                                    .w(&start, "val")?
                                    .map(&mut intern)
                                    .transpose()?
                                    .flatten();
                                let again = repeated(&mut style_seen, Some(0));
                                if let Some((_, style, _)) = &mut style {
                                    // A base Word may not apply is never applied, never dropped:
                                    // the style is undecided.
                                    if hidden {
                                        style.props.undecided(UNKNOWN_NAMESPACE);
                                    } else {
                                        styles.props_twice |= style.based_on.set(base, again);
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
                                    styles.props_twice |= match parent {
                                        Some(Node::Run(_)) => props.run.read(
                                            &walker,
                                            &start,
                                            local,
                                            &mut props_seen,
                                            &mut intern,
                                        )?,
                                        Some(Node::Para(_)) => props.para.read(
                                            &walker,
                                            &start,
                                            local,
                                            &mut props_seen,
                                            &mut intern,
                                        )?,
                                        _ => props.cell.read(
                                            &walker,
                                            &start,
                                            local,
                                            &mut props_seen,
                                        )?,
                                    };
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
                                    } else {
                                        let band = if row {
                                            &mut style.row_band
                                        } else {
                                            &mut style.column_band
                                        };
                                        styles.props_twice |= band.set(size, again);
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
                        if let Some((id, style, default)) = style.take() {
                            styles.define(id, style, default)?;
                        }
                    }
                    Some(Node::Region) => {
                        if let (Some((found, props)), Some((_, style, _))) =
                            (region.take(), &mut style)
                        {
                            // A region written twice in one style: which Word uses is not known.
                            // One that sets nothing is not kept.
                            let bit = 1 << found as u16;
                            if style.region_seen & bit != 0 {
                                style.props.undecided(UNDECIDED);
                            } else if props != Props::default() {
                                style.regions.push((found, props));
                            }
                            style.region_seen |= bit;
                        }
                    }
                    _ => {}
                },
                Item::Text { .. } | Item::Unread { .. } => {}
                Item::Eof => {
                    styles.ids = ids;
                    return Ok(styles);
                }
            }
        }
    }

    /// Keeps a style read from the styles part. An id defined twice keeps its first definition
    /// besides its last (Word is seen to take the last whole; the standard keeps the first's
    /// id), unless the two are the same.
    fn define(&mut self, id: StyleId, style: Style, default: bool) -> Result<(), DocError> {
        let banded = [
            Region::Band1Horz,
            Region::Band2Horz,
            Region::Band1Vert,
            Region::Band2Vert,
        ]
        .iter()
        .any(|&region| style.region_seen & (1 << region as u16) != 0);
        self.banded |= banded;
        let kind = style.kind;
        match self.styles.get_mut(&id) {
            Some(defined) => {
                if *defined.last != style {
                    if defined.first.is_none() {
                        defined.first = Some(defined.last.clone());
                    }
                    *defined.last = style;
                    self.twice = true;
                }
            }
            None => {
                if self.styles.len() >= MAX_STYLES {
                    return Err(DocError::TooComplex("styles"));
                }
                self.styles.insert(
                    id,
                    Defined {
                        last: Box::new(style),
                        first: None,
                    },
                );
            }
        }
        if default {
            let entry = self.defaults_by_kind.entry(kind).or_insert((id, id));
            if entry.1 != id {
                entry.1 = id;
                self.twice = true;
            }
        }
        Ok(())
    }

    /// A style id the body names, if the styles part interned it.
    fn find(&self, id: &str) -> Option<StyleId> {
        self.ids.get(id).copied()
    }

    /// The definition of `id` that `reading` takes.
    fn style(&self, id: StyleId, reading: Reading) -> Option<&Style> {
        let defined = self.styles.get(&id)?;
        Some(match &defined.first {
            Some(first) if reading.styles_first => first,
            _ => &defined.last,
        })
    }

    /// The style `id` names when it is one of `kind`, else the default style of `kind`.
    fn resolve(&self, id: Option<StyleId>, kind: StyleKind, reading: Reading) -> Option<StyleId> {
        id.filter(|id| {
            self.style(*id, reading)
                .is_some_and(|style| style.kind == kind)
        })
        .or_else(|| {
            self.defaults_by_kind
                .get(&kind)
                .map(|&(first, last)| if reading.styles_first { first } else { last })
        })
    }

    /// `id`'s properties with its `w:basedOn` chain folded in, outermost first, as `reading`
    /// takes them. A chain stops at a cycle or a missing style; one that reaches a style of
    /// another kind is undecided, and one longer than 16 styles is `TooComplex`.
    fn merged(
        &self,
        id: StyleId,
        kind: StyleKind,
        reading: Reading,
    ) -> Result<Rc<Merged>, DocError> {
        let reading = Reading {
            bands_off: false,
            ..reading
        };
        let key = (kind, id, reading);
        if let Some(found) = self.merged.borrow().get(&key) {
            return Ok(found.clone());
        }
        deadline::check()?;
        let mut chain = Vec::new();
        let mut seen = HashSet::new();
        let mut next = Some(id);
        let mut crossed = false;
        while let Some(id) = next {
            let Some(style) = self.style(id, reading) else {
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
            next = style.based_on.at(reading);
        }
        let mut merged = Merged::default();
        for style in chain.iter().rev() {
            merged.props.overlay(&style.props, reading);
            for (region, props) in &style.regions {
                merged
                    .regions
                    .entry(*region)
                    .or_default()
                    .overlay(props, reading);
            }
            if let Some(size) = style.row_band.at(reading) {
                merged.row_band = size;
            }
            if let Some(size) = style.column_band.at(reading) {
                merged.column_band = size;
            }
        }
        if crossed {
            merged.props.undecided(UNDECIDED);
        }
        // A chain that reads the same under another reading is held once.
        let same = [false, true]
            .into_iter()
            .flat_map(|styles_first| {
                [false, true].map(|props_first| Reading {
                    styles_first,
                    props_first,
                    bands_off: false,
                })
            })
            .filter(|other| *other != reading)
            .find_map(|other| {
                self.merged
                    .borrow()
                    .get(&(kind, id, other))
                    .filter(|found| ***found == merged)
                    .cloned()
            });
        let merged = same.unwrap_or_else(|| Rc::new(merged));
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
                        // Where Word may not read it, undecided; written twice, each value is a
                        // reading, and a setting read two ways takes the one that hides more.
                        let again = repeated(&mut seen, bit.map(|at| at as u32));
                        match local {
                            // Where it is undecided, both page colours count, as when it is off.
                            "displayBackgroundShape" => {
                                let on = !hidden
                                    && Toggle::parse(walker.w(&start, "val")?.as_deref())
                                        == Toggle::On;
                                settings.display_background =
                                    on && (!again || settings.display_background);
                            }
                            "clrSchemeMapping" => {
                                for (at, (attribute, ..)) in MAPPED.iter().enumerate() {
                                    let value = walker.w(&start, attribute)?;
                                    let read = match value.as_deref().map(scheme_index) {
                                        _ if hidden => Mapping::Bad,
                                        None => Mapping::Slot(MAPPED[at].3),
                                        Some(Some(slot)) => Mapping::Slot(slot),
                                        Some(None) => Mapping::Bad,
                                    };
                                    // Two values for a name: which slot Word takes is not known.
                                    settings.mapping[at] = if again && settings.mapping[at] != read
                                    {
                                        Mapping::Bad
                                    } else {
                                        read
                                    };
                                }
                            }
                            "updateFields" => {
                                settings.update_fields |= hidden
                                    || Toggle::parse(walker.w(&start, "val")?.as_deref())
                                        != Toggle::Off;
                            }
                            _ => {}
                        }
                    }
                    depth += 1;
                }
                Item::End => depth -= 1,
                Item::Text { .. } | Item::Unread { .. } => {}
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

/// What the reader left out at a place in a paragraph: something drawn that is not text (a
/// picture, a note mark, a page number), or text Word draws or may draw. Between two visible
/// stretches either leaves `GAP`; at a paragraph's start or end only left-out text does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
enum Gap {
    #[default]
    None,
    Object,
    Text,
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
    under: Option<Rc<Link>>,
    /// Something was left out after the last kept run: the next starts after a gap.
    gap: Gap,
    /// Why content outside any run was left out (an equation, an element the reader does not
    /// read).
    dropped: Option<&'static str>,
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
    /// What was left out before the run's first kept text, and after its last.
    gap_before: Gap,
    gap_after: Gap,
    has_props: bool,
    /// The run has children besides its `w:rPr`; a `w:rPr` now is refused.
    content: bool,
}

impl Run {
    fn push(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        if self.gap_after != Gap::None {
            self.text.push(GAP);
            self.gap_after = Gap::None;
        }
        if self.text.is_empty() {
            self.text = text;
        } else {
            self.text.push_str(&text);
        }
    }

    fn gap(&mut self, gap: Gap) {
        if self.text.is_empty() {
            self.gap_before = self.gap_before.max(gap);
        } else {
            self.gap_after = self.gap_after.max(gap);
        }
    }

    fn drop_text(&mut self, why: &'static str) {
        self.gap(Gap::Text);
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
    style: Twice<Option<StyleId>>,
    look: Twice<Option<Look>>,
    /// Why a table property sits where Word's choice cannot be decided.
    undecided: Option<&'static str>,
    rows: Vec<RowShape>,
}

#[derive(Clone, Copy)]
struct RowShape {
    cells: usize,
    look: Twice<Option<Look>>,
}

struct Table {
    shape: Rc<RefCell<TableShape>>,
    row: Option<OpenRow>,
    has_props: bool,
    /// What lies beneath the table: the chain of the place it sits in.
    outer: Option<Rc<Link>>,
}

struct OpenRow {
    /// A tracked deletion of the row (`w:trPr/w:del`, `w14:conflictDel`): its content is
    /// excluded.
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
    /// The cell's link in the chain beneath its paragraphs, made for the first of them.
    link: Option<Rc<Link>>,
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
    /// Why its text cannot be shown: a hidden, out-of-place or off-page shape, a fill the
    /// reader does not read, a floating box with no fill over what the reader cannot see.
    hidden: Option<&'static str>,
    /// Its fill, or `None` when it has none and what lies beneath shows through.
    fill: Option<ColourSpec>,
    /// The shape's own text colour (`wps:style/a:fontRef`), a candidate besides the run's.
    text_colour: Option<ColourSpec>,
}

enum Container {
    Table(Table),
    /// A text box: what is inside it is not in the table around it.
    TextBox(Rc<Link>),
}

/// One container beneath a paragraph, linked to the one around it: the paragraphs of a cell or a
/// text box share one link, and a container nested in another adds one link to its chain.
struct Link {
    under: Under,
    outer: Option<Rc<Link>>,
    /// What this link and those around it put beneath a paragraph, per reading, kept once a
    /// link nested in it asked.
    outward: RefCell<Readings>,
}

impl Link {
    fn new(under: Under, outer: Option<Rc<Link>>) -> Rc<Link> {
        Rc::new(Link {
            under,
            outer,
            outward: RefCell::new(Vec::new()),
        })
    }
}

/// One thing beneath a paragraph's text.
enum Under {
    /// A table cell, with its table's shape.
    Cell(Rc<RefCell<TableShape>>, Rc<CellAt>),
    Box(Rc<BoxInfo>),
    /// The paragraph a text box sits in, and the run holding its drawing: Word lays out no
    /// object anchored in hidden text.
    Host(Rc<ParaProps>, Option<Rc<RunProps>>),
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
    /// Why a shape property sits where Word's choice cannot be decided, or why the shape is out
    /// of the place Word lays shapes out in.
    undecided: Option<&'static str>,
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
    /// A frame (`wp:inline`, `wp:anchor`), a group or canvas (whose shapes stand directly in
    /// it), a WordprocessingML shape (`wps:wsp`, which holds a text box in `wps:txbx`).
    frame: bool,
    group: bool,
    wsp: bool,
    /// The fill a VML shape has when it sets none: its `v:shapetype`'s, else VML's own white;
    /// unknown when it names a shapetype not defined before it.
    vml_default: Option<ShapeFill>,
    /// A `v:shapetype`'s id, under which its fill is kept when it ends.
    shapetype: Option<String>,
    /// A text box in it was read: a shape property after that is refused.
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

    fn undecide(&mut self, why: &'static str) {
        self.undecided = self.undecided.or(Some(why));
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
        let mut hidden = if let Some(why) = self.undecided {
            Some(why)
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

/// A field's type: the first word of its instruction, read past leading white space.
#[derive(Default)]
struct FieldKind {
    word: String,
    /// The word has ended.
    done: bool,
    /// The type cannot be read: its word is longer than any type, or a field nested in the
    /// instruction writes it.
    lost: bool,
}

impl FieldKind {
    fn read(instruction: &str) -> FieldKind {
        let mut kind = FieldKind::default();
        kind.feed(instruction);
        kind
    }

    fn feed(&mut self, text: &str) {
        for c in text.chars() {
            if self.done {
                return;
            }
            if c.is_whitespace() || c == '\\' {
                // A switch before any word leaves the field with no type.
                self.done = !self.word.is_empty() || c == '\\';
                continue;
            }
            if self.word.chars().count() == MAX_FIELD_CODE {
                self.lost = true;
                self.done = true;
                return;
            }
            self.word.push(c);
        }
    }

    /// The type, in upper case, when it can be read.
    fn name(&self) -> Option<String> {
        (!self.lost && !self.word.is_empty()).then(|| self.word.to_ascii_uppercase())
    }

    /// A type whose result Word never shows, so no display stands where the field is.
    fn never_shown(&self) -> bool {
        self.name()
            .is_some_and(|name| NEVER_SHOWN.contains(&name.as_str()))
    }
}

/// An open field.
struct Field {
    /// Still in its instruction (before `separate`).
    code: bool,
    kind: FieldKind,
    /// Why its result is not shown, once `separate` has decided it.
    hide: Option<&'static str>,
    /// `w:dirty`: Word updates it when the document opens.
    dirty: bool,
    /// A field character of it sits where Word's choice cannot be decided.
    undecided: bool,
    /// Some of its result was read, shown or not.
    result: bool,
}

/// An open content control: bound to a custom XML part, and past its properties.
#[derive(Default)]
struct Sdt {
    bound: bool,
    content: bool,
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
    /// A container whose text is dropped whole (`Part::dropping`): a data-bound control's
    /// content, an override of the reading order.
    Dropping(Level),
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
    /// `w:drawing` and `w:pict`: what they draw stands between the run's text, and only a frame
    /// in one or a VML shape in the other is laid out.
    Drawing,
    Pict,
    Shape,
    /// `a:graphic` in a frame, and its `a:graphicData`, where a drawing's shape stands.
    Graphic,
    GraphicData,
    ShapeProps,
    ShapeStyle,
    Position(usize),
    PosOffset(usize),
    /// An element whose colour child sets a shape's fill or text colour, and that colour.
    ColourHolder(Target),
    Colour(Target),
    /// `a:extLst`: extensions Word does not draw from.
    Extensions,
    /// `wps:txbx` in a `wps:wsp`, or `v:textbox` in a VML shape: where Word reads a shape's text.
    ShapeText,
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
        Node::Container(level) | Node::Dropping(level) => Some(level),
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

/// What a run element adds: text, or something Word draws that the reader does not read.
enum Put {
    Text(String),
    Gap(Gap),
}

/// One part's walk.
struct Part {
    name: Arc<str>,
    story: Story,
    ordinal: u32,
    /// Inside deleted, moved-away or embedded content.
    excluded: u32,
    body_started: bool,
    paragraphs: Vec<Option<Para>>,
    containers: Vec<Container>,
    shapes: Vec<ShapeState>,
    fields: Vec<Field>,
    /// How many fields were open when each open text box or `w:fldSimple` began: a field
    /// character inside one acts only on the fields above its floor.
    floors: Vec<usize>,
    /// The fills of the `v:shapetype`s defined so far, by id.
    shapetypes: HashMap<String, Option<ShapeFill>>,
    sdts: Vec<Sdt>,
    /// Why the text of the open content is dropped whole, innermost last.
    dropping: Vec<&'static str>,
    /// Open `a:extLst` elements.
    extensions: u32,
    /// Paragraphs of the open tables, judged once the outermost one ends. Boxed, so the queue
    /// of a large table grows by pointers rather than copies of whole paragraphs.
    #[allow(clippy::vec_box)]
    pending: Vec<Box<Para>>,
    /// The properties seen in the open `w:rPr`, `w:pPr`, `w:tcPr`, `w:trPr`, `w:tblPr` or
    /// `w:tblPrEx`.
    seen: u32,
    out: Vec<(u32, String, Option<String>)>,
    hidden: Vec<(u32, &'static str, Option<String>)>,
}

impl Part {
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
        if let Some(why) = self.dropping.last() {
            return Shown::Dropped(why);
        }
        match self.fields.iter().find_map(|field| field.hide) {
            Some(why) => Shown::Dropped(why),
            None => Shown::Yes,
        }
    }

    /// Adds a run element's text, or a gap for something Word draws, to the open run as the open
    /// fields, controls and markup allow; with no run open, a gap goes to the paragraph.
    fn put(&mut self, hidden: bool, put: Put) {
        let shown = self.shown(hidden);
        if matches!(&put, Put::Text(text) if text.is_empty()) {
            return;
        }
        if !matches!(shown, Shown::Code) {
            for field in self.fields.iter_mut().filter(|field| !field.code) {
                field.result = true;
            }
        }
        let Some(Some(para)) = self.paragraphs.last_mut() else {
            return;
        };
        match (shown, put, &mut para.run) {
            (Shown::Code, ..) => {}
            (Shown::Dropped(why), _, Some(run)) => run.drop_text(why),
            (Shown::Dropped(why), _, None) => {
                para.gap = Gap::Text;
                para.dropped = para.dropped.or(Some(why));
            }
            (Shown::Yes, Put::Text(text), Some(run)) => run.push(text),
            (Shown::Yes, Put::Text(_), None) => {}
            (Shown::Yes, Put::Gap(gap), Some(run)) => run.gap(gap),
            (Shown::Yes, Put::Gap(gap), None) => para.gap = para.gap.max(gap),
        }
    }

    /// Content of the paragraph that the reader does not read (an equation, an element of a
    /// namespace it does not understand): a gap, and why it was left out.
    fn drop_unread(&mut self) {
        if !matches!(self.shown(false), Shown::Code) {
            self.put(true, Put::Gap(Gap::Text));
        }
    }

    /// Markup inside a run's text: what the run shows around it is undecided.
    fn undecide_text(&mut self) {
        if let Some(run) = self.run() {
            run.drop_text(UNDECIDED);
        }
    }

    /// Markup inside an instruction: its field's type is undecided.
    fn undecide_instruction(&mut self) {
        if let Some(field) = self.fields.iter_mut().rev().find(|field| field.code) {
            field.undecided = true;
        }
    }

    /// Text of a field's instruction, read for the innermost field still in its instruction.
    fn instruction(&mut self, text: &str, hidden: bool) {
        let floor = self.floor();
        if let Some(field) = self.fields[floor..]
            .iter_mut()
            .rev()
            .find(|field| field.code)
        {
            field.undecided |= hidden;
            field.kind.feed(text);
        }
    }

    /// The floor of the innermost open text box or `w:fldSimple`.
    fn floor(&self) -> usize {
        self.floors.last().copied().unwrap_or(0)
    }

    /// A field character that would act on a field begun outside the text box or `w:fldSimple`
    /// it stands in: the innermost field is undecided instead.
    fn outside_floor(&mut self) -> bool {
        if self.fields.len() > self.floor() {
            return false;
        }
        if let Some(field) = self.fields.last_mut() {
            field.undecided = true;
        }
        true
    }

    /// A field starts: if the innermost open field is in its instruction and has not finished
    /// its type's word, the new field's result writes the type.
    fn nest_field(&mut self) {
        if let Some(outer) = self.fields.last_mut()
            && outer.code
            && !outer.kind.done
        {
            outer.kind.lost = true;
        }
    }

    /// A field ended. One that read no result, where Word draws one (it computes it, or the
    /// reader cannot tell), leaves a gap like a dropped run.
    fn field_ended(&mut self, field: &Field) {
        if !field.result && (field.undecided || !field.kind.never_shown()) {
            self.put(false, Put::Gap(Gap::Text));
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

    /// What lies beneath content starting now: the link of the innermost container, which the
    /// paragraphs of one cell or text box share. A cell it reaches is fixed, so its properties
    /// can no longer change.
    fn chain(&mut self) -> Option<Rc<Link>> {
        match self.containers.last_mut()? {
            Container::Table(table) => {
                let shape = table.shape.clone();
                let outer = table.outer.clone();
                match table.row.as_mut().and_then(|row| row.cell.as_mut()) {
                    // In a table, outside any cell, content is out of its place, and undecided
                    // for that.
                    None => outer,
                    Some(cell) => {
                        if cell.link.is_none() {
                            cell.link = Some(Link::new(Under::Cell(shape, cell.shared()), outer));
                        }
                        cell.link.clone()
                    }
                }
            }
            Container::TextBox(link) => Some(link.clone()),
        }
    }

    fn in_table(&self) -> bool {
        self.containers
            .iter()
            .any(|container| matches!(container, Container::Table(_)))
    }

    /// A tracked deletion of the open row: the accepted view has no such row, neither its
    /// content nor its place. True when its content is now excluded.
    fn delete_row(&mut self, hidden: bool) -> bool {
        let Some(table) = self.table() else {
            return false;
        };
        if hidden {
            let mut shape = table.shape.borrow_mut();
            shape.undecided = shape.undecided.or(Some(UNKNOWN_NAMESPACE));
            return false;
        }
        match &mut table.row {
            Some(row) if !row.deleted => {
                row.deleted = true;
                table.shape.borrow_mut().rows.pop();
                self.excluded += 1;
                true
            }
            _ => false,
        }
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
    /// A property was written twice with two values in the parts read so far.
    props_twice: bool,
    /// The last chain judged, and what it puts beneath its paragraphs under each reading.
    last_beneath: Option<(Rc<Link>, Readings)>,
}

impl Doc<'_> {
    fn read_part(&mut self, name: &str, xml: &[u8], story: Story) -> Result<(), DocError> {
        let mut walker = Walker::new(xml, self.limits, &mut self.budget, document_namespace)?;
        if story == Story::Main {
            check_root(xml)?;
        }
        let mut part = Part {
            name: Arc::from(name),
            story,
            ordinal: 0,
            excluded: 0,
            body_started: false,
            paragraphs: Vec::new(),
            containers: Vec::new(),
            shapes: Vec::new(),
            shapetypes: HashMap::new(),
            fields: Vec::new(),
            floors: Vec::new(),
            sdts: Vec::new(),
            dropping: Vec::new(),
            extensions: 0,
            pending: Vec::new(),
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
                        Some(Node::Text) => part.put(hidden, Put::Text(text)),
                        Some(Node::InstrText) => part.instruction(&text, hidden),
                        Some(&Node::PosOffset(axis)) => {
                            if let Some(shape) = part.shapes.last_mut() {
                                match text.trim().parse::<i64>() {
                                    _ if hidden => shape.undecide(UNKNOWN_NAMESPACE),
                                    Ok(offset) => shape.offset[axis] = Some(offset),
                                    Err(_) => shape.undecide(UNDECIDED),
                                }
                            }
                        }
                        // Text of an element the reader does not read is reported with it
                        // (`Item::Unread`).
                        _ => {}
                    }
                }
                Item::Unread { skipped } => {
                    if skipped || part.excluded > 0 {
                        continue;
                    }
                    let parent = nodes.last().copied();
                    match parent {
                        Some(Node::Text) => part.undecide_text(),
                        Some(Node::InstrText) => part.undecide_instruction(),
                        _ if parent == Some(Node::Run) || level(parent) == Some(Level::Run) => {
                            part.drop_unread()
                        }
                        // Between paragraphs nothing joins across it, but what it says is not
                        // read: the paragraph after it warns.
                        _ if level(parent) == Some(Level::Block) => {
                            part.hidden
                                .push((part.ordinal + 1, UNKNOWN_NAMESPACE, None));
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
        // Anything but a first `w:pPr` is the paragraph's content, and anything but a first
        // `w:rPr` the run's.
        if parent == Some(Node::Paragraph)
            && !(w && local == "pPr")
            && let Some(para) = part.para()
        {
            para.content = true;
        }
        if parent == Some(Node::Run)
            && !(w && local == "rPr")
            && let Some(run) = part.run()
        {
            run.content = true;
        }
        // Markup inside text, which Word does not read there: what the run shows around it is
        // undecided, and inside an instruction its field's type is.
        match parent {
            Some(Node::Text) => {
                part.undecide_text();
                return Ok(Node::Other);
            }
            Some(Node::InstrText) => {
                part.undecide_instruction();
                return Ok(Node::Other);
            }
            _ => {}
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
                if part.body_started {
                    return Err(DocError::Malformed("docx_structure"));
                }
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
                    gap: Gap::None,
                    dropped: None,
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
                part.put(hidden, Put::Text("\t".into()));
                Node::Other
            }
            (Some(Node::Run), "br" | "cr") => {
                part.put(hidden, Put::Text("\n".into()));
                Node::Other
            }
            (Some(Node::Run), "noBreakHyphen") => {
                part.put(hidden, Put::Text("-".into()));
                Node::Other
            }
            (Some(Node::Run), "sym") => {
                let font = walker.w(start, "font")?;
                let code = walker.w(start, "char")?;
                part.put(hidden, symbol_put(font.as_deref(), code.as_deref()));
                Node::Other
            }
            (Some(Node::Run), "drawing" | "pict") => {
                part.put(hidden, Put::Gap(Gap::Object));
                if local == "drawing" {
                    Node::Drawing
                } else {
                    Node::Pict
                }
            }
            // Ruby and date blocks: text Word draws there.
            (
                Some(Node::Run),
                "ruby" | "dayShort" | "dayLong" | "monthShort" | "monthLong" | "yearShort"
                | "yearLong",
            ) => {
                part.put(hidden, Put::Gap(Gap::Text));
                part.excluded += 1;
                Node::Excluded
            }
            // Note marks, page numbers and separators: something Word draws there.
            (
                Some(Node::Run),
                "footnoteReference"
                | "endnoteReference"
                | "footnoteRef"
                | "endnoteRef"
                | "pgNum"
                | "separator"
                | "continuationSeparator"
                | "contentPart",
            ) => {
                part.put(hidden, Put::Gap(Gap::Object));
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
                    // Properties after the run's content: the reader has read that content
                    // before them, as Word does not.
                    if run.content {
                        return Err(DocError::Malformed("docx_structure"));
                    }
                    if run.has_props {
                        run.props.undecide(UNDECIDED);
                    }
                    if hidden {
                        run.props.undecide(UNKNOWN_NAMESPACE);
                    }
                    run.has_props = true;
                }
                part.seen = 0;
                Node::RunProps
            }
            (Some(Node::RunProps), _) => {
                let mut seen = part.seen;
                let styles = &self.styles;
                let mut twice = false;
                if let Some(run) = part.run() {
                    if hidden {
                        run.props.undecide(UNKNOWN_NAMESPACE);
                    }
                    twice = run.props.read(walker, start, local, &mut seen, &mut |id| {
                        Ok(styles.find(&id))
                    })?;
                }
                self.props_twice |= twice;
                part.seen = seen;
                Node::Other
            }
            (Some(Node::Paragraph), "pPr") => {
                if let Some(para) = part.para() {
                    if para.content {
                        return Err(DocError::Malformed("docx_structure"));
                    }
                    if para.has_props {
                        para.props.undecide(UNDECIDED);
                    }
                    if hidden {
                        para.props.undecide(UNKNOWN_NAMESPACE);
                    }
                    para.has_props = true;
                }
                part.seen = 0;
                Node::ParaProps
            }
            (Some(Node::ParaProps), _) => {
                let mut seen = part.seen;
                let styles = &self.styles;
                let mut twice = false;
                if let Some(para) = part.para() {
                    if hidden {
                        para.props.undecide(UNKNOWN_NAMESPACE);
                    }
                    twice = para
                        .props
                        .read(walker, start, local, &mut seen, &mut |id| {
                            Ok(styles.find(&id))
                        })?;
                }
                self.props_twice |= twice;
                part.seen = seen;
                Node::Other
            }
            (Some(Node::RowProps), "del") => {
                part.delete_row(hidden);
                Node::Other
            }
            (_, "del" | "moveFrom") => {
                part.excluded += 1;
                Node::Excluded
            }
            (_, "object") => {
                part.put(hidden, Put::Gap(Gap::Object));
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
                let info = match (parent, part.shapes.last()) {
                    (Some(Node::ShapeText), Some(shape)) => {
                        let info = shape.box_info();
                        // Every shape around it is fixed now.
                        for shape in &mut part.shapes {
                            shape.text_read = true;
                        }
                        info
                    }
                    // Text outside a shape's text box: where Word draws it, if at all, is not
                    // known.
                    _ => BoxInfo {
                        hidden: Some(UNDECIDED),
                        fill: None,
                        text_colour: None,
                    },
                };
                let host = match part.paragraphs.last_mut() {
                    Some(Some(host)) => Some((
                        host.shared
                            .get_or_insert_with(|| Rc::new(host.props.clone()))
                            .clone(),
                        host.run.as_ref().map(|run| Rc::new(run.props.clone())),
                        host.under.clone(),
                    )),
                    _ => None,
                };
                let outer = match host {
                    Some((props, run, under)) => Some(Link::new(Under::Host(props, run), under)),
                    None => part.chain(),
                };
                part.paragraphs.push(None);
                part.floors.push(part.fields.len());
                part.containers.push(Container::TextBox(Link::new(
                    Under::Box(Rc::new(info)),
                    outer,
                )));
                Node::TextBox
            }
            (_, "sdt") => {
                part.sdts.push(Sdt::default());
                Node::Sdt(level(parent))
            }
            (Some(Node::Sdt(_)), "sdtPr") => {
                // Properties after the control's content, which the reader read unbound.
                if part.sdts.last().is_some_and(|sdt| sdt.content) {
                    return Err(DocError::Malformed("docx_structure"));
                }
                Node::SdtProps
            }
            (Some(Node::SdtProps), "dataBinding") => {
                bind(part);
                Node::Other
            }
            (Some(Node::Sdt(level)), "sdtContent") => {
                let bound = match part.sdts.last_mut() {
                    Some(sdt) => {
                        sdt.content = true;
                        sdt.bound
                    }
                    None => false,
                };
                match (level, bound) {
                    (Some(level), true) => {
                        part.dropping.push("data_binding");
                        Node::Dropping(level)
                    }
                    (Some(level), false) => Node::Container(level),
                    (None, _) => Node::Other,
                }
            }
            (_, "customXml") => level(parent).map_or(Node::Other, Node::Container),
            // A right-to-left override: Word draws its text in reverse order.
            (_, "bdo") if level(parent) == Some(Level::Run) => {
                if walker.w(start, "val")?.as_deref() == Some("rtl") {
                    part.dropping.push("reversed");
                    Node::Dropping(Level::Run)
                } else {
                    Node::Container(Level::Run)
                }
            }
            (_, "hyperlink" | "smartTag" | "dir" | "ins" | "moveTo")
                if level(parent) == Some(Level::Run) =>
            {
                Node::Container(Level::Run)
            }
            (_, "fldSimple") if level(parent) == Some(Level::Run) => {
                if part.fields.len() as u64 >= u64::from(self.limits.max_xml_depth) {
                    return Err(DocError::TooComplex("field_depth"));
                }
                let kind = FieldKind::read(&walker.w(start, "instr")?.unwrap_or_default());
                let dirty = walker
                    .w(start, "dirty")?
                    .is_some_and(|value| Toggle::parse(Some(&value)) != Toggle::Off);
                part.nest_field();
                let depth = part.fields.len();
                part.fields.push(Field {
                    code: false,
                    hide: self.field_hide(&kind, dirty, part.story),
                    kind,
                    dirty,
                    undecided: hidden,
                    result: false,
                });
                part.floors.push(part.fields.len());
                Node::SimpleField(depth)
            }
            (_, "tbl") => {
                let outer = part.chain();
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
                    outer,
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
                    .map(|id| id.and_then(|id| self.styles.find(&id)));
                let look = (local == "tblLook")
                    .then(|| Look::read(walker, start))
                    .transpose()?;
                let again = repeated(&mut part.seen, Some(u32::from(local == "tblLook")));
                let mut twice = false;
                if let Some(table) = part.table() {
                    let mut shape = table.shape.borrow_mut();
                    if hidden {
                        shape.undecided = shape.undecided.or(Some(UNKNOWN_NAMESPACE));
                    }
                    if let Some(style) = style {
                        twice |= shape.style.set(style, again);
                    }
                    if let Some(look) = look {
                        twice |= shape.look.set(Some(look), again);
                    }
                }
                self.props_twice |= twice;
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
                        look: Twice::default(),
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
            (Some(Node::Row), "tblPrEx") => {
                part.seen = 0;
                Node::RowExceptions
            }
            (Some(Node::RowExceptions), "tblLook") => {
                let look = Look::read(walker, start)?;
                let again = repeated(&mut part.seen, Some(0));
                let mut twice = false;
                if let Some(table) = part.table() {
                    let mut shape = table.shape.borrow_mut();
                    if hidden {
                        shape.undecided = shape.undecided.or(Some(UNKNOWN_NAMESPACE));
                    }
                    if let Some(row) = shape.rows.last_mut() {
                        twice = row.look.set(Some(look), again);
                    }
                }
                self.props_twice |= twice;
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
                        link: None,
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
                        cell.at.props.undecide(UNDECIDED);
                    }
                    cell.has_props = true;
                }
                part.seen = 0;
                Node::CellProps
            }
            (Some(Node::CellProps), _) => {
                let mut seen = part.seen;
                let mut deleted = false;
                let mut twice = false;
                if let Some(cell) = part.cell() {
                    if hidden {
                        cell.at.props.undecide(UNKNOWN_NAMESPACE);
                    }
                    twice = cell.at.props.read(walker, start, local, &mut seen)?;
                    if local == "cellDel" && !hidden && !cell.deleted {
                        cell.deleted = true;
                        deleted = true;
                    }
                }
                self.props_twice |= twice;
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

    /// Elements outside WordprocessingML: OLE objects, Word's symbol, binding and conflict
    /// extensions, and the frames, shapes and fills that decide what a text box's text lies on.
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
        // Extensions of shape properties (Word keeps a fill it does not draw there): passed
        // over whole.
        if part.extensions > 0 {
            return Ok(Node::Other);
        }
        if (namespace, local) == (DRAWINGML, "extLst") {
            part.extensions += 1;
            return Ok(Node::Extensions);
        }
        // A shape property where Word's choice cannot be decided leaves the shape undecided.
        if hidden && let Some(shape) = part.shapes.last_mut() {
            shape.undecide(UNKNOWN_NAMESPACE);
        }
        // A frame's properties after its text box was read, which the reader judged without them.
        let late = |part: &Part| {
            if part.shapes.last().is_some_and(|shape| shape.text_read) {
                Err(DocError::Malformed("docx_structure"))
            } else {
                Ok(())
            }
        };
        let toggle = |value: Option<String>| value.map(|value| Toggle::parse(Some(&value)));
        // VML writes its booleans `t` and `f`.
        let vml_toggle = |value: Option<String>| {
            value.map(|value| match value.as_str() {
                "t" | "true" => Toggle::On,
                "f" | "false" => Toggle::Off,
                _ => Toggle::Bad,
            })
        };
        Ok(match (namespace, local, parent) {
            (OFFICE, "OLEObject", _) => {
                self.warnings
                    .once(ExtractWarning::EmbeddedContentIgnored { what: "ole_object" })?;
                part.excluded += 1;
                Node::Excluded
            }
            (W16SE, "symEx", Some(Node::Run)) => {
                let font = walker.attr(start, Some(W16SE), "font")?;
                let code = walker.attr(start, Some(W16SE), "char")?;
                part.put(hidden, symbol_put(font.as_deref(), code.as_deref()));
                Node::Other
            }
            (W15, "dataBinding", Some(Node::SdtProps)) => {
                bind(part);
                Node::Other
            }
            (W14, "conflictDel", Some(Node::RowProps)) => {
                part.delete_row(hidden);
                Node::Other
            }
            (WP, "inline" | "anchor", _) => {
                let mut shape = ShapeState::inside(part.shapes.last());
                shape.anchored = local == "anchor";
                if hidden {
                    shape.undecide(UNKNOWN_NAMESPACE);
                }
                shape.frame = true;
                // A frame outside `w:drawing`, which Word does not lay out there.
                if parent != Some(Node::Drawing) {
                    shape.undecide(UNDECIDED);
                }
                match toggle(walker.attr(start, None, "simplePos")?) {
                    Some(Toggle::On) => shape.simple = true,
                    Some(Toggle::Bad) => shape.undecide(UNDECIDED),
                    _ => {}
                }
                part.shapes.push(shape);
                Node::Shape
            }
            (WP, "docPr", _) | (WPS | WPG | WPC, "cNvPr", _) => {
                late(part)?;
                if let Some(shape) = part.shapes.last_mut() {
                    match toggle(walker.attr(start, None, "hidden")?) {
                        Some(Toggle::On) => shape.hidden = true,
                        Some(Toggle::Bad) => shape.undecide(UNDECIDED),
                        _ => {}
                    }
                }
                Node::Other
            }
            (WP, "simplePos", _) => {
                late(part)?;
                if let Some(shape) = part.shapes.last_mut()
                    && shape.simple
                {
                    for (axis, name) in ["x", "y"].into_iter().enumerate() {
                        match walker.attr(start, None, name)?.map(|v| v.parse::<i64>()) {
                            Some(Ok(offset)) => shape.offset[axis] = Some(offset),
                            Some(Err(_)) => shape.undecide(UNDECIDED),
                            None => {}
                        }
                    }
                }
                Node::Other
            }
            (WP, "positionH" | "positionV", _) => {
                late(part)?;
                Node::Position(usize::from(local == "positionV"))
            }
            (WP, "posOffset", Some(Node::Position(axis))) => {
                if part.shapes.last().is_some_and(|shape| shape.simple) {
                    Node::Other
                } else {
                    Node::PosOffset(axis)
                }
            }
            (WP, "extent", _) => {
                late(part)?;
                if let Some(shape) = part.shapes.last_mut() {
                    for (axis, name) in ["cx", "cy"].into_iter().enumerate() {
                        match walker.attr(start, None, name)?.map(|v| v.parse::<i64>()) {
                            Some(Ok(extent)) => shape.extent[axis] = extent,
                            Some(Err(_)) => shape.undecide(UNDECIDED),
                            None => {}
                        }
                    }
                }
                Node::Other
            }
            (DRAWINGML, "graphic", Some(Node::Shape))
                if part.shapes.last().is_some_and(|shape| shape.frame) =>
            {
                Node::Graphic
            }
            (DRAWINGML, "graphicData", Some(Node::Graphic)) => Node::GraphicData,
            (WPS, "wsp", _) | (WPG, "wgp" | "grpSp", _) | (WPC, "wpc", _) => {
                // A shape stands in its frame's graphic, or in a group or canvas; elsewhere Word
                // does not lay it out.
                let framed = match parent {
                    Some(Node::GraphicData) => true,
                    Some(Node::Shape) => part
                        .shapes
                        .last()
                        .is_some_and(|outer| outer.group && !outer.vml),
                    _ => false,
                };
                let mut shape = ShapeState::inside(part.shapes.last());
                shape.group = local != "wsp";
                shape.wsp = local == "wsp";
                if hidden {
                    shape.undecide(UNKNOWN_NAMESPACE);
                }
                if !framed {
                    shape.undecide(UNDECIDED);
                }
                part.shapes.push(shape);
                Node::Shape
            }
            (WPS, "txbx", Some(Node::Shape))
                if part.shapes.last().is_some_and(|shape| shape.wsp) =>
            {
                Node::ShapeText
            }
            (VML, "textbox", Some(Node::Shape))
                if part
                    .shapes
                    .last()
                    .is_some_and(|shape| shape.vml && !shape.group) =>
            {
                Node::ShapeText
            }
            (WPS, "spPr", _) | (WPG, "grpSpPr", _) => {
                late(part)?;
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
                late(part)?;
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
                let in_group = parent == Some(Node::Shape)
                    && part
                        .shapes
                        .last()
                        .is_some_and(|outer| outer.vml && outer.group);
                let mut shape = ShapeState::inside(part.shapes.last());
                shape.vml = true;
                shape.group = local == "group";
                if hidden {
                    shape.undecide(UNKNOWN_NAMESPACE);
                }
                // A VML shape outside `w:pict` or a VML group, which Word does not lay out there.
                if parent != Some(Node::Pict) && !in_group {
                    shape.undecide(UNDECIDED);
                }
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
                let filled = vml_toggle(walker.attr(start, None, "filled")?);
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
                late(part)?;
                if let Some(shape) = part.shapes.last_mut() {
                    let on = vml_toggle(walker.attr(start, None, "on")?);
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
                part.nest_field();
                part.fields.push(Field {
                    code: true,
                    kind: FieldKind::default(),
                    hide: None,
                    dirty,
                    undecided: hidden,
                    result: false,
                });
            }
            Some("separate") => {
                if !part.outside_floor()
                    && let Some(field) = part.fields.last_mut()
                {
                    if hidden || !field.code {
                        field.undecided = true;
                    } else {
                        field.hide = self.field_hide(&field.kind, field.dirty, part.story);
                    }
                    field.code = false;
                }
            }
            Some("end") => {
                if hidden {
                    if let Some(field) = part.fields.last_mut() {
                        field.undecided = true;
                    }
                } else if !part.outside_floor()
                    && let Some(field) = part.fields.pop()
                {
                    part.field_ended(&field);
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

    /// Why a field's result is not what Word shows: a type the reader cannot read, a field Word
    /// never shows, one it computes as it lays the document out or opens it, one it draws from
    /// its code, any field of a header or footer (recomputed on every page), and any field Word
    /// updates on open (`w:updateFields`, `w:dirty`). A hyperlink keeps its text when updated.
    fn field_hide(&self, kind: &FieldKind, dirty: bool, story: Story) -> Option<&'static str> {
        let Some(kind) = kind.name() else {
            return Some(UNDECIDED);
        };
        if kind == "HYPERLINK" {
            return None;
        }
        let kind = kind.as_str();
        (NEVER_SHOWN.contains(&kind)
            || LAYOUT_COMPUTED.contains(&kind)
            || DRAWN_FROM_CODE.contains(&kind)
            || story == Story::HeaderFooter
            || self.settings.update_fields
            || dirty)
            .then_some("field_result")
    }

    fn close(&mut self, part: &mut Part, node: Node) -> Result<(), DocError> {
        match node {
            Node::Paragraph => {
                if let Some(Some(para)) = part.paragraphs.pop() {
                    // A paragraph whose kept runs hold only white space, none of it dropped,
                    // shows nothing and warns of nothing.
                    let quiet = para.dropped.is_none()
                        && para.runs.iter().all(|run| {
                            run.dropped.is_none() && run.text.chars().all(char::is_whitespace)
                        });
                    if quiet {
                    } else if part.in_table() {
                        // Each queued paragraph makes a block or a warning, and there may be
                        // `max_blocks` of each: more cannot be read.
                        let made = part.pending.len()
                            + part.out.len()
                            + part.hidden.len()
                            + self.blocks.len()
                            + self.warnings.list.len();
                        if made as u64 >= 2 * u64::from(self.limits.max_blocks) {
                            return Err(DocError::TooComplex("blocks"));
                        }
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
                        run.gap_before = run.gap_before.max(para.gap);
                        para.gap = Gap::None;
                        para.runs.push(*run);
                    } else {
                        para.gap = para.gap.max(run.gap_before).max(run.gap_after);
                    }
                }
            }
            Node::Excluded => part.excluded -= 1,
            Node::Extensions => part.extensions -= 1,
            Node::TextBox => {
                part.paragraphs.pop();
                part.containers.pop();
                // A field begun in the box ends with its story.
                if let Some(floor) = part.floors.pop() {
                    part.fields.truncate(floor);
                }
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
            Node::Dropping(_) => {
                part.dropping.pop();
            }
            Node::SimpleField(depth) => {
                part.floors.pop();
                // A field begun inside it and not ended there stays open, its end still to come,
                // and undecided.
                if depth < part.fields.len() {
                    let field = part.fields.remove(depth);
                    for inner in &mut part.fields[depth..] {
                        inner.undecided = true;
                    }
                    part.field_ended(&field);
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// Marks the open content control as bound to a custom XML part: Word refills it from that part
/// when the document opens.
fn bind(part: &mut Part) {
    if let Some(sdt) = part.sdts.last_mut() {
        sdt.bound = true;
    }
}

/// What a `w:sym` or `w16se:symEx` draws: its character when the reader can tell, else a gap.
fn symbol_put(font: Option<&str>, code: Option<&str>) -> Put {
    match symbol(font, code) {
        Some(symbol) => Put::Text(symbol.into()),
        None => Put::Gap(Gap::Text),
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
                    None => shape.undecide(UNDECIDED),
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

/// Why the colours beneath a run cannot be listed: one does not parse, or there are more
/// candidates than `MAX_CANDIDATES` (the run is then undecided), or the job must stop.
#[derive(Debug, Clone)]
enum Unlisted {
    Paint,
    Many,
    Stop(DocError),
}

/// The candidate colours one place may show: past this many, what Word draws there is left
/// undecided rather than listed.
const MAX_CANDIDATES: usize = 64;

/// Adds `colour` to `colours` unless it is there; too many is `Unlisted::Many`.
fn add_candidate(colours: &mut Vec<Rgb>, colour: Rgb) -> Result<(), Unlisted> {
    if !colours.contains(&colour) {
        if colours.len() == MAX_CANDIDATES {
            return Err(Unlisted::Many);
        }
        colours.push(colour);
    }
    Ok(())
}

/// What a link and the links around it put beneath a paragraph, as one reading takes it.
struct Outward {
    /// A cell's table level: what its table style gives the cell's paragraphs.
    level: Option<TableLevel>,
    /// This link's own layers, nearest first.
    layers: Vec<Layer>,
    outer: Option<Rc<Outward>>,
    /// Why every run beneath it is hidden, and why every run beneath it is undecided: this
    /// link's reason, else the nearest outer one's.
    whole: Option<&'static str>,
    undecided: Option<&'static str>,
    /// A text box's own text colour.
    box_colour: Option<ColourSpec>,
    /// The colours beneath `layers` and the links around them, once a run asked.
    colours: OnceCell<Result<Vec<Rgb>, Unlisted>>,
}

/// What a link puts beneath a paragraph, for each reading worked out so far.
type Readings = Vec<(Reading, Rc<Outward>)>;

/// One reading of a paragraph: what lies beneath it, and its own effective properties.
struct View {
    reading: Reading,
    beneath: Option<Rc<Outward>>,
    props: ParaProps,
    style: Option<Rc<Merged>>,
}

impl View {
    fn level(&self) -> Option<&TableLevel> {
        self.beneath.as_ref()?.level.as_ref()
    }
}

impl Doc<'_> {
    /// Every reading the parts read so far leave open, the last-wins reading first.
    fn readings(&self) -> Vec<Reading> {
        let props = self.props_twice || self.styles.props_twice;
        let mut readings = Vec::with_capacity(8);
        for styles_first in [false, true] {
            for props_first in [false, true] {
                for bands_off in [false, true] {
                    if (styles_first && !self.styles.twice)
                        || (props_first && !props)
                        || (bands_off && !self.styles.banded)
                    {
                        continue;
                    }
                    readings.push(Reading {
                        styles_first,
                        props_first,
                        bands_off,
                    });
                }
            }
        }
        readings
    }

    /// A paragraph's effective properties: the defaults, its cell's table level, its style and
    /// its own (as `reading` takes them); with the merged paragraph style.
    fn para_props(
        &self,
        own: &ParaProps,
        level: Option<&TableLevel>,
        reading: Reading,
    ) -> Result<(ParaProps, Option<Rc<Merged>>), DocError> {
        let styles = &self.styles;
        let style = styles
            .resolve(own.style, StyleKind::Paragraph, reading)
            .map(|id| styles.merged(id, StyleKind::Paragraph, reading))
            .transpose()?;
        let mut props = ParaProps::default();
        props.overlay(styles.defaults.para.at(reading));
        if let Some(level) = level {
            props.overlay(&level.props.para);
        }
        if let Some(style) = &style {
            props.overlay(&style.props.para);
        }
        props.overlay(own);
        Ok((props, style))
    }

    /// A run's properties under `reading`: the defaults, then the given levels, then its own.
    fn effective(
        &self,
        own: &RunProps,
        levels: &[Option<&RunProps>; 3],
        reading: Reading,
    ) -> RunProps {
        let mut effective = RunProps::default();
        effective.overlay(self.styles.defaults.run.at(reading));
        for props in levels.iter().flatten() {
            effective.overlay(props);
        }
        effective.overlay(own);
        effective
    }

    /// What `link` and the links around it put beneath a paragraph under `reading`: a cell's
    /// table level and shading (table style formatting reaches a paragraph only in its own cell),
    /// a text box's fill and visibility, an inline box's host paragraph and run. A link nested
    /// in another keeps the outer one's (`keep`), so each is worked out once per reading.
    fn outward(&self, link: &Link, reading: Reading, keep: bool) -> Result<Rc<Outward>, DocError> {
        if let Some((_, found)) = link
            .outward
            .borrow()
            .iter()
            .find(|(seen, _)| *seen == reading)
        {
            return Ok(found.clone());
        }
        deadline::check()?;
        let outer = link
            .outer
            .as_deref()
            .map(|outer| self.outward(outer, reading, true))
            .transpose()?;
        let mut out = Outward {
            level: None,
            layers: Vec::new(),
            outer,
            whole: None,
            undecided: None,
            box_colour: None,
            colours: OnceCell::new(),
        };
        match &link.under {
            Under::Cell(shape, cell) => {
                let level = self.table_level(&shape.borrow(), cell, reading)?;
                let props = cell.props.at(reading);
                if props.merged {
                    out.whole = Some("merged_cell");
                }
                out.undecided = props.undecided.or(level.undecided);
                out.layers.extend(props.shading.map(Layer::Shading));
                out.layers.extend(level.region_shading.map(Layer::Shading));
                out.layers
                    .extend(level.props.cell.shading.map(Layer::Shading));
                out.level = Some(level);
            }
            Under::Box(info) => {
                out.whole = info.hidden;
                out.box_colour = info.text_colour;
                if let Some(fill) = &info.fill {
                    out.layers.push(Layer::Fill(match self.paint(fill) {
                        Paint::Colours(colours) => Ok(colours),
                        Paint::Auto | Paint::Bad => Err(()),
                    }));
                }
            }
            Under::Host(props, run) => {
                // The host paragraph sits in what lies beneath it: its cell's level, if any.
                let level = out.outer.as_ref().and_then(|outer| outer.level.as_ref());
                let (host, style) = self.para_props(props.at(reading), level, reading)?;
                out.undecided = host.undecided;
                out.layers.extend(host.shading.map(Layer::Shading));
                if let Some(run) = run {
                    let own = run.at(reading);
                    let char_style = self
                        .styles
                        .resolve(own.style, StyleKind::Character, reading)
                        .map(|id| self.styles.merged(id, StyleKind::Character, reading))
                        .transpose()?;
                    let levels = [
                        level.map(|level| &level.props.run),
                        style.as_ref().map(|style| &style.props.run),
                        char_style.as_ref().map(|style| &style.props.run),
                    ];
                    let effective = self.effective(own, &levels, reading);
                    out.whole = if self.vanished(own, &levels, &effective, reading) {
                        Some("vanish")
                    } else {
                        effective.undecided
                    };
                }
            }
        }
        if let Some(outer) = &out.outer {
            out.whole = out.whole.or(outer.whole);
            out.undecided = out.undecided.or(outer.undecided);
        }
        let out = Rc::new(out);
        if keep {
            link.outward.borrow_mut().push((reading, out.clone()));
        }
        Ok(out)
    }

    /// What lies beneath the paragraphs of `under` under `reading`; the paragraphs of one cell
    /// or text box share it, worked out once.
    fn beneath(
        &mut self,
        under: Option<&Rc<Link>>,
        reading: Reading,
    ) -> Result<Option<Rc<Outward>>, DocError> {
        let Some(link) = under else {
            return Ok(None);
        };
        if let Some((last, seen)) = &self.last_beneath
            && Rc::ptr_eq(last, link)
            && let Some((_, found)) = seen.iter().find(|(seen, _)| *seen == reading)
        {
            return Ok(Some(found.clone()));
        }
        let found = self.outward(link, reading, false)?;
        match &mut self.last_beneath {
            Some((last, seen)) if Rc::ptr_eq(last, link) => seen.push((reading, found.clone())),
            last => *last = Some((link.clone(), vec![(reading, found.clone())])),
        }
        Ok(Some(found))
    }

    /// Judges a finished paragraph's runs under every reading and records its visible text and
    /// hidden reason. Where text was left out between two visible stretches, or text Word draws
    /// was left out at either end, `GAP` stands in its place.
    fn judge(&mut self, part: &mut Part, para: Para) -> Result<(), DocError> {
        deadline::check()?;
        let mut views = Vec::new();
        for reading in self.readings() {
            let beneath = self.beneath(para.under.as_ref(), reading)?;
            let level = beneath.as_ref().and_then(|beneath| beneath.level.as_ref());
            let (props, style) = self.para_props(para.props.at(reading), level, reading)?;
            views.push(View {
                reading,
                beneath,
                props,
                style,
            });
        }
        let whole = views
            .iter()
            .find_map(|view| view.beneath.as_ref().and_then(|beneath| beneath.whole));

        let mut text = String::new();
        let mut why = whole.or(para.dropped);
        let mut gap = Gap::None;
        if whole.is_none() {
            for (at, run) in para.runs.into_iter().enumerate() {
                if at % 256 == 255 {
                    deadline::check()?;
                }
                let mut hidden = None;
                if !run.text.is_empty() {
                    for view in &views {
                        hidden = self.run_hidden(view, &run)?;
                        if hidden.is_some() {
                            break;
                        }
                    }
                }
                match hidden {
                    Some(reason) => {
                        why = why.or(Some(reason));
                        gap = Gap::Text;
                    }
                    None => {
                        why = why.or(run.dropped);
                        gap = gap.max(run.gap_before);
                        if run.text.is_empty() {
                            continue;
                        }
                        // Neighbours never join across what was left out, and a stretch never
                        // starts where text was left out before it.
                        if gap == Gap::Text || (gap == Gap::Object && !text.is_empty()) {
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
            if gap.max(para.gap) == Gap::Text && !text.is_empty() {
                text.push(GAP);
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

    /// Why a run with text is hidden under one reading of its paragraph, if it is.
    fn run_hidden(&self, view: &View, run: &Run) -> Result<Option<&'static str>, DocError> {
        let reading = view.reading;
        let styles = &self.styles;
        let own = run.props.at(reading);
        let char_style = styles
            .resolve(own.style, StyleKind::Character, reading)
            .map(|id| styles.merged(id, StyleKind::Character, reading))
            .transpose()?;
        let levels = [
            view.level().map(|level| &level.props.run),
            view.style.as_ref().map(|style| &style.props.run),
            char_style.as_ref().map(|style| &style.props.run),
        ];
        let effective = self.effective(own, &levels, reading);
        if self.vanished(own, &levels, &effective, reading) {
            return Ok(Some("vanish"));
        }
        if self.tiny(&effective, &run.text) {
            return Ok(Some("tiny"));
        }
        if let Some(why) = self.low_contrast(&effective, own, view)? {
            return Ok(Some(why));
        }
        let beneath = view.beneath.as_ref().and_then(|beneath| beneath.undecided);
        Ok(view.props.undecided.or(beneath).or(effective.undecided))
    }

    /// `w:vanish` is a toggle (ECMA-376 Part 1 §17.7.3): the run's own value wins; otherwise the
    /// run is hidden when either reading of the toggle rule hides it. `w:specVanish` and
    /// `w:webHidden` hide too (over-hiding is the safe direction).
    fn vanished(
        &self,
        direct: &RunProps,
        levels: &[Option<&RunProps>; 3],
        effective: &RunProps,
        reading: Reading,
    ) -> bool {
        let defaults = self.styles.defaults.run.at(reading).vanish;
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

    /// `low_contrast` when a candidate colour falls below `min_contrast` against a background
    /// that may lie beneath, `undecided` when the backgrounds are too many to list. In a shape,
    /// the shape's own text colour counts too wherever the run itself sets none.
    fn low_contrast(
        &self,
        effective: &RunProps,
        direct: &RunProps,
        view: &View,
    ) -> Result<Option<&'static str>, DocError> {
        const LOW: Option<&str> = Some("low_contrast");
        let mut colours = match effective.colour.as_ref().map(|spec| self.paint(spec)) {
            None | Some(Paint::Auto) => Vec::new(),
            Some(Paint::Bad) => return Ok(LOW),
            Some(Paint::Colours(colours)) => colours,
        };
        let own_colour = direct
            .colour
            .as_ref()
            .is_some_and(|spec| !matches!(self.paint(spec), Paint::Auto));
        let box_colour = view
            .beneath
            .as_ref()
            .and_then(|beneath| beneath.box_colour.as_ref());
        if !own_colour && let Some(spec) = box_colour {
            match self.paint(spec) {
                Paint::Colours(more) => colours.extend(more),
                Paint::Bad => return Ok(LOW),
                Paint::Auto => {}
            }
        }
        // Automatic text takes `readable_on` of its background, never low-contrast by itself.
        if colours.is_empty() {
            return Ok(None);
        }
        let mut near = Vec::with_capacity(3);
        if let Some(highlight) = effective.highlight {
            near.push(Layer::Highlight(highlight));
        }
        if let Some(shading) = effective.shading {
            near.push(Layer::Shading(shading));
        }
        near.extend(view.props.shading.map(Layer::Shading));
        let backgrounds = match self.background(&near, view.beneath.as_deref()) {
            Ok(backgrounds) => backgrounds,
            Err(Unlisted::Paint) => return Ok(LOW),
            Err(Unlisted::Many) => return Ok(Some(UNDECIDED)),
            Err(Unlisted::Stop(error)) => return Err(error),
        };
        let floor = self.limits.min_contrast;
        let low = colours.iter().any(|&colour| {
            backgrounds
                .iter()
                .any(|&background| colour::contrast_ratio(colour, background) < floor)
        });
        Ok(low.then_some("low_contrast"))
    }

    /// The colours that may lie beneath a run: the first of `near`'s layers that shows one, else
    /// what lies beneath `far` (worked out once per link and reading), else the page.
    fn background(&self, near: &[Layer], far: Option<&Outward>) -> Result<Vec<Rgb>, Unlisted> {
        deadline::check().map_err(Unlisted::Stop)?;
        let Some((layer, rest)) = near.split_first() else {
            return match far {
                Some(far) => far
                    .colours
                    .get_or_init(|| self.background(&far.layers, far.outer.as_deref()))
                    .clone(),
                None => self.page.clone().map_err(|()| Unlisted::Paint),
            };
        };
        let beneath = || self.background(rest, far);
        match layer {
            Layer::Highlight(Highlight::None) => beneath(),
            Layer::Highlight(Highlight::Rgb(colour)) => Ok(vec![*colour]),
            Layer::Highlight(Highlight::Bad) => Err(Unlisted::Paint),
            Layer::Fill(colours) => colours.clone().map_err(|()| Unlisted::Paint),
            Layer::Shading(shading) => {
                // An automatic pattern colour is black (Word draws it so); an automatic fill
                // lets the layer beneath show through.
                let pattern = || match self.paint(&shading.colour) {
                    Paint::Auto => Ok(vec![Rgb::BLACK]),
                    Paint::Colours(colours) => Ok(colours),
                    Paint::Bad => Err(Unlisted::Paint),
                };
                let fill = || match self.paint(&shading.fill) {
                    Paint::Auto => beneath(),
                    Paint::Colours(colours) => Ok(colours),
                    Paint::Bad => Err(Unlisted::Paint),
                };
                let mut colours = Vec::new();
                match shading.pattern {
                    Pattern::Clear | Pattern::Nil => return fill(),
                    Pattern::Solid => return pattern(),
                    Pattern::Percent(percent) => {
                        let (over, under) = (pattern()?, fill()?);
                        for &over in &over {
                            for &under in &under {
                                add_candidate(&mut colours, colour::mix(over, under, percent))?;
                            }
                        }
                    }
                    Pattern::Other => {
                        for colour in pattern()?.into_iter().chain(fill()?) {
                            add_candidate(&mut colours, colour)?;
                        }
                    }
                }
                Ok(colours)
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

    /// The table style level of a cell's paragraph under `reading`: the table's style (else the
    /// default table style) with the regions its look turns on for the cell, in Word's order.
    fn table_level(
        &self,
        shape: &TableShape,
        cell: &CellAt,
        reading: Reading,
    ) -> Result<TableLevel, DocError> {
        let Some(row) = shape.rows.get(cell.row) else {
            return Ok(TableLevel {
                undecided: Some(UNDECIDED),
                ..TableLevel::default()
            });
        };
        let Some(merged) = self
            .styles
            .resolve(shape.style.at(reading), StyleKind::Table, reading)
            .map(|id| self.styles.merged(id, StyleKind::Table, reading))
            .transpose()?
        else {
            return Ok(TableLevel {
                undecided: shape.undecided,
                ..TableLevel::default()
            });
        };
        let table_look = shape
            .look
            .at(reading)
            .unwrap_or(Look::from_bits(WORD_TABLE_LOOK));
        let look = row.look.at(reading).unwrap_or(table_look);
        // A band size that is missing or 0: no banding under one reading, bands of one under the
        // other.
        let band = |size: u32| match (size, reading.bands_off) {
            (0, true) => None,
            (0, false) => Some(1),
            (size, _) => Some(size as usize),
        };
        let (rows, columns) = (shape.rows.len(), row.cells);
        let (last_row, last_column) = (cell.row + 1 == rows, cell.column + 1 == columns);
        let (first_row, first_column) = (cell.row == 0, cell.column == 0);
        let mut regions = vec![Region::WholeTable];
        let header_rows = usize::from(table_look.first_row);
        if !look.no_row_bands
            && cell.row >= header_rows
            && let Some(size) = band(merged.row_band)
        {
            let band = (cell.row - header_rows) / size;
            regions.push(if band.is_multiple_of(2) {
                Region::Band1Horz
            } else {
                Region::Band2Horz
            });
        }
        let header_columns = usize::from(table_look.first_column);
        if !look.no_column_bands
            && cell.column >= header_columns
            && let Some(size) = band(merged.column_band)
        {
            let band = (cell.column - header_columns) / size;
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

    /// Records a finished part's warnings and blocks. The blocks copy every visible character, so
    /// the budget is checked as they are made and once they are all made.
    fn finish_part(&mut self, mut part: Part) -> Result<(), DocError> {
        deadline::check()?;
        part.out.sort_by_key(|(ordinal, _, _)| *ordinal);
        part.hidden.sort_by_key(|(ordinal, _, _)| *ordinal);
        for (ordinal, why, para_id) in part.hidden {
            self.warnings.push(ExtractWarning::HiddenContent {
                at: Locator::DocxParagraph {
                    part: part.name.clone(),
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
                if self.blocks.len() % 1024 == 1023 {
                    deadline::check()?;
                }
                self.blocks.push(Block {
                    ordinal,
                    piece: piece as u32,
                    text: replace_nul(&text[range]),
                    locator: Locator::DocxParagraph {
                        part: part.name.clone(),
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
        deadline::check()
    }
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
