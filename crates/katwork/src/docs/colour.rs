//! Colours as the readers judge them: WCAG 2 contrast, and the twelve colours of a DrawingML
//! theme shared by the DOCX and XLSX readers.
use quick_xml::{events::Event, name::ResolveResult};

use super::{
    DocError, DocLimits, deadline,
    xml_guard::{self, ElementBudget},
};

const DRAWINGML: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const MC: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";
/// The elements from the theme part's root to its colour scheme.
const SCHEME_PATH: [&str; 3] = ["theme", "themeElements", "clrScheme"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const BLACK: Rgb = Rgb { r: 0, g: 0, b: 0 };
    pub const WHITE: Rgb = Rgb {
        r: 255,
        g: 255,
        b: 255,
    };
}

/// `RRGGBB`, or `AARRGGBB` with the alpha byte ignored, in either case. `auto` and anything else
/// are `None`.
pub fn parse_hex(s: &str) -> Option<Rgb> {
    let digits = match s.len() {
        6 => s,
        8 => s.get(2..)?,
        _ => return None,
    };
    if !s.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
    Some(Rgb {
        r: byte(0)?,
        g: byte(2)?,
        b: byte(4)?,
    })
}

/// The WCAG 2 contrast ratio, `(L1 + 0.05) / (L2 + 0.05)` with `L1` the lighter colour's
/// relative luminance, from 1 to 21.
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    let (lighter, darker) = if a >= b { (a, b) } else { (b, a) };
    ((lighter + 0.05) / (darker + 0.05)) as f32
}

fn luminance(colour: Rgb) -> f64 {
    let linear = |value: u8| {
        let c = f64::from(value) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(colour.r) + 0.7152 * linear(colour.g) + 0.0722 * linear(colour.b)
}

/// Black or white, whichever contrasts more with `background` (black on a tie): the colour Word
/// draws automatic text in.
pub fn readable_on(background: Rgb) -> Rgb {
    if contrast_ratio(Rgb::BLACK, background) >= contrast_ratio(Rgb::WHITE, background) {
        Rgb::BLACK
    } else {
        Rgb::WHITE
    }
}

/// `mix` of `over` laid over `under`, as a pattern of `percent` coverage shows from a distance.
pub(crate) fn mix(over: Rgb, under: Rgb, percent: u8) -> Rgb {
    let percent = u16::from(percent.min(100));
    let channel = |over: u8, under: u8| {
        ((u16::from(over) * percent + u16::from(under) * (100 - percent) + 50) / 100) as u8
    };
    Rgb {
        r: channel(over.r, under.r),
        g: channel(over.g, under.g),
        b: channel(over.b, under.b),
    }
}

/// The twelve slots of a theme's `a:clrScheme`, in the scheme's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SchemeSlot {
    Dk1,
    Lt1,
    Dk2,
    Lt2,
    Accent1,
    Accent2,
    Accent3,
    Accent4,
    Accent5,
    Accent6,
    Hlink,
    FolHlink,
}

impl SchemeSlot {
    const ALL: [SchemeSlot; 12] = [
        SchemeSlot::Dk1,
        SchemeSlot::Lt1,
        SchemeSlot::Dk2,
        SchemeSlot::Lt2,
        SchemeSlot::Accent1,
        SchemeSlot::Accent2,
        SchemeSlot::Accent3,
        SchemeSlot::Accent4,
        SchemeSlot::Accent5,
        SchemeSlot::Accent6,
        SchemeSlot::Hlink,
        SchemeSlot::FolHlink,
    ];

    /// An XLSX `theme` index, in Excel's order, which is not the scheme's.
    pub fn from_xlsx_theme_index(index: u32) -> Option<SchemeSlot> {
        Some(match index {
            0 => SchemeSlot::Lt1,
            1 => SchemeSlot::Dk1,
            2 => SchemeSlot::Lt2,
            3 => SchemeSlot::Dk2,
            4..=9 => SchemeSlot::ALL[index as usize],
            10 => SchemeSlot::Hlink,
            11 => SchemeSlot::FolHlink,
            _ => return None,
        })
    }

    /// The slot an `a:clrScheme` child names (`dk1` to `folHlink`).
    pub(crate) fn from_name(name: &str) -> Option<SchemeSlot> {
        Some(match name {
            "dk1" => SchemeSlot::Dk1,
            "lt1" => SchemeSlot::Lt1,
            "dk2" => SchemeSlot::Dk2,
            "lt2" => SchemeSlot::Lt2,
            "accent1" => SchemeSlot::Accent1,
            "accent2" => SchemeSlot::Accent2,
            "accent3" => SchemeSlot::Accent3,
            "accent4" => SchemeSlot::Accent4,
            "accent5" => SchemeSlot::Accent5,
            "accent6" => SchemeSlot::Accent6,
            "hlink" => SchemeSlot::Hlink,
            "folHlink" => SchemeSlot::FolHlink,
            _ => return None,
        })
    }
}

/// A theme part's colour scheme. A slot the part does not define as a plain `a:srgbClr` or an
/// `a:sysClr` with `lastClr` stays unknown to `slot`, and a colour naming it has two candidates
/// in the DOCX reader, the RGB value beside it and `office_default()`'s. `candidates` resolves
/// every DrawingML colour form and transform a slot may hold, and is `None` only for a slot the
/// part leaves undecided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeColours {
    colours: [Option<Rgb>; 12],
    candidates: [Option<Vec<Rgb>>; 12],
}

impl ThemeColours {
    /// The scheme of a new Word document.
    pub fn office_default() -> ThemeColours {
        let hex = |s| parse_hex(s);
        let colours = [
            hex("000000"),
            hex("FFFFFF"),
            hex("44546A"),
            hex("E7E6E6"),
            hex("4472C4"),
            hex("ED7D31"),
            hex("A5A5A5"),
            hex("FFC000"),
            hex("5B9BD5"),
            hex("70AD47"),
            hex("0563C1"),
            hex("954F72"),
        ];
        ThemeColours {
            colours,
            candidates: colours.map(|colour| colour.map(|colour| vec![colour])),
        }
    }

    /// Reads the colour scheme at `a:theme/a:themeElements/a:clrScheme`, after `xml_guard::scan`
    /// (which takes the part's elements from `budget`). A part holding any markup-compatibility
    /// element, a second scheme on that path, or an element of another namespace inside the
    /// scheme leaves every slot unknown: which colours Word takes from it is not known then.
    pub fn parse(
        theme_xml: &[u8],
        limits: &DocLimits,
        budget: &mut ElementBudget,
    ) -> Result<ThemeColours, DocError> {
        xml_guard::scan(theme_xml, limits, None, budget)?;
        let mut reader = xml_guard::reader(theme_xml);
        let mut scheme = Scheme::default();
        // How many of the open elements, from the root, are the scheme's path so far.
        let (mut depth, mut on_path) = (0usize, 0usize);
        let (mut schemes, mut uncertain) = (0u32, false);
        for events in 0u64.. {
            if events.is_multiple_of(1024) {
                deadline::check()?;
            }
            let (namespace, event) = reader.read_resolved_event().map_err(xml_guard::map_error)?;
            let (drawingml, compatibility) = match namespace {
                ResolveResult::Bound(ns) => (ns.0 == DRAWINGML, ns.0 == MC),
                _ => (false, false),
            };
            match &event {
                Event::Start(start) | Event::Empty(start) => {
                    let empty = matches!(event, Event::Empty(_));
                    let name = start.local_name();
                    let local = name.as_ref();
                    uncertain |= compatibility;
                    if on_path == SCHEME_PATH.len() {
                        // Inside the scheme: slots, their colours, then colour transforms.
                        uncertain |= !drawingml;
                        match depth - on_path {
                            0 => {
                                scheme.open(SchemeSlot::from_name(local).filter(|_| drawingml));
                                if empty {
                                    scheme.close();
                                }
                            }
                            1 => {
                                let attribute = match (drawingml, local) {
                                    (true, "srgbClr") => Some("val"),
                                    (true, "sysClr") => Some("lastClr"),
                                    _ => None,
                                };
                                let colour = attribute
                                    .and_then(|attribute| unqualified(start, attribute))
                                    .and_then(|value| parse_hex(&value));
                                scheme.colour(colour);
                                scheme.form = Some(Form::read(local, start));
                            }
                            // A colour transform (`a:lumMod`, `a:tint` ...) changes the colour.
                            2 => {
                                scheme.plain = false;
                                if let Some(form) = &mut scheme.form {
                                    if form.transforms.len() >= MAX_TRANSFORMS {
                                        form.base = Base::Undecided;
                                    } else {
                                        form.transforms
                                            .push((local.to_owned(), unqualified(start, "val")));
                                    }
                                }
                            }
                            // Nothing in DrawingML stands inside a transform.
                            _ => {
                                scheme.plain = false;
                                scheme.nested = true;
                            }
                        }
                    } else if depth == on_path && drawingml && local == SCHEME_PATH[on_path] {
                        on_path += 1;
                        if on_path == SCHEME_PATH.len() {
                            schemes += 1;
                        }
                        if empty {
                            on_path -= 1;
                        }
                    }
                    if !empty {
                        depth += 1;
                    }
                }
                Event::End(_) => {
                    depth -= 1;
                    if on_path == SCHEME_PATH.len() && depth == on_path {
                        scheme.close();
                    }
                    on_path = on_path.min(depth);
                }
                Event::Eof => break,
                _ => {}
            }
        }
        if uncertain || schemes > 1 {
            return Ok(ThemeColours {
                colours: [None; 12],
                candidates: Default::default(),
            });
        }
        let candidates = scheme.candidates();
        Ok(ThemeColours {
            colours: scheme.colours,
            candidates,
        })
    }

    /// The theme's own colour for `slot`, if the part defines it plainly.
    pub fn slot(&self, slot: SchemeSlot) -> Option<Rgb> {
        self.colours[slot as usize]
    }

    /// Every colour `slot` may be drawn in, whatever DrawingML colour form and transforms the
    /// part writes it with (see `Form::shades`); `None` when the part leaves it undecided: not
    /// written, written twice, written in a form or with a transform the reader cannot resolve,
    /// or translucent.
    pub fn candidates(&self, slot: SchemeSlot) -> Option<&[Rgb]> {
        self.candidates[slot as usize].as_deref()
    }

    /// `slot`'s colour with `tint` (−1.0 darkens to black, 1.0 lightens to white) applied to its
    /// HSL luminance, as ECMA-376 Part 1 §18.8.19 states. A slot the part leaves unknown
    /// resolves through `office_default()`; callers that judge visibility check `slot` first,
    /// because such a colour also has the RGB value beside it as a candidate.
    pub fn resolve(&self, slot: SchemeSlot, tint: f64) -> Rgb {
        let colour = self
            .slot(slot)
            .or_else(|| ThemeColours::office_default().slot(slot))
            .unwrap_or(Rgb::BLACK);
        apply_tint(colour, tint)
    }
}

/// The slots read so far, and the one being read: its colour, whether it held exactly one
/// colour element with no transform, its colour element and transforms as written, and which
/// slots were already written.
#[derive(Default)]
struct Scheme {
    colours: [Option<Rgb>; 12],
    forms: [Option<Form>; 12],
    written: [bool; 12],
    current: Option<SchemeSlot>,
    colour: Option<Rgb>,
    colours_seen: u32,
    plain: bool,
    form: Option<Form>,
    /// An element stood inside a transform.
    nested: bool,
}

impl Scheme {
    fn open(&mut self, slot: Option<SchemeSlot>) {
        (self.current, self.colour, self.colours_seen, self.plain) = (slot, None, 0, true);
        (self.form, self.nested) = (None, false);
    }

    fn colour(&mut self, colour: Option<Rgb>) {
        self.colours_seen += 1;
        self.colour = colour;
    }

    fn close(&mut self) {
        if let Some(slot) = self.current.take() {
            let index = slot as usize;
            // A slot written twice, or not as one plain colour, stays unknown; one written twice,
            // with no colour or two, or with markup inside a transform, stays undecided.
            let once = self.colours_seen == 1 && !self.written[index];
            self.colours[index] = self.colour.filter(|_| self.plain && once);
            self.forms[index] = self.form.take().filter(|_| once && !self.nested);
            self.written[index] = true;
        }
    }

    /// Each slot's candidates: a `a:schemeClr` slot takes the colours of the slot it names
    /// (one that names another such slot, or itself, stays undecided), then its own transforms.
    fn candidates(&self) -> [Option<Vec<Rgb>>; 12] {
        let own: Vec<Option<Vec<Shade>>> = self
            .forms
            .iter()
            .map(|form| match form {
                Some(Form {
                    base: Base::Shades(shades),
                    transforms,
                }) => Form::transform(shades.clone(), transforms),
                _ => None,
            })
            .collect();
        std::array::from_fn(|index| {
            let form = self.forms[index].as_ref()?;
            let shades = match &form.base {
                Base::Shades(_) => own[index].clone()?,
                Base::Slot(named) => match &self.forms[*named as usize] {
                    Some(Form {
                        base: Base::Shades(_),
                        ..
                    }) => Form::transform(own[*named as usize].clone()?, &form.transforms)?,
                    _ => return None,
                },
                Base::Undecided => return None,
            };
            opaque(&shades)
        })
    }
}

/// Transforms a theme slot may hold before the reader leaves it undecided: Office writes none.
const MAX_TRANSFORMS: usize = 64;
/// Candidates one slot may have before the reader leaves it undecided.
const MAX_SHADES: usize = 64;

/// A colour as a transform sees it: its sRGB channels from 0 to 1, and its opacity.
type Shade = ([f64; 3], f64);

/// A slot's colour element (`CT_Color`, ECMA-376 Part 1 §20.1.2.3) and its transforms, as
/// written.
#[derive(Debug, Clone)]
struct Form {
    base: Base,
    transforms: Vec<(String, Option<String>)>,
}

#[derive(Debug, Clone)]
enum Base {
    /// The colours the element may be drawn in.
    Shades(Vec<Shade>),
    /// `a:schemeClr`: another slot's colour.
    Slot(SchemeSlot),
    /// A form, or a value, the reader cannot resolve.
    Undecided,
}

impl Form {
    /// The colour element `local` (in DrawingML's namespace) at `start`: `a:srgbClr`,
    /// `a:scrgbClr` (linear light), `a:hslClr`, `a:prstClr`, `a:schemeClr`, or `a:sysClr`, drawn in
    /// the system colour its `val` names (Office's `window` white and `windowText` black), its
    /// `lastClr` (the colour its writer last saw) a second candidate.
    fn read(local: &str, start: &quick_xml::events::BytesStart) -> Form {
        let attribute = |name: &str| unqualified(start, name);
        let hex = |value: Option<String>| {
            value
                .filter(|value| value.len() == 6)
                .and_then(|value| parse_hex(&value))
        };
        let shade = |colour: Rgb| {
            (
                [colour.r, colour.g, colour.b].map(|c| f64::from(c) / 255.0),
                1.0,
            )
        };
        let base = match local {
            "srgbClr" => hex(attribute("val")).map(|colour| vec![shade(colour)]),
            "scrgbClr" => {
                let [r, g, b] =
                    ["r", "g", "b"].map(|name| attribute(name).as_deref().and_then(percent));
                r.zip(g)
                    .zip(b)
                    .map(|((r, g), b)| vec![([r, g, b].map(|c| encode(c.clamp(0.0, 1.0))), 1.0)])
            }
            "hslClr" => {
                let hue = attribute("hue").as_deref().and_then(angle);
                let [sat, lum] =
                    ["sat", "lum"].map(|name| attribute(name).as_deref().and_then(percent));
                hue.zip(sat).zip(lum).map(|((hue, sat), lum)| {
                    vec![(
                        hsl_to_rgb(hue, sat.clamp(0.0, 1.0), lum.clamp(0.0, 1.0)),
                        1.0,
                    )]
                })
            }
            "prstClr" => attribute("val")
                .as_deref()
                .and_then(preset)
                .map(|colour| vec![shade(colour)]),
            "sysClr" => {
                let named = match attribute("val").as_deref() {
                    Some("window") => Some(Rgb::WHITE),
                    Some("windowText") => Some(Rgb::BLACK),
                    _ => None,
                };
                let last = attribute("lastClr");
                match (named, &last, hex(last.clone())) {
                    (Some(named), None, _) => Some(vec![shade(named)]),
                    (Some(named), Some(_), Some(last)) => Some(vec![shade(named), shade(last)]),
                    _ => None,
                }
            }
            "schemeClr" => {
                return Form {
                    base: attribute("val")
                        .as_deref()
                        .and_then(scheme_reference)
                        .map_or(Base::Undecided, Base::Slot),
                    transforms: Vec::new(),
                };
            }
            _ => None,
        };
        Form {
            base: base.map_or(Base::Undecided, Base::Shades),
            transforms: Vec::new(),
        }
    }

    /// `shades` through `transforms` in order (ECMA-376 Part 1 §20.1.2.3): the HSL ones
    /// (`hue`, `sat`, `lum`, their `Mod` and `Off`, `comp`) on the colour's HSL values; the ones
    /// on channels (`tint`, `shade`, `inv`, `gray`, `red`, `green`, `blue`, their `Mod` and
    /// `Off`) in sRGB and in linear light, both candidates, since which one Office uses is not
    /// documented for each; `gamma` and `invGamma` give the colour with and without them; the
    /// `alpha` ones set its opacity. `None` for a transform or value it does not know.
    fn transform(
        mut shades: Vec<Shade>,
        transforms: &[(String, Option<String>)],
    ) -> Option<Vec<Shade>> {
        for (name, value) in transforms {
            let value = value.as_deref();
            let mut out = Vec::new();
            for (colour, alpha) in shades {
                match name.as_str() {
                    "alpha" => out.push((colour, percent(value?)?)),
                    "alphaMod" => out.push((colour, alpha * percent(value?)?)),
                    "alphaOff" => out.push((colour, alpha + percent(value?)?)),
                    "hue" | "hueMod" | "hueOff" | "sat" | "satMod" | "satOff" | "lum"
                    | "lumMod" | "lumOff" | "comp" => {
                        let (mut hue, mut sat, mut lum) = rgb_to_hsl(colour);
                        match name.as_str() {
                            "hue" => hue = angle(value?)?,
                            "hueMod" => hue *= percent(value?)?,
                            "hueOff" => hue += angle(value?)?,
                            "sat" => sat = percent(value?)?,
                            "satMod" => sat *= percent(value?)?,
                            "satOff" => sat += percent(value?)?,
                            "lum" => lum = percent(value?)?,
                            "lumMod" => lum *= percent(value?)?,
                            "lumOff" => lum += percent(value?)?,
                            _ => hue += 0.5,
                        }
                        let colour = hsl_to_rgb(
                            hue.rem_euclid(1.0),
                            sat.clamp(0.0, 1.0),
                            lum.clamp(0.0, 1.0),
                        );
                        out.push((colour, alpha));
                    }
                    "gamma" => {
                        out.push((colour, alpha));
                        out.push((colour.map(encode), alpha));
                    }
                    "invGamma" => {
                        out.push((colour, alpha));
                        out.push((colour.map(decode), alpha));
                    }
                    channels => {
                        let amount = match channels {
                            "inv" | "gray" => 0.0,
                            _ => percent(value?)?,
                        };
                        for linear in [false, true] {
                            let mut c = if linear { colour.map(decode) } else { colour };
                            match channels {
                                "tint" => c = c.map(|v| v * amount + (1.0 - amount)),
                                "shade" => c = c.map(|v| v * amount),
                                "inv" => c = c.map(|v| 1.0 - v),
                                "gray" => c = [0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]; 3],
                                "red" => c[0] = amount,
                                "redMod" => c[0] *= amount,
                                "redOff" => c[0] += amount,
                                "green" => c[1] = amount,
                                "greenMod" => c[1] *= amount,
                                "greenOff" => c[1] += amount,
                                "blue" => c[2] = amount,
                                "blueMod" => c[2] *= amount,
                                "blueOff" => c[2] += amount,
                                _ => return None,
                            }
                            let c = c.map(|v| v.clamp(0.0, 1.0));
                            out.push((if linear { c.map(encode) } else { c }, alpha));
                        }
                    }
                }
            }
            if out.len() > MAX_SHADES {
                return None;
            }
            shades = out;
        }
        Some(shades)
    }
}

/// The colours of opaque `shades`, as `Rgb`, each once; `None` when one is translucent (what
/// shows through it is not known) or there are too many.
fn opaque(shades: &[Shade]) -> Option<Vec<Rgb>> {
    let mut colours = Vec::new();
    for (colour, alpha) in shades {
        if *alpha < 1.0 - 1e-9 || colour.iter().any(|c| !c.is_finite()) {
            return None;
        }
        let [r, g, b] = colour.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
        let colour = Rgb { r, g, b };
        if !colours.contains(&colour) {
            colours.push(colour);
        }
    }
    (!colours.is_empty() && colours.len() <= MAX_SHADES).then_some(colours)
}

/// A DrawingML percentage: thousandths of a percent (`50000`), or Strict's `50%`.
fn percent(value: &str) -> Option<f64> {
    let (number, scale) = match value.strip_suffix('%') {
        Some(number) => (number, 100.0),
        None => (value, 100_000.0),
    };
    let digits = number.strip_prefix('-').unwrap_or(number);
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    number
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .map(|value| value / scale)
}

/// A DrawingML angle, in 60,000ths of a degree, as a share of a full turn.
fn angle(value: &str) -> Option<f64> {
    let digits = value.strip_prefix('-').unwrap_or(value);
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    value.parse::<f64>().ok().map(|value| value / 21_600_000.0)
}

/// The slot a `a:schemeClr` names; its text and background aliases name the dark and light
/// slots. `phClr`, a placeholder for a colour a style supplies, names none.
fn scheme_reference(name: &str) -> Option<SchemeSlot> {
    match name {
        "tx1" => Some(SchemeSlot::Dk1),
        "bg1" => Some(SchemeSlot::Lt1),
        "tx2" => Some(SchemeSlot::Dk2),
        "bg2" => Some(SchemeSlot::Lt2),
        name => SchemeSlot::from_name(name),
    }
}

/// sRGB's transfer function, from linear light.
fn encode(c: f64) -> f64 {
    if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Linear light, from an sRGB channel.
fn decode(c: f64) -> f64 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// `a:prstClr`'s colours (ECMA-376 Part 1 §20.1.10.47, ST_PresetColorVal), the CSS colours
/// under DrawingML's names and their `dk`, `lt` and `med` short forms.
fn preset(name: &str) -> Option<Rgb> {
    const PRESETS: &[(&str, u32)] = &[
        ("aliceBlue", 0xF0F8FF),
        ("antiqueWhite", 0xFAEBD7),
        ("aqua", 0x00FFFF),
        ("aquamarine", 0x7FFFD4),
        ("azure", 0xF0FFFF),
        ("beige", 0xF5F5DC),
        ("bisque", 0xFFE4C4),
        ("black", 0x000000),
        ("blanchedAlmond", 0xFFEBCD),
        ("blue", 0x0000FF),
        ("blueViolet", 0x8A2BE2),
        ("brown", 0xA52A2A),
        ("burlyWood", 0xDEB887),
        ("cadetBlue", 0x5F9EA0),
        ("chartreuse", 0x7FFF00),
        ("chocolate", 0xD2691E),
        ("coral", 0xFF7F50),
        ("cornflowerBlue", 0x6495ED),
        ("cornsilk", 0xFFF8DC),
        ("crimson", 0xDC143C),
        ("cyan", 0x00FFFF),
        ("darkBlue", 0x00008B),
        ("darkCyan", 0x008B8B),
        ("darkGoldenrod", 0xB8860B),
        ("darkGray", 0xA9A9A9),
        ("darkGrey", 0xA9A9A9),
        ("darkGreen", 0x006400),
        ("darkKhaki", 0xBDB76B),
        ("darkMagenta", 0x8B008B),
        ("darkOliveGreen", 0x556B2F),
        ("darkOrange", 0xFF8C00),
        ("darkOrchid", 0x9932CC),
        ("darkRed", 0x8B0000),
        ("darkSalmon", 0xE9967A),
        ("darkSeaGreen", 0x8FBC8F),
        ("darkSlateBlue", 0x483D8B),
        ("darkSlateGray", 0x2F4F4F),
        ("darkSlateGrey", 0x2F4F4F),
        ("darkTurquoise", 0x00CED1),
        ("darkViolet", 0x9400D3),
        ("deepPink", 0xFF1493),
        ("deepSkyBlue", 0x00BFFF),
        ("dimGray", 0x696969),
        ("dimGrey", 0x696969),
        ("dkBlue", 0x00008B),
        ("dkCyan", 0x008B8B),
        ("dkGoldenrod", 0xB8860B),
        ("dkGray", 0xA9A9A9),
        ("dkGrey", 0xA9A9A9),
        ("dkGreen", 0x006400),
        ("dkKhaki", 0xBDB76B),
        ("dkMagenta", 0x8B008B),
        ("dkOliveGreen", 0x556B2F),
        ("dkOrange", 0xFF8C00),
        ("dkOrchid", 0x9932CC),
        ("dkRed", 0x8B0000),
        ("dkSalmon", 0xE9967A),
        ("dkSeaGreen", 0x8FBC8F),
        ("dkSlateBlue", 0x483D8B),
        ("dkSlateGray", 0x2F4F4F),
        ("dkSlateGrey", 0x2F4F4F),
        ("dkTurquoise", 0x00CED1),
        ("dkViolet", 0x9400D3),
        ("dodgerBlue", 0x1E90FF),
        ("firebrick", 0xB22222),
        ("floralWhite", 0xFFFAF0),
        ("forestGreen", 0x228B22),
        ("fuchsia", 0xFF00FF),
        ("gainsboro", 0xDCDCDC),
        ("ghostWhite", 0xF8F8FF),
        ("gold", 0xFFD700),
        ("goldenrod", 0xDAA520),
        ("gray", 0x808080),
        ("grey", 0x808080),
        ("green", 0x008000),
        ("greenYellow", 0xADFF2F),
        ("honeydew", 0xF0FFF0),
        ("hotPink", 0xFF69B4),
        ("indianRed", 0xCD5C5C),
        ("indigo", 0x4B0082),
        ("ivory", 0xFFFFF0),
        ("khaki", 0xF0E68C),
        ("lavender", 0xE6E6FA),
        ("lavenderBlush", 0xFFF0F5),
        ("lawnGreen", 0x7CFC00),
        ("lemonChiffon", 0xFFFACD),
        ("lightBlue", 0xADD8E6),
        ("lightCoral", 0xF08080),
        ("lightCyan", 0xE0FFFF),
        ("lightGoldenrodYellow", 0xFAFAD2),
        ("lightGray", 0xD3D3D3),
        ("lightGrey", 0xD3D3D3),
        ("lightGreen", 0x90EE90),
        ("lightPink", 0xFFB6C1),
        ("lightSalmon", 0xFFA07A),
        ("lightSeaGreen", 0x20B2AA),
        ("lightSkyBlue", 0x87CEFA),
        ("lightSlateGray", 0x778899),
        ("lightSlateGrey", 0x778899),
        ("lightSteelBlue", 0xB0C4DE),
        ("lightYellow", 0xFFFFE0),
        ("lime", 0x00FF00),
        ("limeGreen", 0x32CD32),
        ("linen", 0xFAF0E6),
        ("ltBlue", 0xADD8E6),
        ("ltCoral", 0xF08080),
        ("ltCyan", 0xE0FFFF),
        ("ltGoldenrodYellow", 0xFAFAD2),
        ("ltGray", 0xD3D3D3),
        ("ltGrey", 0xD3D3D3),
        ("ltGreen", 0x90EE90),
        ("ltPink", 0xFFB6C1),
        ("ltSalmon", 0xFFA07A),
        ("ltSeaGreen", 0x20B2AA),
        ("ltSkyBlue", 0x87CEFA),
        ("ltSlateGray", 0x778899),
        ("ltSlateGrey", 0x778899),
        ("ltSteelBlue", 0xB0C4DE),
        ("ltYellow", 0xFFFFE0),
        ("magenta", 0xFF00FF),
        ("maroon", 0x800000),
        ("medAquamarine", 0x66CDAA),
        ("medBlue", 0x0000CD),
        ("medOrchid", 0xBA55D3),
        ("medPurple", 0x9370DB),
        ("medSeaGreen", 0x3CB371),
        ("medSlateBlue", 0x7B68EE),
        ("medSpringGreen", 0x00FA9A),
        ("medTurquoise", 0x48D1CC),
        ("medVioletRed", 0xC71585),
        ("mediumAquamarine", 0x66CDAA),
        ("mediumBlue", 0x0000CD),
        ("mediumOrchid", 0xBA55D3),
        ("mediumPurple", 0x9370DB),
        ("mediumSeaGreen", 0x3CB371),
        ("mediumSlateBlue", 0x7B68EE),
        ("mediumSpringGreen", 0x00FA9A),
        ("mediumTurquoise", 0x48D1CC),
        ("mediumVioletRed", 0xC71585),
        ("midnightBlue", 0x191970),
        ("mintCream", 0xF5FFFA),
        ("mistyRose", 0xFFE4E1),
        ("moccasin", 0xFFE4B5),
        ("navajoWhite", 0xFFDEAD),
        ("navy", 0x000080),
        ("oldLace", 0xFDF5E6),
        ("olive", 0x808000),
        ("oliveDrab", 0x6B8E23),
        ("orange", 0xFFA500),
        ("orangeRed", 0xFF4500),
        ("orchid", 0xDA70D6),
        ("paleGoldenrod", 0xEEE8AA),
        ("paleGreen", 0x98FB98),
        ("paleTurquoise", 0xAFEEEE),
        ("paleVioletRed", 0xDB7093),
        ("papayaWhip", 0xFFEFD5),
        ("peachPuff", 0xFFDAB9),
        ("peru", 0xCD853F),
        ("pink", 0xFFC0CB),
        ("plum", 0xDDA0DD),
        ("powderBlue", 0xB0E0E6),
        ("purple", 0x800080),
        ("red", 0xFF0000),
        ("rosyBrown", 0xBC8F8F),
        ("royalBlue", 0x4169E1),
        ("saddleBrown", 0x8B4513),
        ("salmon", 0xFA8072),
        ("sandyBrown", 0xF4A460),
        ("seaGreen", 0x2E8B57),
        ("seaShell", 0xFFF5EE),
        ("sienna", 0xA0522D),
        ("silver", 0xC0C0C0),
        ("skyBlue", 0x87CEEB),
        ("slateBlue", 0x6A5ACD),
        ("slateGray", 0x708090),
        ("slateGrey", 0x708090),
        ("snow", 0xFFFAFA),
        ("springGreen", 0x00FF7F),
        ("steelBlue", 0x4682B4),
        ("tan", 0xD2B48C),
        ("teal", 0x008080),
        ("thistle", 0xD8BFD8),
        ("tomato", 0xFF6347),
        ("turquoise", 0x40E0D0),
        ("violet", 0xEE82EE),
        ("wheat", 0xF5DEB3),
        ("white", 0xFFFFFF),
        ("whiteSmoke", 0xF5F5F5),
        ("yellow", 0xFFFF00),
        ("yellowGreen", 0x9ACD32),
    ];
    PRESETS
        .iter()
        .find(|(preset, _)| *preset == name)
        .map(|&(_, value)| Rgb {
            r: (value >> 16) as u8,
            g: (value >> 8) as u8,
            b: value as u8,
        })
}

fn unqualified(start: &quick_xml::events::BytesStart, name: &str) -> Option<String> {
    start
        .attributes()
        .flatten()
        .find(|attribute| attribute.key.0 == name)
        .and_then(|attribute| {
            xml_guard::attribute_value(&attribute.value)
                .ok()
                .map(|v| v.into_owned())
        })
}

/// `colour` with `tint` applied to its HSL luminance (ECMA-376 Part 1 §18.8.19): the XLSX reader
/// applies a `tint` to RGB and palette colours too.
pub(crate) fn apply_tint(colour: Rgb, tint: f64) -> Rgb {
    if !tint.is_finite() || tint == 0.0 {
        return colour;
    }
    let tint = tint.clamp(-1.0, 1.0);
    let (hue, saturation, lightness) = to_hsl(colour);
    let lightness = if tint < 0.0 {
        lightness * (1.0 + tint)
    } else {
        lightness * (1.0 - tint) + tint
    };
    from_hsl(hue, saturation, lightness.clamp(0.0, 1.0))
}

fn to_hsl(colour: Rgb) -> (f64, f64, f64) {
    rgb_to_hsl([colour.r, colour.g, colour.b].map(|c| f64::from(c) / 255.0))
}

/// Hue (a share of a turn), saturation and lightness of sRGB channels from 0 to 1.
fn rgb_to_hsl([r, g, b]: [f64; 3]) -> (f64, f64, f64) {
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let lightness = (max + min) / 2.0;
    if max == min {
        return (0.0, 0.0, lightness);
    }
    let delta = max - min;
    let saturation = if lightness > 0.5 {
        delta / (2.0 - max - min)
    } else {
        delta / (max + min)
    };
    let hue = if max == r {
        (g - b) / delta + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    };
    (hue / 6.0, saturation, lightness)
}

fn from_hsl(hue: f64, saturation: f64, lightness: f64) -> Rgb {
    let channel = |value: f64| (value * 255.0).round().clamp(0.0, 255.0) as u8;
    let [r, g, b] = hsl_to_rgb(hue, saturation, lightness).map(channel);
    Rgb { r, g, b }
}

/// sRGB channels from 0 to 1 of a hue (a share of a turn), saturation and lightness.
fn hsl_to_rgb(hue: f64, saturation: f64, lightness: f64) -> [f64; 3] {
    if saturation == 0.0 {
        return [lightness; 3];
    }
    let q = if lightness < 0.5 {
        lightness * (1.0 + saturation)
    } else {
        lightness + saturation - lightness * saturation
    };
    let p = 2.0 * lightness - q;
    let component = |t: f64| {
        let t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [
        component(hue + 1.0 / 3.0),
        component(hue),
        component(hue - 1.0 / 3.0),
    ]
}
