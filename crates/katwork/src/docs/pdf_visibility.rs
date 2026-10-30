//! Whether a person sees each PDF character (U2). Colours are resolved from the colour space
//! and operands the content stream set (`resolve`), and `classify` gives one verdict per
//! character from the paint state the walker reported and the areas painted before and after
//! it. When the rules cannot decide, a character is hidden, or suspect when only what lies
//! beneath it is unknown, never plainly visible.
//!
//! Fixed rules of `pdf-text` version 1 (a change is a new version): a character's box spans its
//! advance by −0.2 to 0.8 of its font size in text space, bounded in user space; "over its
//! centre" means an area's bounding box contains the box's centre; the effective height is
//! `|Tfs| × |det M| / |M's x basis|` and the effective width `|Tfs| × Tz/100 × |M's x basis|`
//! with `M = Tm × CTM`; composites are computed per sRGB channel in f64 and rounded half up to
//! a byte before the contrast ratio.
use std::rc::Rc;

pub use pdf_extract::{BlendMode, ColourSpace, Function};

use super::{
    DocError, DocLimits, HiddenWhy, PageInfo,
    colour::{Rgb, contrast_ratio},
    deadline,
};

/// One character's verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Visible,
    Suspect(&'static str),
    Hidden(HiddenWhy),
}

/// A colour as the content stream set it.
#[derive(Debug, Clone, PartialEq)]
pub struct Paint {
    pub space: Rc<ColourSpace>,
    pub operands: Vec<f32>,
}

impl Paint {
    pub fn gray(value: f32) -> Paint {
        Paint {
            space: Rc::new(ColourSpace::DeviceGray),
            operands: vec![value],
        }
    }

    pub fn rgb(r: f32, g: f32, b: f32) -> Paint {
        Paint {
            space: Rc::new(ColourSpace::DeviceRGB),
            operands: vec![r, g, b],
        }
    }

    fn resolve(&self) -> Option<Rgb> {
        resolve(&self.space, &self.operands)
    }

    /// A Separation `/None`, or a DeviceN whose every colorant is `/None`: it never marks.
    fn never_marks(&self) -> bool {
        match &*self.space {
            ColourSpace::Separation { colorant, .. } => colorant == b"None",
            ColourSpace::DeviceN { colorants, .. } => colorants.iter().all(|name| name == b"None"),
            _ => false,
        }
    }
}

/// An enclosing transparency group as painted at its `Do`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GroupPaint {
    pub id: u32,
    pub alpha: f32,
    pub soft_mask: bool,
    pub blend: BlendMode,
    pub knockout: bool,
}

impl GroupPaint {
    /// Painted with an alpha below 1, a soft mask or a blend mode other than Normal.
    fn changes_its_content(&self) -> bool {
        self.alpha < 1.0 || self.soft_mask || !self.blend.is_normal()
    }
}

/// One character's paint state. Alphas, the soft mask and the blend mode are folded through
/// its transparency groups; `groups` lists them outermost first.
#[derive(Debug, Clone, PartialEq)]
pub struct CharPaint {
    /// Drawing order, shared with `AreaPaint::order`.
    pub order: u32,
    /// `[llx, lly, urx, ury]` in user space.
    pub bbox: [f32; 4],
    pub render_mode: u8,
    pub fill: Paint,
    pub stroke: Paint,
    pub fill_alpha: f32,
    pub stroke_alpha: f32,
    pub soft_mask: bool,
    pub blend: BlendMode,
    /// The clipping path's bounding box in user space; `None` for no clip.
    pub clip: Option<[f32; 4]>,
    /// Optional content hides it (an off group, or an expression KatWork does not evaluate).
    pub optional_hidden: bool,
    pub font_size: f32,
    /// `Tz / 100`.
    pub horizontal_scaling: f32,
    /// `Tm × CTM` as `[a b c d e f]`.
    pub matrix: [f32; 6],
    pub groups: Rc<[GroupPaint]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AreaKind {
    Path,
    Image,
    InlineImage,
    Shading,
}

/// One painted area.
#[derive(Debug, Clone, PartialEq)]
pub struct AreaPaint {
    pub order: u32,
    pub kind: AreaKind,
    /// Its bounding box in user space, cut to the clip's.
    pub bbox: [f32; 4],
    /// Its fill colour, `None` when unknown (images, shadings, areas under an unevaluated
    /// optional-content expression).
    pub fill: Option<Paint>,
    pub alpha: f32,
    pub soft_mask: bool,
    pub blend: BlendMode,
    /// One axis-aligned rectangle, under a clip made of such rectangles.
    pub rect: bool,
    /// An image with its own mask (`/SMask`, `/Mask` or `/ImageMask true`).
    pub masked: bool,
    pub groups: Rc<[GroupPaint]>,
}

impl AreaPaint {
    /// Alpha 1, no soft mask and no mask of its own.
    fn opaque(&self) -> bool {
        self.alpha >= 1.0 && !self.soft_mask && !self.masked
    }

    fn known(&self) -> Option<Rgb> {
        self.fill.as_ref().and_then(Paint::resolve)
    }

    /// An opaque rectangle of a known colour painted with blend mode Normal: the only kind of
    /// area whose colour a character's background can be.
    fn plain_background(&self) -> Option<Rgb> {
        if self.kind == AreaKind::Path && self.rect && self.opaque() && self.blend.is_normal() {
            self.known()
        } else {
            None
        }
    }
}

fn byte(value: f64) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0 + 0.5).floor() as u8
}

fn gray(value: f64) -> Rgb {
    let value = byte(value);
    Rgb {
        r: value,
        g: value,
        b: value,
    }
}

fn cmyk(c: f64, m: f64, y: f64, k: f64) -> Rgb {
    let channel = |value: f64| byte((1.0 - value.clamp(0.0, 1.0)) * (1.0 - k.clamp(0.0, 1.0)));
    Rgb {
        r: channel(c),
        g: channel(m),
        b: channel(y),
    }
}

/// The sRGB colour of `operands` in `space`, or `None` when it cannot be known: `Lab`,
/// `Pattern`, an ICC profile with no usable `/N`, an Indexed table too short for its `hival`,
/// a tint transform other than Type 2 or one-input Type 0, or operands that do not fit.
/// DeviceCMYK converts naively (`r = 255 × (1 − c) × (1 − k)`); an ICC profile is read by its
/// `/N` only; a Separation `/None` has no colour (classify calls it invisible).
pub fn resolve(space: &ColourSpace, operands: &[f32]) -> Option<Rgb> {
    let values: Vec<f64> = operands.iter().map(|value| f64::from(*value)).collect();
    resolve_depth(space, &values, 0)
}

fn resolve_depth(space: &ColourSpace, values: &[f64], depth: usize) -> Option<Rgb> {
    if depth > 4 || values.iter().any(|value| !value.is_finite()) {
        return None;
    }
    match (space, values) {
        (ColourSpace::DeviceGray | ColourSpace::CalGray | ColourSpace::IccBased { n: 1 }, [g]) => Some(gray(*g)),
        (ColourSpace::DeviceRGB | ColourSpace::CalRGB | ColourSpace::IccBased { n: 3 }, [r, g, b]) => Some(Rgb {
            r: byte(*r),
            g: byte(*g),
            b: byte(*b),
        }),
        (ColourSpace::DeviceCMYK | ColourSpace::IccBased { n: 4 }, [c, m, y, k]) => Some(cmyk(*c, *m, *y, *k)),
        (ColourSpace::Indexed { base, hival, lookup }, [index]) => {
            let width = base.components();
            let entries = usize::try_from(*hival).ok()?.checked_add(1)?;
            if *hival > 255 || width == 0 || lookup.len() < width.checked_mul(entries)? {
                return None;
            }
            let index = (index.round().max(0.0) as usize).min(entries - 1);
            let entry = lookup.get(index * width..(index + 1) * width)?;
            let components: Vec<f64> = entry.iter().map(|byte| f64::from(*byte) / 255.0).collect();
            resolve_depth(base, &components, depth + 1)
        }
        (ColourSpace::Separation { colorant, alternate, tint }, [value]) => {
            if colorant == b"None" {
                return None;
            }
            resolve_depth(alternate, &evaluate(tint, *value)?, depth + 1)
        }
        (ColourSpace::DeviceN { colorants, alternate, tint }, [value]) if colorants.len() == 1 => {
            resolve_depth(alternate, &evaluate(tint, *value)?, depth + 1)
        }
        _ => None,
    }
}

/// A one-input tint transform: Type 2, or Type 0 with one input and linear interpolation.
fn evaluate(function: &Function, x: f64) -> Option<Vec<f64>> {
    match function {
        Function::Exponential { domain, c0, c1, n, range } => {
            if c0.len() != c1.len() || c0.is_empty() {
                return None;
            }
            let x = x.clamp(domain[0].min(domain[1]), domain[0].max(domain[1]));
            let power = x.powf(*n);
            let mut out: Vec<f64> = c0.iter().zip(c1).map(|(a, b)| a + power * (b - a)).collect();
            if let Some(range) = range {
                clip_to(&mut out, range)?;
            }
            out.iter().all(|value| value.is_finite()).then_some(out)
        }
        Function::Sampled { domain, range, size, bits_per_sample, order, encode, decode, samples } => {
            let ([d0, d1], [size], [e0, e1]) = (domain.as_slice(), size.as_slice(), encode.get(..2)?) else {
                return None;
            };
            let outputs = range.len() / 2;
            let bits = *bits_per_sample;
            if *order != 1 || outputs == 0 || decode.len() < outputs * 2 || ![1, 2, 4, 8, 12, 16, 24, 32].contains(&bits) {
                return None;
            }
            let last = f64::from(size.checked_sub(1)?);
            let x = x.clamp(d0.min(*d1), d0.max(*d1));
            let e = if d1 == d0 { *e0 } else { e0 + (x - d0) * (e1 - e0) / (d1 - d0) };
            let e = e.clamp(0.0, last);
            let (low, high) = (e.floor() as u64, e.ceil() as u64);
            let max = (1u64 << bits) - 1;
            let sample = |index: u64, output: usize| -> Option<f64> {
                let bit = (index * outputs as u64 + output as u64).checked_mul(u64::from(bits))?;
                let mut value = 0u64;
                for offset in 0..u64::from(bits) {
                    let at = bit + offset;
                    let byte = *samples.get(usize::try_from(at / 8).ok()?)?;
                    value = value << 1 | u64::from(byte >> (7 - at % 8) & 1);
                }
                Some(value as f64)
            };
            let mut out = Vec::with_capacity(outputs);
            for output in 0..outputs {
                let (a, b) = (sample(low, output)?, sample(high, output)?);
                let value = a + (b - a) * (e - low as f64);
                let (lo, hi) = (decode[output * 2], decode[output * 2 + 1]);
                out.push(lo + value * (hi - lo) / max as f64);
            }
            clip_to(&mut out, range)?;
            out.iter().all(|value| value.is_finite()).then_some(out)
        }
        Function::Unsupported => None,
    }
}

fn clip_to(values: &mut [f64], range: &[f64]) -> Option<()> {
    for (index, value) in values.iter_mut().enumerate() {
        let (Some(lo), Some(hi)) = (range.get(index * 2), range.get(index * 2 + 1)) else {
            continue;
        };
        *value = value.clamp(lo.min(*hi), lo.max(*hi));
    }
    Some(())
}

/// `top` laid over `under` at `alpha`, per channel in f64.
fn over(top: [f64; 3], under: [f64; 3], alpha: f64) -> [f64; 3] {
    let alpha = alpha.clamp(0.0, 1.0);
    [0, 1, 2].map(|channel| alpha * top[channel] + (1.0 - alpha) * under[channel])
}

fn channels(colour: Rgb) -> [f64; 3] {
    [f64::from(colour.r), f64::from(colour.g), f64::from(colour.b)]
}

fn rounded(channels: [f64; 3]) -> Rgb {
    let round = |value: f64| (value.clamp(0.0, 255.0) + 0.5).floor() as u8;
    Rgb {
        r: round(channels[0]),
        g: round(channels[1]),
        b: round(channels[2]),
    }
}

/// Grid cells per side of the spatial index over the page.
const GRID: usize = 32;
/// An area wider than this many cells is checked against every character instead.
const MAX_CELLS: usize = 64;
/// Area-character comparisons one page may cost.
const MAX_COMPARISONS: u64 = 10_000_000;

fn contains_point(rect: [f32; 4], (x, y): (f32, f32)) -> bool {
    rect[0] <= x && x <= rect[2] && rect[1] <= y && y <= rect[3]
}

fn contains_rect(outer: [f32; 4], inner: [f32; 4]) -> bool {
    outer[0] <= inner[0] && inner[2] <= outer[2] && outer[1] <= inner[1] && inner[3] <= outer[3]
}

fn centre(rect: [f32; 4]) -> (f32, f32) {
    ((rect[0] + rect[2]) / 2.0, (rect[1] + rect[3]) / 2.0)
}

/// The groups two items share: the common start of their chains.
fn shared<'a>(a: &'a [GroupPaint], b: &[GroupPaint]) -> &'a [GroupPaint] {
    let common = a.iter().zip(b).take_while(|(x, y)| x.id == y.id).count();
    &a[..common]
}

/// Areas by grid cell, for "which areas lie over this point".
struct Index {
    origin: (f32, f32),
    cell: (f32, f32),
    cells: Vec<Vec<usize>>,
    wide: Vec<usize>,
}

impl Index {
    fn new(areas: &[AreaPaint], page: &PageInfo) -> Index {
        let bounds = page.media_box;
        let cell = (
            ((bounds[2] - bounds[0]) / GRID as f32).max(f32::MIN_POSITIVE),
            ((bounds[3] - bounds[1]) / GRID as f32).max(f32::MIN_POSITIVE),
        );
        let mut index = Index {
            origin: (bounds[0], bounds[1]),
            cell,
            cells: vec![Vec::new(); GRID * GRID],
            wide: Vec::new(),
        };
        for (at, area) in areas.iter().enumerate() {
            let (x0, y0) = index.cell_of((area.bbox[0], area.bbox[1]));
            let (x1, y1) = index.cell_of((area.bbox[2], area.bbox[3]));
            if (x1 - x0 + 1) * (y1 - y0 + 1) > MAX_CELLS {
                index.wide.push(at);
                continue;
            }
            for y in y0..=y1 {
                for x in x0..=x1 {
                    index.cells[y * GRID + x].push(at);
                }
            }
        }
        index
    }

    fn cell_of(&self, (x, y): (f32, f32)) -> (usize, usize) {
        let clamp = |value: f32| (value.max(0.0) as usize).min(GRID - 1);
        (
            clamp((x - self.origin.0) / self.cell.0),
            clamp((y - self.origin.1) / self.cell.1),
        )
    }

    /// The cell's areas and the wide ones: each list is in drawing order.
    fn lists(&self, point: (f32, f32)) -> (&[usize], &[usize]) {
        let (x, y) = self.cell_of(point);
        (&self.cells[y * GRID + x], &self.wide)
    }

    /// The areas whose box may hold `point`, in drawing order: the two lists merged.
    fn near(&self, point: (f32, f32)) -> Vec<usize> {
        let (cell, wide) = self.lists(point);
        let mut near = Vec::with_capacity(cell.len() + wide.len());
        let (mut a, mut b) = (0, 0);
        while a < cell.len() || b < wide.len() {
            if b == wide.len() || (a < cell.len() && cell[a] < wide[b]) {
                near.push(cell[a]);
                a += 1;
            } else {
                near.push(wide[b]);
                b += 1;
            }
        }
        near
    }
}

/// One verdict per character, by the first rule that applies (the SP2a plan, Task 5):
/// `InvisibleMode`, `OptionalContentOff`, `OutsideCropBox`, `Clipped`, `Transparent`,
/// `UnknownPaint`, `Tiny`, `LowContrast`, `Covered`, then `Suspect("unknown_background")`.
/// A page that cannot be classified within its budget or deadline hides every character
/// rather than guess; the reader calls `try_classify` and fails the read instead.
pub fn classify(chars: &[CharPaint], paints: &[AreaPaint], page: &PageInfo, limits: &DocLimits) -> Vec<Visibility> {
    try_classify(chars, paints, page, limits)
        .unwrap_or_else(|_| vec![Visibility::Hidden(HiddenWhy::UnknownPaint); chars.len()])
}

/// `classify`, or the error that stopped it (the deadline, or `TooComplex("areas")` past
/// `MAX_COMPARISONS`).
pub(crate) fn try_classify(
    chars: &[CharPaint],
    paints: &[AreaPaint],
    page: &PageInfo,
    limits: &DocLimits,
) -> Result<Vec<Visibility>, DocError> {
    let index = Index::new(paints, page);
    let mut comparisons = 0u64;
    let mut verdicts = Vec::with_capacity(chars.len());
    // Characters inside a knockout group, for the knockout rule.
    let knocked: Vec<usize> = (0..chars.len()).filter(|at| chars[*at].groups.iter().any(|group| group.knockout)).collect();
    for (at, char) in chars.iter().enumerate() {
        if at.is_multiple_of(256) {
            deadline::check()?;
        }
        let point = centre(char.bbox);
        // The budget counts the areas looked at, before any is.
        let (cell, wide) = index.lists(point);
        let in_knockout = char.groups.iter().any(|group| group.knockout);
        comparisons += (cell.len() + wide.len()) as u64 + 1 + if in_knockout { knocked.len() as u64 } else { 0 };
        if comparisons > MAX_COMPARISONS {
            return Err(DocError::TooComplex("areas"));
        }
        let near: Vec<&AreaPaint> = index
            .near(point)
            .into_iter()
            .map(|area| &paints[area])
            .filter(|area| contains_point(area.bbox, point))
            .collect();
        let before = near.iter().rev().find(|area| area.order < char.order).copied();
        let after: Vec<&AreaPaint> = near.into_iter().filter(|area| area.order > char.order).collect();
        let knocked_later = in_knockout
            && knocked.iter().any(|other| {
                let other = &chars[*other];
                other.order > char.order
                    && contains_point(other.bbox, point)
                    && shared(&char.groups, &other.groups).iter().any(|group| group.knockout)
            });
        verdicts.push(verdict(char, before, &after, knocked_later, page, limits));
    }
    Ok(verdicts)
}

/// The paints a render mode uses: fill for 0 and 4, stroke for 1 and 5, both for 2 and 6.
fn painted(char: &CharPaint) -> Vec<(&Paint, f32)> {
    match char.render_mode % 4 {
        0 => vec![(&char.fill, char.fill_alpha)],
        1 => vec![(&char.stroke, char.stroke_alpha)],
        _ => vec![(&char.fill, char.fill_alpha), (&char.stroke, char.stroke_alpha)],
    }
}

fn effective_size(char: &CharPaint) -> (f64, f64) {
    let [a, b, c, d, _, _] = char.matrix.map(f64::from);
    let size = f64::from(char.font_size).abs();
    let along = (a * a + b * b).sqrt();
    let height = if along > 0.0 { size * (a * d - b * c).abs() / along } else { 0.0 };
    let width = size * f64::from(char.horizontal_scaling).abs() * along;
    (height, width)
}

fn verdict(
    char: &CharPaint,
    before: Option<&AreaPaint>,
    after: &[&AreaPaint],
    knocked_later: bool,
    page: &PageInfo,
    limits: &DocLimits,
) -> Visibility {
    use HiddenWhy::*;
    let point = centre(char.bbox);
    let paints = painted(char);
    if matches!(char.render_mode, 3 | 7) || paints.iter().all(|(paint, _)| paint.never_marks()) {
        return Visibility::Hidden(InvisibleMode);
    }
    if char.optional_hidden {
        return Visibility::Hidden(OptionalContentOff);
    }
    if !contains_point(page.crop_box, point) {
        return Visibility::Hidden(OutsideCropBox);
    }
    if char.clip.is_some_and(|clip| !contains_point(clip, point)) {
        return Visibility::Hidden(Clipped);
    }
    if paints.iter().all(|(_, alpha)| *alpha < limits.min_text_alpha) {
        return Visibility::Hidden(Transparent);
    }
    let colours: Option<Vec<(Rgb, f32)>> = paints.iter().map(|(paint, alpha)| paint.resolve().map(|rgb| (rgb, *alpha))).collect();
    let unknown_after = after.iter().any(|area| {
        let groups = shared(&char.groups, &area.groups);
        area.soft_mask
            || !area.blend.is_normal()
            || area.masked
            || (area.alpha < 1.0 && area.known().is_none())
            || groups.iter().any(GroupPaint::changes_its_content)
            || groups.iter().any(|group| group.knockout)
    });
    if colours.is_none() || char.soft_mask || !char.blend.is_normal() || unknown_after || knocked_later {
        return Visibility::Hidden(UnknownPaint);
    }
    let (height, width) = effective_size(char);
    let min = f64::from(limits.min_visible_pt);
    if height < min || width < min {
        return Visibility::Hidden(Tiny);
    }
    // The background: white with nothing beneath, a plain rectangle's colour, else unknown.
    let background = match before {
        None => Some(Rgb::WHITE),
        Some(area) => area.plain_background(),
    };
    if let (Some(background), Some(colours)) = (background, &colours) {
        let low = colours.iter().all(|(colour, alpha)| {
            let mut text = over(channels(*colour), channels(background), f64::from(*alpha));
            let mut under = channels(background);
            for area in after {
                if let (Some(colour), true) = (area.known(), area.alpha < 1.0) {
                    text = over(channels(colour), text, f64::from(area.alpha));
                    under = over(channels(colour), under, f64::from(area.alpha));
                }
            }
            contrast_ratio(rounded(text), rounded(under)) < limits.min_contrast
        });
        if low {
            return Visibility::Hidden(LowContrast);
        }
    }
    if after.iter().any(|area| area.opaque() && contains_rect(area.bbox, char.bbox)) {
        return Visibility::Hidden(Covered);
    }
    if background.is_none() {
        return Visibility::Suspect("unknown_background");
    }
    Visibility::Visible
}
