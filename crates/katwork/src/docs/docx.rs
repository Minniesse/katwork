//! KatWork's DOCX reader (`docx-accepted` version 1): the accepted view (tracked changes
//! accepted) of the main part, then its headers, footers, footnotes and endnotes, with hidden
//! runs excluded. Where what Word draws cannot be decided, the text counts as hidden.
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
    replace_nul, split_pieces, xml_guard,
};

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const W14: &str = "http://schemas.microsoft.com/office/word/2010/wordml";
const MC: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";
const PACKAGE_RELATIONSHIPS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const RELATIONSHIP: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/";
const VBA_PROJECT: &str = "http://schemas.microsoft.com/office/2006/relationships/vbaProject";
const STRICT: &str = "http://purl.oclc.org/ooxml/";
/// Namespaces read besides WordprocessingML and markup compatibility: Word's 2010 and 2012
/// extensions, the drawing namespaces and VML. Word's 2015 and later ones match `is_w16`.
const UNDERSTOOD: [&str; 12] = [
    W14,
    "http://schemas.microsoft.com/office/word/2012/wordml",
    "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing",
    "http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing",
    "http://schemas.microsoft.com/office/word/2010/wordprocessingShape",
    "http://schemas.microsoft.com/office/word/2010/wordprocessingGroup",
    "http://schemas.microsoft.com/office/word/2010/wordprocessingCanvas",
    "http://schemas.openxmlformats.org/drawingml/2006/main",
    "http://schemas.openxmlformats.org/drawingml/2006/picture",
    "urn:schemas-microsoft-com:vml",
    "urn:schemas-microsoft-com:office:office",
    "urn:schemas-microsoft-com:office:word",
];
/// `w:basedOn` chains longer than this are refused rather than cut short.
const MAX_STYLE_CHAIN: usize = 16;
/// `w:tblLook` when a table has none: Word's default (first row, first column, no column bands).
const WORD_TABLE_LOOK: u16 = 0x04A0;
/// `ST_HighlightColor` (ECMA-376 Part 1 §17.18.40).
const HIGHLIGHTS: [(&str, &str); 16] = [
    ("black", "000000"),
    ("blue", "0000FF"),
    ("cyan", "00FFFF"),
    ("darkBlue", "00008B"),
    ("darkCyan", "008B8B"),
    ("darkGray", "A9A9A9"),
    ("darkGreen", "006400"),
    ("darkMagenta", "800080"),
    ("darkRed", "8B0000"),
    ("darkYellow", "808000"),
    ("green", "00FF00"),
    ("lightGray", "D3D3D3"),
    ("magenta", "FF00FF"),
    ("red", "FF0000"),
    ("white", "FFFFFF"),
    ("yellow", "FFFF00"),
];

pub(super) fn read(bytes: &[u8], limits: &DocLimits) -> Result<Extracted, DocError> {
    let package = package::open_package(bytes, limits)?;
    let root_rels = relationships(&package, "", limits)?.ok_or(DocError::Malformed("docx_rels"))?;
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
    let rels = relationships(&package, &main_name, limits)?.unwrap_or_default();

    let mut warnings = Warnings::new(limits.max_blocks);
    if package.part("word/vbaProject.bin").is_some()
        || rels.iter().any(|rel| rel.kind == VBA_PROJECT)
    {
        warnings.once(ExtractWarning::ActiveContentIgnored {
            what: "vba_project",
        })?;
    }
    let target = |kind: &str| -> Result<Option<&[u8]>, DocError> {
        let kind = format!("{RELATIONSHIP}{kind}");
        match rels.iter().find(|rel| rel.kind == kind && !rel.external) {
            None => Ok(None),
            Some(rel) => rel
                .internal_target()
                .and_then(|name| package.part(&name))
                .map(Some)
                .ok_or(DocError::Malformed("docx_part_missing")),
        }
    };
    let theme = target("theme")?
        .map(|xml| ThemeColours::parse(xml, limits))
        .transpose()?;
    let settings = target("settings")?
        .map(|xml| Settings::parse(xml, limits))
        .transpose()?
        .unwrap_or_default();
    let styles = target("styles")?
        .map(|xml| Styles::parse(xml, limits))
        .transpose()?
        .unwrap_or_default();
    // Headers, footers, footnotes and endnotes, in part-name order, each read once.
    let mut others = BTreeMap::new();
    for rel in &rels {
        let kind = rel.kind.strip_prefix(RELATIONSHIP).unwrap_or_default();
        if !rel.external && matches!(kind, "header" | "footer" | "footnotes" | "endnotes") {
            let name = rel
                .internal_target()
                .filter(|name| package.part(name).is_some())
                .ok_or(DocError::Malformed("docx_part_missing"))?;
            if !name.eq_ignore_ascii_case(&main_name) {
                others.insert(name.to_ascii_lowercase(), name);
            }
        }
    }

    let mut doc = Doc {
        limits,
        styles,
        theme,
        settings,
        page: Ok(vec![Rgb::WHITE]),
        blocks: Vec::new(),
        warnings,
    };
    let main_xml = package.part(&main_name).unwrap_or_default();
    doc.read_part(&main_name, main_xml, true)?;
    for name in others.values() {
        doc.read_part(name, package.part(name).unwrap_or_default(), false)?;
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

/// The relationships of `part` (`""` for the package), or `None` when it has no rels part.
fn relationships(
    package: &Package,
    part: &str,
    limits: &DocLimits,
) -> Result<Option<Vec<Relationship>>, DocError> {
    let (base, file) = match part.rsplit_once('/') {
        Some((dir, file)) => (format!("{dir}/"), file),
        None => (String::new(), part),
    };
    let Some(xml) = package.part(&format!("{base}_rels/{file}.rels")) else {
        return Ok(None);
    };
    let mut walker = Walker::new(xml, limits, relationships_namespace)?;
    let mut found = Vec::new();
    let mut depth = 0u32;
    loop {
        match walker.next()? {
            Item::Start {
                start,
                namespace,
                skipped,
                hidden,
                ..
            } => {
                let name = start.local_name();
                if depth == 1
                    && !skipped
                    && !hidden
                    && namespace == Some(PACKAGE_RELATIONSHIPS)
                    && name.as_ref() == "Relationship"
                {
                    let kind = walker.attr(&start, None, "Type")?.unwrap_or_default();
                    let mode = walker.attr(&start, None, "TargetMode")?;
                    found.push(Relationship {
                        kind,
                        target: walker.attr(&start, None, "Target")?,
                        external: mode.is_some_and(|mode| mode != "Internal"),
                        base: base.clone(),
                    });
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
        /// Inside a branch that is not read: only paragraph starts still count.
        skipped: bool,
        /// Inside an element of an unknown namespace or a branch whose choice Word may make
        /// differently: what is read here cannot be shown, and a property here is undecided.
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
    /// A choice was skipped because the reader does not understand what it requires, so Word
    /// may take it where the reader takes another branch.
    uncertain: bool,
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
    fn new(
        xml: &'i [u8],
        limits: &DocLimits,
        understood: fn(&str) -> Option<&'static str>,
    ) -> Result<Walker<'i>, DocError> {
        xml_guard::scan(xml, limits, None)?;
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

    /// Opens a non-compatibility element; an element of an unknown namespace hides what is
    /// inside it and is not reported.
    fn open(&mut self, start: BytesStart<'i>, namespace: Option<&'static str>) -> Option<Item<'i>> {
        let reported = namespace.is_some();
        self.hidden += u32::from(!reported);
        self.frames.push(Frame {
            reported,
            skip: false,
            hide: !reported,
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
    /// understood namespaces is read, else the `mc:Fallback`; a branch read after a skipped
    /// choice is hidden, and so are skipped choices' contents for anything they would change.
    fn open_compatibility(&mut self, start: &BytesStart) -> Result<(), DocError> {
        let name = start.local_name();
        let in_alternate = self.frames.last().is_some_and(|frame| frame.alternate);
        let mut frame = Frame {
            reported: false,
            skip: false,
            hide: false,
            alternate: false,
        };
        match (name.as_ref(), in_alternate) {
            ("AlternateContent", _) => {
                frame.alternate = true;
                self.alternates.push(Alternate::default());
            }
            ("Choice", true) => {
                let understood = self.requires_understood(start)?;
                let state = self
                    .alternates
                    .last_mut()
                    .expect("an open AlternateContent");
                if state.chosen {
                    frame.skip = true;
                } else if understood {
                    state.chosen = true;
                    frame.hide = state.uncertain;
                } else {
                    state.uncertain = true;
                    frame.skip = true;
                    frame.hide = true;
                }
            }
            ("Fallback", true) => {
                let state = self
                    .alternates
                    .last_mut()
                    .expect("an open AlternateContent");
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
    UNDERSTOOD
        .iter()
        .copied()
        .find(|known| *known == uri)
        .or_else(|| is_w16(uri).then_some("w16"))
}

fn relationships_namespace(uri: &str) -> Option<&'static str> {
    (uri == PACKAGE_RELATIONSHIPS).then_some(PACKAGE_RELATIONSHIPS)
}

/// Word's 2015 and later extension namespaces (`w16`, `w16cid`, `w16se`, `w16du` ...).
fn is_w16(uri: &str) -> bool {
    let Some(rest) = uri.strip_prefix("http://schemas.microsoft.com/office/word/") else {
        return false;
    };
    let Some((year, rest)) = rest.split_at_checked(4) else {
        return false;
    };
    year.parse::<u16>().is_ok_and(|year| year >= 2015)
        && (rest == "/wordml" || rest.starts_with("/wordml/"))
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
        let units = [
            ("mm", 72.0 / 25.4),
            ("cm", 72.0 / 2.54),
            ("in", 72.0),
            ("pt", 1.0),
            ("pc", 12.0),
            ("pi", 12.0),
        ];
        for (unit, points) in units {
            if let Some(number) = value.strip_suffix(unit)
                && !number.is_empty()
                && number
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || byte == b'.')
                && number.bytes().filter(|&byte| byte == b'.').count() <= 1
                && let Ok(number) = number.parse::<f64>()
            {
                return Size::HalfPoints(number * points * 2.0);
            }
        }
        Size::Bad
    }
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

/// A `w:themeColor` value: a scheme slot, one of the four names `w:clrSchemeMapping` maps
/// (`background1`, `text1`, `background2`, `text2`, by index), or one that does not parse.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ThemeRef {
    Slot(SchemeSlot),
    Mapped(usize),
    Bad,
}

impl ThemeRef {
    /// `None` for `none`, which names no theme colour.
    fn parse(value: &str) -> Option<ThemeRef> {
        Some(match value {
            "none" => return None,
            "background1" => ThemeRef::Mapped(0),
            "text1" => ThemeRef::Mapped(1),
            "background2" => ThemeRef::Mapped(2),
            "text2" => ThemeRef::Mapped(3),
            name => scheme_index(name).map_or(ThemeRef::Bad, ThemeRef::Slot),
        })
    }
}

/// `ST_ColorSchemeIndex` and the slot names `ST_ThemeColor` shares with it.
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

/// Run properties as one level of the cascade sets them. `undecided`: a property was written
/// where what Word applies cannot be decided (an unknown namespace, an uncertain branch).
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
    undecided: bool,
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
        self.undecided |= later.undecided;
    }

    fn read(&mut self, walker: &Walker, start: &BytesStart, local: &str) -> Result<(), DocError> {
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
    undecided: bool,
}

impl ParaProps {
    fn overlay(&mut self, later: &ParaProps) {
        if later.style.is_some() {
            self.style = later.style.clone();
        }
        if later.shading.is_some() {
            self.shading = later.shading;
        }
        self.undecided |= later.undecided;
    }

    fn read(&mut self, walker: &Walker, start: &BytesStart, local: &str) -> Result<(), DocError> {
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
    undecided: bool,
}

impl CellProps {
    fn overlay(&mut self, later: &CellProps) {
        if later.shading.is_some() {
            self.shading = later.shading;
        }
        self.undecided |= later.undecided;
    }

    fn read(&mut self, walker: &Walker, start: &BytesStart, local: &str) -> Result<(), DocError> {
        if local == "shd" {
            self.shading = Some(Shading::read(walker, start)?);
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

    fn undecided(&mut self) {
        self.run.undecided = true;
        self.para.undecided = true;
        self.cell.undecided = true;
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
    styles: HashMap<String, Style>,
    defaults_by_kind: HashMap<StyleKind, String>,
    merged: RefCell<HashMap<(StyleKind, String), Rc<Merged>>>,
}

impl Styles {
    fn parse(xml: &[u8], limits: &DocLimits) -> Result<Styles, DocError> {
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
        let mut walker = Walker::new(xml, limits, document_namespace)?;
        let mut nodes: Vec<Node> = Vec::new();
        // The style being read (its id and whether it may be a default), and its region.
        let mut style: Option<(String, Style, bool)> = None;
        let mut region: Option<(Region, Props)> = None;
        loop {
            match walker.next()? {
                Item::Start {
                    start,
                    namespace,
                    skipped,
                    hidden,
                    ..
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
                            (Some(Node::DocDefaults), "rPrDefault") => Node::RunDefault,
                            (Some(Node::DocDefaults), "pPrDefault") => Node::ParaDefault,
                            (Some(Node::RunDefault), "rPr") => Node::Run(Into::Defaults),
                            (Some(Node::ParaDefault), "pPr") => Node::Para(Into::Defaults),
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
                                style = kind.zip(id).map(|(kind, id)| {
                                    let mut props = Props::default();
                                    if hidden {
                                        props.undecided();
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
                                    style.based_on = walker.w(&start, "val")?;
                                }
                                Node::Other
                            }
                            (Some(Node::Style), "tblStylePr") => {
                                region = walker
                                    .w(&start, "type")?
                                    .and_then(|value| Region::parse(&value))
                                    .map(|found| (found, Props::default()));
                                Node::Region
                            }
                            (Some(Node::Style), "rPr") => Node::Run(Into::Style),
                            (Some(Node::Style), "pPr") => Node::Para(Into::Style),
                            (Some(Node::Style), "tcPr") => Node::Cell(Into::Style),
                            (Some(Node::Style), "tblPr") => Node::TableProps,
                            (Some(Node::Region), "rPr") => Node::Run(Into::Region),
                            (Some(Node::Region), "pPr") => Node::Para(Into::Region),
                            (Some(Node::Region), "tcPr") => Node::Cell(Into::Region),
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
                                        props.undecided();
                                    }
                                    match parent {
                                        Some(Node::Run(_)) => {
                                            props.run.read(&walker, &start, local)?
                                        }
                                        Some(Node::Para(_)) => {
                                            props.para.read(&walker, &start, local)?
                                        }
                                        _ => props.cell.read(&walker, &start, local)?,
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
                                if let Some((_, style, _)) = &mut style {
                                    if local == "tblStyleRowBandSize" {
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
                                styles.defaults.undecided()
                            }
                            Node::Run(Into::Style)
                            | Node::Para(Into::Style)
                            | Node::Cell(Into::Style) => {
                                if let Some((_, style, _)) = &mut style {
                                    style.props.undecided();
                                }
                            }
                            _ => {
                                if let Some((_, props)) = &mut region {
                                    props.undecided();
                                }
                            }
                        }
                    }
                    nodes.push(node);
                }
                Item::End => match nodes.pop() {
                    Some(Node::Style) => {
                        if let Some((id, style, default)) = style.take() {
                            if default {
                                styles.defaults_by_kind.insert(style.kind, id.clone());
                            }
                            styles.styles.insert(id, style);
                        }
                    }
                    Some(Node::Region) => {
                        if let (Some((found, props)), Some((_, style, _))) =
                            (region.take(), &mut style)
                        {
                            style.regions.push((found, props));
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
    /// a cycle or a style of another kind; one longer than 16 styles is `TooComplex`.
    fn merged(&self, id: &str, kind: StyleKind) -> Result<Rc<Merged>, DocError> {
        let key = (kind, id.to_owned());
        if let Some(found) = self.merged.borrow().get(&key) {
            return Ok(found.clone());
        }
        let mut chain = Vec::new();
        let mut seen = HashSet::new();
        let mut next = Some(id);
        while let Some(id) = next {
            let Some(style) = self.styles.get(id).filter(|style| style.kind == kind) else {
                break;
            };
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
    /// `background1`, `text1`, `background2` and `text2`.
    mapping: [Mapping; 4],
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            display_background: false,
            mapping: [
                Mapping::Slot(SchemeSlot::Lt1),
                Mapping::Slot(SchemeSlot::Dk1),
                Mapping::Slot(SchemeSlot::Lt2),
                Mapping::Slot(SchemeSlot::Dk2),
            ],
        }
    }
}

impl Settings {
    fn parse(xml: &[u8], limits: &DocLimits) -> Result<Settings, DocError> {
        let mut settings = Settings::default();
        let mut walker = Walker::new(xml, limits, document_namespace)?;
        let mut depth = 0u32;
        loop {
            match walker.next()? {
                Item::Start {
                    start,
                    namespace,
                    skipped,
                    hidden,
                    ..
                } => {
                    let name = start.local_name();
                    if depth == 1 && !skipped && namespace == Some(W) {
                        match name.as_ref() {
                            // Where it is uncertain, both page colours count, as when it is off.
                            "displayBackgroundShape" => {
                                settings.display_background = !hidden
                                    && Toggle::parse(walker.w(&start, "val")?.as_deref())
                                        == Toggle::On;
                            }
                            "clrSchemeMapping" => {
                                for (at, attribute) in ["bg1", "t1", "bg2", "t2"].iter().enumerate()
                                {
                                    if let Some(value) = walker.w(&start, attribute)? {
                                        settings.mapping[at] = match scheme_index(&value) {
                                            Some(slot) if !hidden => Mapping::Slot(slot),
                                            _ => Mapping::Bad,
                                        };
                                    } else if hidden {
                                        settings.mapping[at] = Mapping::Bad;
                                    }
                                }
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
enum Layer<'a> {
    Highlight(Highlight),
    Shading(&'a Shading),
}

/// A paragraph being read: its runs are judged when it ends (or, in a table, when the table
/// ends and its shape is known).
struct Para {
    ordinal: u32,
    para_id: Option<String>,
    props: ParaProps,
    runs: Vec<Run>,
    run: Option<Run>,
    cell: Option<CellAt>,
}

#[derive(Default)]
struct Run {
    props: RunProps,
    text: String,
    /// Text of this run was dropped because what Word shows there cannot be decided.
    unknown: bool,
}

/// A table paragraph's cell: its row and column, and the cell's own properties.
#[derive(Clone)]
struct CellAt {
    row: usize,
    column: usize,
    props: CellProps,
}

struct Table {
    style: Option<String>,
    look: Option<Look>,
    /// A table property sat where Word's choice cannot be decided.
    undecided: bool,
    rows: Vec<Row>,
    open_row: bool,
    /// Paragraphs of this table, judged once its last row and columns are known.
    pending: Vec<Para>,
}

struct Row {
    cells: usize,
    look: Option<Look>,
    cell: Option<CellAt>,
    cell_has_content: bool,
}

enum Container {
    Table(Table),
    /// A text box: what is inside it is not in the table around it.
    TextBox,
}

#[derive(Clone, Copy, PartialEq)]
enum Node {
    Document,
    Background,
    Body,
    Paragraph,
    Run,
    Text,
    RunProps,
    ParaProps,
    Table,
    TableProps,
    Row,
    RowExceptions,
    Cell,
    CellProps,
    TextBox,
    Excluded,
    Other,
}

/// One part's walk.
struct Part<'n> {
    name: &'n str,
    ordinal: u32,
    /// Inside deleted, moved-away or embedded content.
    excluded: u32,
    body_started: bool,
    paragraphs: Vec<Option<Para>>,
    containers: Vec<Container>,
    /// Open fields: true while their instruction is being read.
    fields: Vec<bool>,
    out: Vec<(u32, String, Option<String>)>,
    hidden: Vec<(u32, &'static str)>,
}

struct Doc<'l> {
    limits: &'l DocLimits,
    styles: Styles,
    theme: Option<ThemeColours>,
    settings: Settings,
    /// The page colour's candidates under the run (white among them unless Word's print layout
    /// shows the page colour), or `Err` when it does not parse.
    page: Result<Vec<Rgb>, ()>,
    blocks: Vec<Block>,
    warnings: Warnings,
}

impl Doc<'_> {
    fn read_part(&mut self, name: &str, xml: &[u8], main: bool) -> Result<(), DocError> {
        if main {
            check_root(xml, self.limits)?;
        }
        let mut walker = Walker::new(xml, self.limits, document_namespace)?;
        let mut part = Part {
            name,
            ordinal: 0,
            excluded: 0,
            body_started: false,
            paragraphs: Vec::new(),
            containers: Vec::new(),
            fields: Vec::new(),
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
                        main,
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
                    if nodes.last() == Some(&Node::Text)
                        && !skipped
                        && part.excluded == 0
                        && part.fields.last() != Some(&true)
                        && let Some(Some(para)) = part.paragraphs.last_mut()
                        && let Some(run) = &mut para.run
                    {
                        if hidden {
                            run.unknown |= !text.is_empty();
                        } else {
                            run.text.push_str(&text);
                        }
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
        main: bool,
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
        if !w {
            if local == "OLEObject" && namespace == Some(UNDERSTOOD[10]) {
                self.warnings
                    .once(ExtractWarning::EmbeddedContentIgnored { what: "ole_object" })?;
                part.excluded += 1;
                return Ok(Node::Excluded);
            }
            return Ok(Node::Other);
        }
        Ok(match (parent, local) {
            (None, "document") if main => Node::Document,
            (Some(Node::Document), "background") => {
                if part.body_started {
                    return Err(DocError::Malformed("docx_structure"));
                }
                let spec = ColourSpec::read(
                    walker,
                    start,
                    ["color", "themeColor", "themeTint", "themeShade"],
                )?;
                self.page = if hidden {
                    Err(())
                } else {
                    self.page_colours(&spec)
                };
                Node::Background
            }
            (Some(Node::Document), "body") => {
                part.body_started = true;
                Node::Body
            }
            (_, "p") => {
                let cell = self.current_cell(part);
                let para_id = walker
                    .attr(start, Some(W14), "paraId")?
                    .filter(|id| id.len() == 8 && id.bytes().all(|b| b.is_ascii_hexdigit()));
                part.paragraphs.push(Some(Para {
                    ordinal: part.ordinal,
                    para_id,
                    props: ParaProps {
                        undecided: hidden,
                        ..ParaProps::default()
                    },
                    runs: Vec::new(),
                    run: None,
                    cell,
                }));
                Node::Paragraph
            }
            (_, "r") => match part.paragraphs.last_mut() {
                Some(Some(para)) if para.run.is_none() => {
                    para.run = Some(Run::default());
                    Node::Run
                }
                _ => Node::Other,
            },
            (Some(Node::Run), "t") => Node::Text,
            (Some(Node::Run), "tab" | "br") => {
                if part.fields.last() != Some(&true)
                    && let Some(Some(para)) = part.paragraphs.last_mut()
                    && let Some(run) = &mut para.run
                {
                    if hidden {
                        run.unknown = true;
                    } else {
                        run.text.push(if local == "tab" { '\t' } else { '\n' });
                    }
                }
                Node::Other
            }
            (Some(Node::Run), "fldChar") => {
                match walker.w(start, "fldCharType")?.as_deref() {
                    Some("begin") => {
                        if part.fields.len() as u64 >= u64::from(self.limits.max_xml_depth) {
                            return Err(DocError::TooComplex("field_depth"));
                        }
                        part.fields.push(true);
                    }
                    Some("separate") => {
                        if let Some(instruction) = part.fields.last_mut() {
                            *instruction = false;
                        }
                    }
                    Some("end") => {
                        part.fields.pop();
                    }
                    _ => {}
                }
                Node::Other
            }
            (Some(Node::Run), "rPr") => {
                if hidden
                    && let Some(Some(para)) = part.paragraphs.last_mut()
                    && let Some(run) = &mut para.run
                {
                    run.props.undecided = true;
                }
                Node::RunProps
            }
            (Some(Node::RunProps), _) => {
                if let Some(Some(para)) = part.paragraphs.last_mut()
                    && let Some(run) = &mut para.run
                {
                    run.props.undecided |= hidden;
                    run.props.read(walker, start, local)?;
                }
                Node::Other
            }
            (Some(Node::Paragraph), "pPr") => {
                if hidden && let Some(Some(para)) = part.paragraphs.last_mut() {
                    para.props.undecided = true;
                }
                Node::ParaProps
            }
            (Some(Node::ParaProps), _) => {
                if let Some(Some(para)) = part.paragraphs.last_mut() {
                    para.props.undecided |= hidden;
                    para.props.read(walker, start, local)?;
                }
                Node::Other
            }
            (_, "del" | "moveFrom") => {
                part.excluded += 1;
                Node::Excluded
            }
            (_, "object") => {
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
                part.paragraphs.push(None);
                part.containers.push(Container::TextBox);
                Node::TextBox
            }
            (_, "tbl") => {
                mark_cell_content(part);
                part.containers.push(Container::Table(Table {
                    style: None,
                    look: None,
                    undecided: false,
                    rows: Vec::new(),
                    open_row: false,
                    pending: Vec::new(),
                }));
                Node::Table
            }
            (Some(Node::Table), "tblPr") => match part.containers.last() {
                Some(Container::Table(table)) if !table.rows.is_empty() => {
                    return Err(DocError::Malformed("docx_structure"));
                }
                _ => Node::TableProps,
            },
            (Some(Node::TableProps), "tblStyle" | "tblLook") => {
                let style = (local == "tblStyle")
                    .then(|| walker.w(start, "val"))
                    .transpose()?
                    .flatten();
                let look = (local == "tblLook")
                    .then(|| Look::read(walker, start))
                    .transpose()?;
                if let Some(Container::Table(table)) = part.containers.last_mut() {
                    table.undecided |= hidden;
                    table.style = style.or(table.style.take());
                    table.look = look.or(table.look);
                }
                Node::Other
            }
            (_, "tr") => match part.containers.last_mut() {
                Some(Container::Table(table)) => {
                    table.rows.push(Row {
                        cells: 0,
                        look: None,
                        cell: None,
                        cell_has_content: false,
                    });
                    table.open_row = true;
                    Node::Row
                }
                _ => Node::Other,
            },
            (Some(Node::Row), "tblPrEx") => Node::RowExceptions,
            (Some(Node::RowExceptions), "tblLook") => {
                let look = Look::read(walker, start)?;
                if let Some(Container::Table(table)) = part.containers.last_mut() {
                    table.undecided |= hidden;
                    if let Some(row) = table.rows.last_mut() {
                        row.look = Some(look);
                    }
                }
                Node::Other
            }
            (_, "tc") => match part.containers.last_mut() {
                Some(Container::Table(table)) if table.open_row => {
                    let row_index = table.rows.len() - 1;
                    let row = table.rows.last_mut().expect("an open row");
                    row.cell = Some(CellAt {
                        row: row_index,
                        column: row.cells,
                        props: CellProps {
                            undecided: hidden,
                            ..CellProps::default()
                        },
                    });
                    row.cells += 1;
                    row.cell_has_content = false;
                    Node::Cell
                }
                _ => Node::Other,
            },
            (Some(Node::Cell), "tcPr") => match part.containers.last() {
                Some(Container::Table(table))
                    if table.rows.last().is_some_and(|row| row.cell_has_content) =>
                {
                    return Err(DocError::Malformed("docx_structure"));
                }
                _ => Node::CellProps,
            },
            (Some(Node::CellProps), _) => {
                if let Some(Container::Table(table)) = part.containers.last_mut()
                    && let Some(row) = table.rows.last_mut()
                    && let Some(cell) = &mut row.cell
                {
                    cell.props.undecided |= hidden;
                    cell.props.read(walker, start, local)?;
                }
                Node::Other
            }
            _ => Node::Other,
        })
    }

    fn close(&mut self, part: &mut Part, node: Node) -> Result<(), DocError> {
        match node {
            Node::Paragraph => {
                if let Some(Some(para)) = part.paragraphs.pop() {
                    match (&para.cell, part.containers.last_mut()) {
                        (Some(_), Some(Container::Table(table))) => table.pending.push(para),
                        _ => self.judge(part, para, None)?,
                    }
                }
            }
            Node::Run => {
                if let Some(Some(para)) = part.paragraphs.last_mut()
                    && let Some(run) = para.run.take()
                {
                    para.runs.push(run);
                }
            }
            Node::Excluded => part.excluded -= 1,
            Node::TextBox => {
                part.paragraphs.pop();
                part.containers.pop();
            }
            Node::Table => {
                if let Some(Container::Table(mut table)) = part.containers.pop() {
                    for para in std::mem::take(&mut table.pending) {
                        self.judge(part, para, Some(&table))?;
                    }
                }
            }
            Node::Row => {
                if let Some(Container::Table(table)) = part.containers.last_mut() {
                    table.open_row = false;
                }
            }
            Node::Cell => {
                if let Some(Container::Table(table)) = part.containers.last_mut()
                    && let Some(row) = table.rows.last_mut()
                {
                    row.cell = None;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// The innermost table cell a paragraph starting now sits in (none inside a text box).
    fn current_cell(&self, part: &mut Part) -> Option<CellAt> {
        mark_cell_content(part);
        match part.containers.last() {
            Some(Container::Table(table)) if table.open_row => {
                table.rows.last().and_then(|row| row.cell.clone())
            }
            _ => None,
        }
    }

    /// Judges a finished paragraph's runs and records its visible text and hidden reason.
    fn judge(
        &mut self,
        part: &mut Part,
        para: Para,
        table: Option<&Table>,
    ) -> Result<(), DocError> {
        deadline::check()?;
        let styles = &self.styles;
        let level = match (table, &para.cell) {
            (Some(table), Some(cell)) => Some(self.table_level(table, cell)?),
            _ => None,
        };
        let para_style = styles
            .resolve(para.props.style.as_deref(), StyleKind::Paragraph)
            .map(|id| styles.merged(id, StyleKind::Paragraph))
            .transpose()?;
        let mut para_props = styles.defaults.para.clone();
        if let Some(level) = &level {
            para_props.overlay(&level.props.para);
        }
        if let Some(style) = &para_style {
            para_props.overlay(&style.props.para);
        }
        para_props.overlay(&para.props);
        let cell_props = para.cell.as_ref().map(|cell| &cell.props);

        let mut text = String::new();
        let mut why: Option<&'static str> = None;
        for run in &para.runs {
            let char_style = styles
                .resolve(run.props.style.as_deref(), StyleKind::Character)
                .map(|id| styles.merged(id, StyleKind::Character))
                .transpose()?;
            let levels = [
                level.as_ref().map(|level| &level.props.run),
                para_style.as_ref().map(|style| &style.props.run),
                char_style.as_ref().map(|style| &style.props.run),
            ];
            let mut effective = styles.defaults.run.clone();
            for props in levels.iter().flatten() {
                effective.overlay(props);
            }
            effective.overlay(&run.props);
            let undecided = effective.undecided
                || para_props.undecided
                || cell_props.is_some_and(|props| props.undecided)
                || level.as_ref().is_some_and(|level| level.undecided);
            let hidden = if run.text.is_empty() && !run.unknown {
                None
            } else {
                self.hidden_why(
                    &run.props,
                    &levels,
                    &effective,
                    &run.text,
                    undecided,
                    &para_props,
                    cell_props,
                    level.as_ref(),
                )
            };
            match hidden {
                Some(reason) => {
                    why = why.or(Some(reason));
                }
                None => {
                    text.push_str(&run.text);
                    if run.unknown {
                        why = why.or(Some("unknown_namespace"));
                    }
                }
            }
        }
        if let Some(why) = why {
            part.hidden.push((para.ordinal, why));
        }
        if !text.chars().all(char::is_whitespace) {
            part.out.push((para.ordinal, text, para.para_id));
        }
        Ok(())
    }

    /// Why a run is hidden, if it is: `vanish`, `tiny`, `low_contrast`, or `unknown_namespace`
    /// when its properties sit where Word's choice cannot be decided.
    #[allow(clippy::too_many_arguments)]
    fn hidden_why(
        &self,
        direct: &RunProps,
        levels: &[Option<&RunProps>; 3],
        effective: &RunProps,
        text: &str,
        undecided: bool,
        para: &ParaProps,
        cell: Option<&CellProps>,
        table: Option<&TableLevel>,
    ) -> Option<&'static str> {
        if self.vanished(direct, levels, effective) {
            return Some("vanish");
        }
        if self.tiny(effective, text) {
            return Some("tiny");
        }
        if self.low_contrast(effective, para, cell, table) {
            return Some("low_contrast");
        }
        undecided.then_some("unknown_namespace")
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

    fn low_contrast(
        &self,
        effective: &RunProps,
        para: &ParaProps,
        cell: Option<&CellProps>,
        table: Option<&TableLevel>,
    ) -> bool {
        let colours = match effective.colour.as_ref().map(|spec| self.paint(spec)) {
            None | Some(Paint::Auto) => return false,
            Some(Paint::Bad) => return true,
            Some(Paint::Colours(colours)) => colours,
        };
        let mut layers = Vec::new();
        if let Some(highlight) = effective.highlight {
            layers.push(Layer::Highlight(highlight));
        }
        let shadings = [
            effective.shading.as_ref(),
            para.shading.as_ref(),
            cell.and_then(|cell| cell.shading.as_ref()),
            table.and_then(|table| table.region_shading.as_ref()),
            table.and_then(|table| table.props.cell.shading.as_ref()),
        ];
        layers.extend(shadings.into_iter().flatten().map(Layer::Shading));
        let Ok(backgrounds) = self.background(&layers) else {
            return true;
        };
        let floor = self.limits.min_contrast;
        colours.iter().any(|&colour| {
            backgrounds
                .iter()
                .any(|&background| colour::contrast_ratio(colour, background) < floor)
        })
    }

    /// The colours that may lie beneath a run: the first layer that shows one, else the page.
    fn background(&self, layers: &[Layer]) -> Result<Vec<Rgb>, ()> {
        let Some((layer, rest)) = layers.split_first() else {
            return self.page.clone();
        };
        match layer {
            Layer::Highlight(Highlight::None) => self.background(rest),
            Layer::Highlight(Highlight::Rgb(colour)) => Ok(vec![*colour]),
            Layer::Highlight(Highlight::Bad) => Err(()),
            Layer::Shading(shading) => {
                // An automatic pattern colour is black (Word draws it so); an automatic fill
                // lets the layer beneath show through.
                let pattern = || match self.paint(&shading.colour) {
                    Paint::Auto => Ok(vec![Rgb::BLACK]),
                    Paint::Colours(colours) => Ok(colours),
                    Paint::Bad => Err(()),
                };
                let fill = || match self.paint(&shading.fill) {
                    Paint::Auto => self.background(rest),
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
    /// theme part; one it cannot resolve has two candidates, the RGB value beside it and the
    /// default Office theme's colour, because what Word draws then is not known.
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
        let slot = match theme_ref {
            ThemeRef::Slot(slot) => Some(slot),
            ThemeRef::Mapped(at) => match self.settings.mapping[at] {
                Mapping::Slot(slot) => Some(slot),
                Mapping::Bad => return Paint::Bad,
            },
            ThemeRef::Bad => None,
        };
        if let (Some(slot), Some(theme)) = (slot, &self.theme)
            && theme.slot(slot).is_some()
        {
            return Paint::Colours(vec![theme.resolve(slot, tint)]);
        }
        let mut candidates = Vec::new();
        if let Some(Hex::Rgb(colour)) = spec.value {
            candidates.push(colour);
        }
        if let Some(slot) = slot {
            candidates.push(ThemeColours::office_default().resolve(slot, tint));
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
    fn table_level(&self, table: &Table, cell: &CellAt) -> Result<TableLevel, DocError> {
        let Some(merged) = self
            .styles
            .resolve(table.style.as_deref(), StyleKind::Table)
            .map(|id| self.styles.merged(id, StyleKind::Table))
            .transpose()?
        else {
            return Ok(TableLevel {
                undecided: table.undecided,
                ..TableLevel::default()
            });
        };
        let table_look = table.look.unwrap_or(Look::from_bits(WORD_TABLE_LOOK));
        let row = &table.rows[cell.row];
        let look = row.look.unwrap_or(table_look);
        let (rows, columns) = (table.rows.len(), row.cells);
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
            undecided: table.undecided || merged.props.cell.undecided,
        };
        for region in regions {
            if let Some(props) = merged.regions.get(&region) {
                level.props.run.overlay(&props.run);
                level.props.para.overlay(&props.para);
                if props.cell.shading.is_some() {
                    level.region_shading = props.cell.shading;
                }
                level.undecided |= props.cell.undecided;
            }
        }
        Ok(level)
    }

    fn finish_part(&mut self, mut part: Part) -> Result<(), DocError> {
        part.out.sort_by_key(|(ordinal, _, _)| *ordinal);
        part.hidden.sort_by_key(|(ordinal, _)| *ordinal);
        for (ordinal, why) in part.hidden {
            self.warnings
                .push(ExtractWarning::HiddenContent { ordinal, why })?;
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

/// The table style level of one cell.
#[derive(Default)]
struct TableLevel {
    props: Props,
    /// The shading of the last region that sets one.
    region_shading: Option<Shading>,
    undecided: bool,
}

/// A paragraph or table starting now is content of the cell it sits in.
fn mark_cell_content(part: &mut Part) {
    if let Some(Container::Table(table)) = part.containers.last_mut()
        && table.open_row
        && let Some(row) = table.rows.last_mut()
    {
        row.cell_has_content = true;
    }
}

/// The main part's root must be a transitional `w:document`; a Strict one is unsupported.
fn check_root(xml: &[u8], limits: &DocLimits) -> Result<(), DocError> {
    xml_guard::scan(xml, limits, None)?;
    let mut reader = xml_guard::reader(xml);
    loop {
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
}
