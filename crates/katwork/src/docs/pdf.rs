//! The PDF reader (`pdf-text` version 1): one lopdf parse behind `pdf_guard`, pdf-extract's
//! walker reporting every glyph and painted area, `pdf_visibility` judging each character, and
//! characters grouped into lines and blocks here.
//!
//! Fixed rules of version 1 (a change is a new version):
//! - Reading order is the content stream's painting order; nothing is sorted by position.
//! - A character starts a new line when its baseline moves by more than half the font size
//!   across the line's direction, moves back by more than half the font size along it, or turns;
//!   a character with no advance (a Thai mark) stays on its line within one font size. A gap
//!   wider than a quarter of the font size reads as one U+0020, never beside a combining mark.
//! - Lines join one block while the vertical gap between them is at most half the earlier
//!   line's height; a new page starts a block; lines are joined with U+000A.
//! - Hidden characters leave their line: each run of them (spaces between two hidden characters
//!   included) is cut out, the two sides are joined by exactly one U+0020 (none before a
//!   combining mark, which stays on its base), and the runs of a block form one hidden block
//!   right after it, its reason the runs' most common.
//! - A marked-content `/ActualText` replaces the text of the glyphs it encloses (Typst writes
//!   one around every Thai cluster its ToUnicode map cannot express); the span is hidden when
//!   any of its glyphs is, and suspect when any is suspect. A span whose text differs from its
//!   glyphs' own text after `normalize_th` is `Suspect("actual_text")`: it reads, but never at
//!   full quality, since only the file says the glyphs mean it.
//! - A glyph whose text repeats that of a glyph or span not hidden earlier on its line, with its
//!   box centre inside that one's box (within 5% of the font size), adds no text: it draws over
//!   a character already read (Typst draws a cluster's positioned mark again with the cluster's
//!   text; some producers overprint text to embolden it). The look-back covers the last 64
//!   glyphs of the same text on the line.
//! - A glyph whose advance is past three times its font size is `Suspect("glyph_width")`: its
//!   box, which the rules test, may stand far from its ink. It is never taken for a repeat.
//! - Ordinals count emitted blocks from 1 across the file, hidden blocks included.
use std::{
    cell::Cell,
    collections::{HashMap, VecDeque},
    rc::Rc,
};

use katwork_thai::normalize_th;
use lopdf::Document;
use pdf_extract::{
    Area, AreaKind as WalkedArea, Glyph, MediaBox, Optional, OutputDev, OutputError, Transform,
};

use super::{
    Block, DocError, DocLimits, ExtractWarning, Extracted, ExtractorId, HiddenWhy, LineBox,
    Locator, PageInfo, TextLayer, deadline,
    pdf_guard::{self, PrescanReport},
    pdf_layer::{self, PageText},
    pdf_visibility::{self, AreaKind, AreaPaint, CharPaint, GroupPaint, Paint, Visibility},
    replace_nul, split_pieces,
};

/// Characters one page may show (beyond the plan's limits table; `TooComplex("page_chars")`).
pub const MAX_PAGE_CHARS: usize = 200_000;
/// Areas one page may paint (`TooComplex("areas")`).
pub const MAX_PAGE_AREAS: usize = 100_000;
/// Distinct fonts one file may load (`TooComplex("fonts")`).
pub const MAX_FONTS: usize = 4_096;
/// A glyph advance past this many times its font size is no real glyph's: the box the rules
/// test would stand far from the ink, so the character is `Suspect("glyph_width")`.
pub const MAX_GLYPH_ADVANCE: f64 = 3.0;
/// Earlier glyphs of the same text on a line that an overprinted repeat is looked for in.
const REPEAT_WINDOW: usize = 64;
/// Walker work units (array elements, glyphs, KiB of Form content) between deadline checks.
const WORK_PER_CHECK: u64 = 1_024;
/// Glyphs grouped into lines between deadline checks: the heaviest step per glyph.
const GLYPHS_PER_CHECK: usize = 64;
/// Characters, line items and spans the other grouping steps handle between deadline checks.
const ITEMS_PER_CHECK: usize = 1_024;

thread_local! {
    static WALKS: Cell<u64> = const { Cell::new(0) };
}

/// How many times this thread handed a document to pdf-extract's walker, for tests.
#[doc(hidden)]
pub fn output_doc_calls() -> u64 {
    WALKS.get()
}

/// One glyph as the walker reported it.
struct Char {
    text: String,
    actual: Option<(u32, Rc<str>)>,
    paint: CharPaint,
    /// The box its text covers: its own, or for an `/ActualText` span's first glyph the
    /// union of the span's glyphs.
    text_box: [f32; 4],
    /// The baseline origin and its end after the advance, in user space.
    origin: (f64, f64),
    end: (f64, f64),
    /// The unit vector along the baseline.
    direction: (f64, f64),
    /// The effective height, for line breaking.
    size: f64,
    /// Shown from a font with no ToUnicode map in a way that loses text.
    unmapped_font: bool,
    /// Its advance is past `MAX_GLYPH_ADVANCE` times its font size.
    wide: bool,
}

#[derive(Default)]
struct Scene {
    chars: Vec<Char>,
    areas: Vec<AreaPaint>,
    images: usize,
    order: u32,
}

/// pdf-extract's output device: it collects each page's glyphs and areas, counts executed
/// operators against `max_content_ops` and checks the deadline, and stops the walk on a limit.
struct Collector<'l> {
    limits: &'l DocLimits,
    pages: Vec<Scene>,
    scene: Scene,
    operators: u64,
    /// Work units reported inside operators, and the count at which the deadline is next
    /// checked.
    work: u64,
    next_check: u64,
    stop: Option<DocError>,
    groups: (Vec<u32>, Rc<[GroupPaint]>),
}

impl Collector<'_> {
    fn fail(&mut self, error: DocError) -> OutputError {
        self.stop.get_or_insert(error);
        OutputError::Stopped
    }

    fn groups(&mut self, groups: &[pdf_extract::Group]) -> Rc<[GroupPaint]> {
        if self.groups.0.len() != groups.len()
            || self
                .groups
                .0
                .iter()
                .zip(groups)
                .any(|(id, group)| *id != group.id)
        {
            let chain: Vec<GroupPaint> = groups
                .iter()
                .map(|group| GroupPaint {
                    id: group.id,
                    alpha: group.alpha as f32,
                    soft_mask: group.soft_mask,
                    blend: group.blend,
                    knockout: group.knockout,
                })
                .collect();
            self.groups = (groups.iter().map(|group| group.id).collect(), chain.into());
        }
        self.groups.1.clone()
    }

    fn next_order(&mut self) -> u32 {
        self.scene.order = self.scene.order.saturating_add(1);
        self.scene.order
    }
}

fn paint(colour: &pdf_extract::Colour) -> Paint {
    Paint {
        space: colour.space.clone(),
        operands: colour
            .components
            .iter()
            .map(|value| *value as f32)
            .collect(),
    }
}

fn rect(rect: pdf_extract::Rect) -> [f32; 4] {
    [
        rect.x0 as f32,
        rect.y0 as f32,
        rect.x1 as f32,
        rect.y1 as f32,
    ]
}

fn apply(m: &Transform, x: f64, y: f64) -> (f64, f64) {
    (x * m.m11 + y * m.m21 + m.m31, x * m.m12 + y * m.m22 + m.m32)
}

impl OutputDev for Collector<'_> {
    fn begin_page(
        &mut self,
        _: u32,
        _: &MediaBox,
        _: Option<(f64, f64, f64, f64)>,
    ) -> Result<(), OutputError> {
        self.scene = Scene::default();
        Ok(())
    }

    fn end_page(&mut self) -> Result<(), OutputError> {
        self.pages.push(std::mem::take(&mut self.scene));
        Ok(())
    }

    fn output_character(
        &mut self,
        _: &Transform,
        _: f64,
        _: f64,
        _: f64,
        _: &str,
    ) -> Result<(), OutputError> {
        Ok(())
    }

    fn begin_word(&mut self) -> Result<(), OutputError> {
        Ok(())
    }

    fn end_word(&mut self) -> Result<(), OutputError> {
        Ok(())
    }

    fn end_line(&mut self) -> Result<(), OutputError> {
        Ok(())
    }

    fn operator(&mut self) -> Result<(), OutputError> {
        self.operators += 1;
        if self.operators > self.limits.max_content_ops {
            return Err(self.fail(DocError::TooComplex("content_ops")));
        }
        if self.operators.is_multiple_of(1024)
            && let Err(error) = deadline::check()
        {
            return Err(self.fail(error));
        }
        Ok(())
    }

    fn work(&mut self, units: u64) -> Result<(), OutputError> {
        self.work = self.work.saturating_add(units);
        if self.work >= self.next_check {
            self.next_check = self.work.saturating_add(WORK_PER_CHECK);
            if let Err(error) = deadline::check() {
                return Err(self.fail(error));
            }
        }
        Ok(())
    }

    fn glyph(&mut self, glyph: &Glyph) -> Result<(), OutputError> {
        if self.scene.chars.len() >= MAX_PAGE_CHARS {
            return Err(self.fail(DocError::TooComplex("page_chars")));
        }
        // A glyph with no finite position is nowhere on the page.
        let Some(bbox) = glyph.bbox() else {
            return Ok(());
        };
        let matrix = glyph.tm.post_transform(&glyph.ctm);
        let order = self.next_order();
        let groups = self.groups(glyph.groups);
        let origin = apply(&glyph.trm, 0.0, 0.0);
        let end = apply(
            &glyph.trm,
            glyph.width * glyph.font_size + glyph.spacing,
            0.0,
        );
        let along = (glyph.trm.m11.hypot(glyph.trm.m12)).max(f64::MIN_POSITIVE);
        let (a, b, c, d) = (matrix.m11, matrix.m12, matrix.m21, matrix.m22);
        let scale = a.hypot(b);
        let size = if scale > 0.0 {
            glyph.font_size.abs() * (a * d - b * c).abs() / scale
        } else {
            0.0
        };
        let font = glyph.font;
        self.scene.chars.push(Char {
            text: glyph.text.to_owned(),
            actual: glyph.actual_text.map(|(id, text)| (id, Rc::from(text))),
            paint: CharPaint {
                order,
                bbox: rect(bbox),
                render_mode: glyph.render_mode,
                fill: paint(glyph.fill),
                stroke: paint(glyph.stroke),
                fill_alpha: glyph.fill_alpha as f32,
                stroke_alpha: glyph.stroke_alpha as f32,
                soft_mask: glyph.soft_mask,
                blend: glyph.blend,
                clip: glyph.clip.bbox.map(rect),
                optional_hidden: glyph.optional != Optional::Visible,
                font_size: glyph.font_size as f32,
                horizontal_scaling: glyph.horizontal_scaling as f32,
                matrix: [a, b, c, d, matrix.m31, matrix.m32].map(|value| value as f32),
                groups,
            },
            text_box: rect(bbox),
            origin,
            end,
            direction: (glyph.trm.m11 / along, glyph.trm.m12 / along),
            size,
            unmapped_font: !font.has_to_unicode && (font.composite || !glyph.text.is_ascii()),
            wide: glyph.width.abs() > MAX_GLYPH_ADVANCE,
        });
        Ok(())
    }

    fn area(&mut self, area: &Area) -> Result<(), OutputError> {
        if self.scene.areas.len() >= MAX_PAGE_AREAS {
            return Err(self.fail(DocError::TooComplex("areas")));
        }
        let order = self.next_order();
        let groups = self.groups(area.groups);
        let (kind, masked) = match area.kind {
            WalkedArea::Fill { .. } => (AreaKind::Path, false),
            WalkedArea::Image { masked } => (AreaKind::Image, masked),
            WalkedArea::InlineImage { masked, .. } => (AreaKind::InlineImage, masked),
            WalkedArea::Shading => (AreaKind::Shading, false),
        };
        if matches!(kind, AreaKind::Image | AreaKind::InlineImage) {
            self.scene.images += 1;
        }
        self.scene.areas.push(AreaPaint {
            order,
            kind,
            bbox: rect(area.bbox),
            fill: area.fill.map(paint),
            alpha: area.alpha as f32,
            soft_mask: area.soft_mask,
            blend: area.blend,
            rect: area.rect,
            masked,
            groups,
        });
        Ok(())
    }
}

/// Runs pdf-extract's walker over the pre-scanned pages.
fn walk(
    doc: &Document,
    report: &PrescanReport,
    limits: &DocLimits,
) -> Result<Vec<Scene>, DocError> {
    let mut collector = Collector {
        limits,
        pages: Vec::new(),
        scene: Scene::default(),
        operators: 0,
        work: 0,
        next_check: WORK_PER_CHECK,
        stop: None,
        groups: (Vec::new(), Rc::from(Vec::new())),
    };
    let options = pdf_extract::Options {
        max_form_depth: limits.max_xobject_depth,
        max_fonts: MAX_FONTS,
        max_contents: pdf_guard::MAX_CONTENTS,
    };
    let pages: Vec<lopdf::ObjectId> = report.pages.iter().map(|page| page.id).collect();
    WALKS.set(WALKS.get() + 1);
    match pdf_extract::output_pages(doc, &pages, &options, &mut collector) {
        Ok(()) => Ok(collector.pages),
        Err(OutputError::Stopped) => Err(collector.stop.unwrap_or(DocError::Internal)),
        Err(OutputError::Limit(what)) => Err(DocError::TooComplex(what)),
        Err(OutputError::Malformed(what)) => Err(DocError::Malformed(what)),
        Err(_) => Err(DocError::Malformed("pdf")),
    }
}

/// One item of a line: a glyph's text, or a space read from a gap.
struct Item {
    text: String,
    hidden: Option<HiddenWhy>,
    suspect: Option<&'static str>,
    bbox: Option<[f32; 4]>,
}

impl Item {
    fn is_space(&self) -> bool {
        !self.text.is_empty() && self.text.chars().all(char::is_whitespace)
    }
}

struct Line {
    items: Vec<Item>,
    /// The union of its glyph boxes.
    rect: Option<[f32; 4]>,
}

fn union(a: Option<[f32; 4]>, b: [f32; 4]) -> [f32; 4] {
    match a {
        None => b,
        Some(a) => [
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        ],
    }
}

fn is_mark(c: char) -> bool {
    use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};
    matches!(
        c.general_category(),
        GeneralCategory::NonspacingMark | GeneralCategory::EnclosingMark
    )
}

/// Replaces each `/ActualText` span's glyph texts with its text, on its first glyph, and gives
/// every glyph of the span the span's verdict.
fn apply_actual_text(chars: &mut [Char], verdicts: &mut [Visibility]) -> Result<(), DocError> {
    let mut spans: HashMap<u32, Vec<usize>> = HashMap::new();
    for (at, char) in chars.iter().enumerate() {
        if at.is_multiple_of(ITEMS_PER_CHECK) {
            deadline::check()?;
        }
        if let Some((id, _)) = &char.actual {
            spans.entry(*id).or_default().push(at);
        }
    }
    for (index, members) in spans.values().enumerate() {
        if index.is_multiple_of(ITEMS_PER_CHECK) {
            deadline::check()?;
        }
        let hidden = members.iter().find_map(|at| match verdicts[*at] {
            Visibility::Hidden(why) => Some(Visibility::Hidden(why)),
            _ => None,
        });
        let suspect = members.iter().find_map(|at| match verdicts[*at] {
            Visibility::Suspect(why) => Some(Visibility::Suspect(why)),
            _ => None,
        });
        // A span whose text is not its glyphs' own (after normalization) is the file's claim
        // alone, so it never reads at full quality.
        let drawn: String = members.iter().map(|at| chars[*at].text.as_str()).collect();
        let matches = members.first().is_some_and(|first| {
            chars[*first]
                .actual
                .as_ref()
                .is_some_and(|(_, actual)| normalize_th(actual).text == normalize_th(&drawn).text)
        });
        let trusted = if matches {
            Visibility::Visible
        } else {
            Visibility::Suspect("actual_text")
        };
        let verdict = hidden.or(suspect).unwrap_or(trusted);
        let span_box = members
            .iter()
            .map(|at| chars[*at].paint.bbox)
            .reduce(|a, b| union(Some(a), b));
        if let (Some(first), Some(span_box)) = (members.first(), span_box) {
            chars[*first].text_box = span_box;
        }
        for (index, at) in members.iter().enumerate() {
            let text = match (&chars[*at].actual, index) {
                (Some((_, text)), 0) => text.to_string(),
                _ => String::new(),
            };
            chars[*at].text = text;
            verdicts[*at] = verdict;
        }
    }
    Ok(())
}

/// Whether `inner`'s centre lies in `outer`, within 5% of `outer`'s height (its font size): a
/// mark drawn again at the end of its cluster sits on the cluster's edge, while the next
/// character's centre is half a character beyond it.
fn holds(outer: [f32; 4], inner: [f32; 4]) -> bool {
    let (x, y) = ((inner[0] + inner[2]) / 2.0, (inner[1] + inner[3]) / 2.0);
    let slack = (outer[3] - outer[1]).abs() * 0.05;
    outer[0] - slack <= x && x <= outer[2] + slack && outer[1] - slack <= y && y <= outer[3] + slack
}

/// The page's glyphs as lines, in painting order.
fn lines(chars: &[Char], verdicts: &[Visibility]) -> Result<Vec<Line>, DocError> {
    let mut lines: Vec<Line> = Vec::new();
    let mut previous: Option<&Char> = None;
    // The boxes of the last `REPEAT_WINDOW` glyphs read on the current line, by text, for
    // overprinted repeats: a bounded look-back, so a line of any length costs linear time.
    let mut read: HashMap<&str, VecDeque<[f32; 4]>> = HashMap::new();
    for (at, (char, verdict)) in chars.iter().zip(verdicts).enumerate() {
        if at.is_multiple_of(GLYPHS_PER_CHECK) {
            deadline::check()?;
        }
        let (hidden, suspect) = match verdict {
            Visibility::Visible => (None, None),
            Visibility::Suspect(why) => (None, Some(*why)),
            Visibility::Hidden(why) => (Some(*why), None),
        };
        let mut space = false;
        let new_line = match previous {
            None => true,
            Some(before) => {
                let size = before.size.max(char.size).max(f64::MIN_POSITIVE);
                let delta = (char.origin.0 - before.end.0, char.origin.1 - before.end.1);
                let along = delta.0 * before.direction.0 + delta.1 * before.direction.1;
                let across = -delta.0 * before.direction.1 + delta.1 * before.direction.0;
                let turned = before.direction.0 * char.direction.0
                    + before.direction.1 * char.direction.1
                    < 0.95;
                let zero_advance =
                    (char.end.0 - char.origin.0).hypot(char.end.1 - char.origin.1) < 1e-6;
                let mark = char.text.chars().next().is_some_and(is_mark);
                let off_line = if zero_advance || mark {
                    across.abs() > size
                } else {
                    across.abs() > size / 2.0
                };
                let broke = turned || off_line || along < -size / 2.0;
                space = !broke
                    && along > size / 4.0
                    && !mark
                    && !char.text.starts_with(char::is_whitespace)
                    && !before.text.ends_with(char::is_whitespace);
                broke
            }
        };
        if new_line || lines.is_empty() {
            lines.push(Line {
                items: Vec::new(),
                rect: None,
            });
            read.clear();
        }
        // A wide glyph's box says nothing of where its ink is: it is never a repeat, and no
        // later glyph is one of it.
        let repeat = !char.text.is_empty()
            && !char.wide
            && read
                .get(char.text.as_str())
                .is_some_and(|boxes| boxes.iter().any(|text_box| holds(*text_box, char.text_box)));
        if !repeat && hidden.is_none() && !char.text.is_empty() && !char.wide {
            let boxes = read.entry(char.text.as_str()).or_default();
            if boxes.len() == REPEAT_WINDOW {
                boxes.pop_front();
            }
            boxes.push_back(char.text_box);
        }
        let Some(line) = lines.last_mut() else {
            continue;
        };
        if space {
            line.items.push(Item {
                text: " ".to_owned(),
                hidden: None,
                suspect: None,
                bbox: None,
            });
        }
        line.rect = Some(union(line.rect, char.paint.bbox));
        line.items.push(Item {
            text: if repeat {
                String::new()
            } else {
                char.text.clone()
            },
            hidden,
            suspect,
            bbox: Some(char.paint.bbox),
        });
        previous = Some(char);
    }
    Ok(lines)
}

/// A space is hidden only between two hidden characters of its line. Linear: the nearest
/// glyph on each side of every space is found in one pass each way.
fn settle_spaces(items: &mut [Item]) {
    // Each item's own state: hidden or not for a glyph with text, none for a space or an
    // empty glyph.
    let states: Vec<Option<bool>> = items
        .iter()
        .map(|item| {
            if item.is_space() || item.text.is_empty() {
                None
            } else {
                Some(item.hidden.is_some())
            }
        })
        .collect();
    let mut before = vec![None; items.len()];
    let mut last = None;
    for (at, state) in states.iter().enumerate() {
        before[at] = last;
        if state.is_some() {
            last = *state;
        }
    }
    let mut after = vec![None; items.len()];
    let mut next = None;
    for (at, state) in states.iter().enumerate().rev() {
        after[at] = next;
        if state.is_some() {
            next = *state;
        }
    }
    // A hidden space takes the reason of the nearest hidden item before it.
    let mut reason = None;
    for at in 0..items.len() {
        if states[at].is_none() && items[at].is_space() {
            items[at].hidden = match (before[at], after[at]) {
                (Some(true), Some(true)) => reason,
                _ => None,
            };
        }
        if items[at].hidden.is_some() {
            reason = items[at].hidden;
        }
    }
}

/// Text with a box and a line for each code point, for pieces and their line boxes.
#[derive(Default)]
struct Text {
    text: String,
    boxes: Vec<Option<[f32; 4]>>,
    lines: Vec<Option<usize>>,
}

impl Text {
    fn push(&mut self, text: &str, bbox: Option<[f32; 4]>, line: Option<usize>) {
        for c in text.chars() {
            self.text.push(c);
            self.boxes.push(bbox);
            self.lines.push(line);
        }
    }

    fn ends_with_space(&self) -> bool {
        self.text.ends_with(char::is_whitespace)
    }

    fn trim_end(&mut self, from: usize) {
        while self.text.len() > from && self.ends_with_space() {
            self.text.pop();
            self.boxes.pop();
            self.lines.pop();
        }
    }
}

/// One block's text and its hidden runs' text.
struct Assembled {
    visible: Text,
    hidden: Text,
    reasons: Vec<HiddenWhy>,
    suspect: Option<&'static str>,
    visible_lines: Vec<String>,
}

fn assemble_block(block: &mut [Line]) -> Result<Assembled, DocError> {
    let mut out = Assembled {
        visible: Text::default(),
        hidden: Text::default(),
        reasons: Vec::new(),
        suspect: None,
        visible_lines: Vec::new(),
    };
    let mut hidden_line = 0usize;
    let mut handled = 0usize;
    for (line_at, line) in block.iter_mut().enumerate() {
        deadline::check()?;
        settle_spaces(&mut line.items);
        // Byte and code point offsets where the line starts and where its content starts (a
        // Text holds one box per code point, so its box count is its length in code points).
        let line_start = out.visible.text.len();
        let line_start_chars = out.visible.boxes.len();
        if line_start > 0 {
            out.visible.push("\n", None, None);
        }
        let content_start = out.visible.text.len();
        let content_start_chars = out.visible.boxes.len();
        let mut cut = false;
        let mut run_open = false;
        for item in &line.items {
            handled += 1;
            if handled.is_multiple_of(ITEMS_PER_CHECK) {
                deadline::check()?;
            }
            match item.hidden {
                Some(why) => {
                    if !item.is_space() {
                        out.reasons.push(why);
                    }
                    if !run_open {
                        // A new run: after a run on another line, a line break; on this line, a space.
                        if !out.hidden.text.is_empty() {
                            let separator = if hidden_line == line_at { " " } else { "\n" };
                            out.hidden.push(separator, None, None);
                        }
                        run_open = true;
                        hidden_line = line_at;
                    }
                    out.hidden.push(&item.text, item.bbox, Some(line_at));
                    if out.visible.text.len() > content_start {
                        cut = true;
                    }
                }
                None => {
                    if run_open {
                        out.hidden.trim_end(0);
                        run_open = false;
                    }
                    if cut {
                        cut = false;
                        if item.is_space() {
                            if out.visible.ends_with_space() {
                                continue;
                            }
                        } else if !out.visible.ends_with_space() && !item.text.starts_with(is_mark)
                        {
                            // A combining mark stays on its base: a space would split its cluster.
                            out.visible.push(" ", None, Some(line_at));
                        }
                    }
                    if item.suspect.is_some() && !item.text.trim().is_empty() {
                        out.suspect = out.suspect.or(item.suspect);
                    }
                    out.visible.push(&item.text, item.bbox, Some(line_at));
                }
            }
        }
        if run_open {
            out.hidden.trim_end(0);
        }
        out.visible.trim_end(content_start);
        // Leading spaces of the line carry nothing either.
        let line_text: String = out.visible.text[content_start..].to_owned();
        let trimmed = line_text.trim_start();
        let skip = line_text.chars().count() - trimmed.chars().count();
        if skip > 0 {
            out.visible.text.replace_range(
                content_start..content_start + (line_text.len() - trimmed.len()),
                "",
            );
            out.visible
                .boxes
                .drain(content_start_chars..content_start_chars + skip);
            out.visible
                .lines
                .drain(content_start_chars..content_start_chars + skip);
        }
        if out.visible.text.len() == content_start {
            // Nothing visible on this line: drop its line break too.
            out.visible.text.truncate(line_start);
            out.visible.boxes.truncate(line_start_chars);
            out.visible.lines.truncate(line_start_chars);
        } else {
            out.visible_lines
                .push(out.visible.text[content_start..].to_owned());
        }
    }
    Ok(out)
}

/// The most common reason, the earliest on a tie.
fn most_common(reasons: &[HiddenWhy]) -> HiddenWhy {
    let mut counts: Vec<(HiddenWhy, usize)> = Vec::new();
    for reason in reasons {
        match counts.iter_mut().find(|(seen, _)| seen == reason) {
            Some((_, count)) => *count += 1,
            None => counts.push((*reason, 1)),
        }
    }
    let best = counts.iter().map(|(_, count)| *count).max().unwrap_or(0);
    counts
        .into_iter()
        .find(|(_, count)| *count == best)
        .map_or(HiddenWhy::UnknownPaint, |(reason, _)| reason)
}

/// Blocks, their ordinals and pieces, and their warnings.
struct Output<'l> {
    limits: &'l DocLimits,
    blocks: Vec<Block>,
    warnings: Vec<ExtractWarning>,
    ordinal: u32,
}

impl Output<'_> {
    fn emit(
        &mut self,
        page: u32,
        text: &Text,
        hidden: Option<HiddenWhy>,
        suspect: Option<&'static str>,
    ) -> Result<(), DocError> {
        self.ordinal += 1;
        let mut chars_before = 0usize;
        for (piece, range) in split_pieces(&text.text)?.into_iter().enumerate() {
            if self.blocks.len() as u64 >= u64::from(self.limits.max_blocks) {
                return Err(DocError::TooComplex("blocks"));
            }
            let piece_text = &text.text[range];
            let len = piece_text.chars().count();
            let mut lines: Vec<LineBox> = Vec::new();
            let mut current: Option<(usize, u32, u32, Option<[f32; 4]>)> = None;
            for offset in 0..len {
                let at = chars_before + offset;
                let line = text.lines[at];
                let bbox = text.boxes[at];
                match (&mut current, line) {
                    (Some((open, _, end, rect)), Some(line)) if *open == line => {
                        *end = offset as u32 + 1;
                        if let Some(bbox) = bbox {
                            *rect = Some(union(*rect, bbox));
                        }
                    }
                    (_, Some(line)) => {
                        if let Some((_, start, end, Some(rect))) = current.take() {
                            lines.push(LineBox {
                                start,
                                end,
                                page,
                                rect,
                            });
                        }
                        current = Some((line, offset as u32, offset as u32 + 1, bbox));
                    }
                    (_, None) => {
                        if let Some((_, start, end, Some(rect))) = current.take() {
                            lines.push(LineBox {
                                start,
                                end,
                                page,
                                rect,
                            });
                        }
                    }
                }
            }
            if let Some((_, start, end, Some(rect))) = current.take() {
                lines.push(LineBox {
                    start,
                    end,
                    page,
                    rect,
                });
            }
            self.blocks.push(Block {
                ordinal: self.ordinal,
                piece: piece as u32,
                text: replace_nul(piece_text),
                locator: Locator::PdfPage { page },
                lines,
                cell: None,
                unit_context: None,
                hidden,
                suspect,
            });
            chars_before += len;
        }
        if let Some(why) = hidden {
            self.warnings.push(ExtractWarning::HiddenContent {
                at: Locator::PdfPage { page },
                why: why.as_str(),
            });
        }
        Ok(())
    }
}

fn block_starts(lines: &[Line]) -> Vec<usize> {
    let mut starts = vec![0];
    for at in 1..lines.len() {
        let (Some(a), Some(b)) = (lines[at - 1].rect, lines[at].rect) else {
            starts.push(at);
            continue;
        };
        let height = (a[3] - a[1]).max(0.0);
        let gap = (a[1].max(b[1]) - a[3].min(b[3])).max(0.0);
        if gap > height / 2.0 {
            starts.push(at);
        }
    }
    starts
}

/// Reads `bytes` as a PDF.
pub(crate) fn read(bytes: &[u8], limits: &DocLimits) -> Result<Extracted, DocError> {
    if bytes.len() as u64 > limits.max_input_bytes {
        return Err(DocError::TooLarge("input"));
    }
    let _budget = pdf_guard::BudgetScope::open(limits);
    let mut doc = pdf_guard::load(bytes, limits)?;
    let report = pdf_guard::prescan(&mut doc, limits)?;
    let scenes = walk(&doc, &report, limits)?;
    drop(doc);
    let mut output = Output {
        limits,
        blocks: Vec::new(),
        warnings: Vec::new(),
        ordinal: 0,
    };
    let mut pages = Vec::with_capacity(report.pages.len());
    for (info, mut scene) in report.pages.iter().zip(scenes) {
        deadline::check()?;
        let mut page = PageInfo {
            page: info.number,
            media_box: info.media_box,
            crop_box: info.crop_box,
            text_layer: TextLayer::Ok,
        };
        let paints: Vec<CharPaint> = scene.chars.iter().map(|char| char.paint.clone()).collect();
        let mut verdicts = pdf_visibility::try_classify(&paints, &scene.areas, &page, limits)?;
        drop(paints);
        // A glyph whose box stands far from its ink cannot be judged by its box.
        for (char, verdict) in scene.chars.iter().zip(verdicts.iter_mut()) {
            if char.wide && *verdict == Visibility::Visible {
                *verdict = Visibility::Suspect("glyph_width");
            }
        }
        apply_actual_text(&mut scene.chars, &mut verdicts)?;
        let shown = |verdict: &Visibility| !matches!(verdict, Visibility::Hidden(_));
        let unmapped_font = scene
            .chars
            .iter()
            .zip(&verdicts)
            .any(|(char, verdict)| char.unmapped_font && shown(verdict));
        let visible_glyphs = scene
            .chars
            .iter()
            .zip(&verdicts)
            .filter(|(char, verdict)| {
                shown(verdict)
                    && (char.text.is_empty() || !char.text.chars().all(char::is_whitespace))
            })
            .count();
        let mut lines = lines(&scene.chars, &verdicts)?;
        let starts = block_starts(&lines);
        let mut visible_lines = Vec::new();
        for (index, start) in starts.iter().enumerate() {
            let end = starts.get(index + 1).copied().unwrap_or(lines.len());
            let block = assemble_block(&mut lines[*start..end])?;
            if !block.visible.text.is_empty() {
                output.emit(page.page, &block.visible, None, block.suspect)?;
            }
            if !block.hidden.text.is_empty() {
                output.emit(
                    page.page,
                    &block.hidden,
                    Some(most_common(&block.reasons)),
                    None,
                )?;
            }
            visible_lines.extend(block.visible_lines);
        }
        page.text_layer = pdf_layer::check_page(&PageText {
            lines: visible_lines,
            visible_glyphs,
            unmapped_font,
            images: scene.images,
        });
        if report.xref_rebuilt && page.text_layer == TextLayer::Ok {
            page.text_layer = TextLayer::Suspect("xref_recovered");
        }
        match page.text_layer {
            TextLayer::Ok => {}
            TextLayer::Suspect(why) => output.warnings.push(ExtractWarning::SuspectTextLayer {
                page: page.page,
                why,
            }),
            TextLayer::Absent => output
                .warnings
                .push(ExtractWarning::NoTextLayer { page: page.page }),
        }
        pages.push(page);
    }
    let mut warnings = report.warnings;
    warnings.extend(output.warnings);
    Ok(Extracted {
        extractor: ExtractorId::PDF,
        blocks: output.blocks,
        pages,
        warnings,
        workbook: None,
    })
}
