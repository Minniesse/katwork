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
/// `a:sysClr` with `lastClr` stays unknown: `slot` is `None` for it, and a colour naming it has
/// two candidates, the RGB value beside it and `office_default()`'s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeColours {
    colours: [Option<Rgb>; 12],
}

impl ThemeColours {
    /// The scheme of a new Word document.
    pub fn office_default() -> ThemeColours {
        let hex = |s| parse_hex(s);
        ThemeColours {
            colours: [
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
            ],
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
                            }
                            // A colour transform (`a:lumMod`, `a:tint` ...) changes the colour.
                            _ => scheme.plain = false,
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
            });
        }
        Ok(ThemeColours {
            colours: scheme.colours,
        })
    }

    /// The theme's own colour for `slot`, if the part defines it plainly.
    pub fn slot(&self, slot: SchemeSlot) -> Option<Rgb> {
        self.colours[slot as usize]
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
/// colour element with no transform, and which slots were already written.
#[derive(Default)]
struct Scheme {
    colours: [Option<Rgb>; 12],
    written: [bool; 12],
    current: Option<SchemeSlot>,
    colour: Option<Rgb>,
    colours_seen: u32,
    plain: bool,
}

impl Scheme {
    fn open(&mut self, slot: Option<SchemeSlot>) {
        (self.current, self.colour, self.colours_seen, self.plain) = (slot, None, 0, true);
    }

    fn colour(&mut self, colour: Option<Rgb>) {
        self.colours_seen += 1;
        self.colour = colour;
    }

    fn close(&mut self) {
        if let Some(slot) = self.current.take() {
            let index = slot as usize;
            // A slot written twice, or not as one plain colour, stays unknown.
            self.colours[index] = self
                .colour
                .filter(|_| self.plain && self.colours_seen == 1 && !self.written[index]);
            self.written[index] = true;
        }
    }
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
    let [r, g, b] = [colour.r, colour.g, colour.b].map(|c| f64::from(c) / 255.0);
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
    if saturation == 0.0 {
        let grey = channel(lightness);
        return Rgb {
            r: grey,
            g: grey,
            b: grey,
        };
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
    Rgb {
        r: channel(component(hue + 1.0 / 3.0)),
        g: channel(component(hue)),
        b: channel(component(hue - 1.0 / 3.0)),
    }
}
