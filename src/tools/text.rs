use anyhow::Result;
use femtovg::{Color, FontId, Paint, Path};
use relm4::gtk::prelude::IMContextExt;
use relm4::gtk::{
    self, TextBuffer,
    gdk::{Key, ModifierType, Rectangle},
};
use std::{borrow::Cow, ops::Range};

use relm4::gtk::prelude::*;

use crate::{
    configuration::APP_CONFIG,
    femtovg_area,
    ime::preedit::{Preedit, UnderlineKind},
    math::{Rect, Vec2D},
    sketch_board::{KeyEventMsg, MouseButton, MouseEventMsg, MouseEventType, TextEventMsg},
    style::Style,
};

use super::{
    Drawable, DrawableClone, DrawableId, GLOW_COLOR, Handle, HandleId, HandleKind, InputContext,
    SELECTION_BLUE, Tool, ToolUpdateResult, Tools,
};
use crate::sketch_board::SketchBoardInput;
use relm4::Sender;
use relm4::gtk::gdk::DisplayManager;
use std::cell::RefCell;
use std::rc::Rc;

/// Visual padding (CSS px) between the cream text-pill and the blue
/// selection/edit outline. 10 px on every side so the outline sits
/// clearly outside the pill and the spacing reads as deliberate.
const OUTLINE_PADDING_CSS: f32 = 10.0;
/// CSS-pixel padding inside each cream pill on the X axis. Equal on
/// left + right so glyphs aren't flush against the rounded edge.
const PILL_PAD_X_CSS: f32 = 6.0;
/// Vertical pill padding (CSS px). Symmetric 6 px above cap-top and
/// 6 px below baseline so the text body floats inside the pill with
/// equal breathing room top and bottom. The bottom pad is further
/// expanded at draw time when the font's descender exceeds 6 CSS
/// px (large annotation sizes / multipliers) so commas, apostrophes,
/// and lowercase "g"/"p"/"y" stay contained inside the pill rather
/// than poking below it.
const PILL_PAD_Y_TOP_CSS: f32 = 6.0;
const PILL_PAD_Y_BOTTOM_CSS: f32 = 6.0;
/// Estimated descender depth as a fraction of `line_height` (the
/// image-space "|"-measured glyph extent). Typical Latin fonts put
/// descenders at ~0.2 em and the "|" measurement comes out around
/// the em-box, so 0.22 leaves a small safety cushion across the
/// font families we ship + the system fallbacks. Used only to size
/// the pill bottom pad when the font is large enough that the
/// descender would otherwise exceed the 8 CSS-px floor.
const DESCENDER_RATIO_OF_LINE_HEIGHT: f32 = 0.22;
/// Extra CSS-px breathing room added below the deepest descender
/// when we extend the bottom pad to contain them at large sizes.
const DESCENDER_SAFETY_CSS: f32 = 2.0;
/// Combined CSS-pixel pad from text glyph to the visible outline —
/// used by bounds() and move_handle to convert between drag positions
/// and wrap-width.
const SELECTION_PAD_X_CSS: f32 = PILL_PAD_X_CSS + OUTLINE_PADDING_CSS;
const SELECTION_PAD_Y_TOP_CSS: f32 = PILL_PAD_Y_TOP_CSS + OUTLINE_PADDING_CSS;
const SELECTION_PAD_Y_BOTTOM_CSS: f32 = PILL_PAD_Y_BOTTOM_CSS + OUTLINE_PADDING_CSS;
/// Default wrap-area width (image-space px) used as a floor when text
/// is empty (no measurable content to auto-fit against). Once the user
/// has typed anything, the auto-fit code derives wrap from glyph
/// metrics and ignores this constant.
const DEFAULT_INITIAL_BOX_WIDTH: f32 = 80.0;
/// Lower bound for `text_box_width`. Below this, dragging a handle
/// inward stops shrinking the box.
const MIN_TEXT_BOX_WIDTH: f32 = 60.0;
/// Image-space hit radius for the editing-mode handle hit test inside
/// `TextTool`. Bigger than the visual handle so users don't have to
/// pixel-precise hit a ~12 px disc to grab it.
const HANDLE_HIT_RADIUS: f32 = 20.0;
/// Blue outline thickness for the editing-mode wrap-area outline, in
/// CSS pixels. Scaled to image units at draw time. 2 px reads as a
/// deliberate frame at any zoom; semi-transparency comes from
/// `TEXT_OUTLINE_ALPHA` so the outline stays unobtrusive.
const EDITING_OUTLINE_WIDTH: f32 = 2.0;
/// Alpha applied to `SELECTION_BLUE` for the text outline (both
/// editing and selected states). Semi-transparent so the outline
/// reads as UI rather than competing with the rendered text.
const TEXT_OUTLINE_ALPHA: f32 = 0.6;
/// Corner radius (CSS px) for the editing/selection outline.
const OUTLINE_CORNER_RADIUS_CSS: f32 = 8.0;
/// Dash and gap lengths (CSS px) for the post-creation selection
/// outline. A solid outline reads as "I'm being edited right now",
/// the dashed variant reads as "I'm selected but not active", which
/// keeps the editing-mode and selection-mode states visually
/// distinct without needing a second color.
const SELECTION_DASH_LEN_CSS: f32 = 8.0;
const SELECTION_GAP_LEN_CSS: f32 = 4.0;
/// Pill corner radius — half the outline's. The pill reads as a
/// snug label backdrop with subtle softening; the outline keeps the
/// stronger 8 px rounding so the concentric shapes are visually
/// distinct.
const PILL_CORNER_RADIUS_CSS: f32 = OUTLINE_CORNER_RADIUS_CSS / 2.0;

/// Build a path of evenly-spaced dashes that trace the outline of a
/// rounded rectangle. Each dash is emitted as its own sub-path so a
/// single `stroke_path` call paints the whole pattern at once. The
/// total dash/gap segment is rebalanced to divide the perimeter
/// cleanly — without that, the last dash near the start point gets
/// awkwardly clipped or overlapped.
fn dashed_rounded_rect_path(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    dash_len: f32,
    gap_len: f32,
) -> Path {
    use std::f32::consts::PI;
    let r = r.min(w * 0.5).min(h * 0.5).max(0.0);
    let edge_w = (w - 2.0 * r).max(0.0);
    let edge_h = (h - 2.0 * r).max(0.0);
    let arc_len = 0.5 * PI * r;
    // Cumulative perimeter offsets at the boundary of each segment
    // (top edge, top-right arc, right edge, br arc, bottom edge,
    // bl arc, left edge, tl arc).
    let s1 = edge_w;
    let s2 = s1 + arc_len;
    let s3 = s2 + edge_h;
    let s4 = s3 + arc_len;
    let s5 = s4 + edge_w;
    let s6 = s5 + arc_len;
    let s7 = s6 + edge_h;
    let perimeter = s7 + arc_len;

    // Map a perimeter parameter `s` ∈ [0, perimeter) to its (x, y)
    // position on the outline. Convention: s=0 is the top edge just
    // after the top-left rounded corner, going clockwise.
    let position_at = |mut s: f32| -> (f32, f32) {
        s = s.rem_euclid(perimeter);
        if s <= s1 {
            (x + r + s, y)
        } else if s <= s2 {
            let t = (s - s1) / arc_len;
            let a = 1.5 * PI + 0.5 * PI * t;
            (x + w - r + r * a.cos(), y + r + r * a.sin())
        } else if s <= s3 {
            (x + w, y + r + (s - s2))
        } else if s <= s4 {
            let t = (s - s3) / arc_len;
            let a = 0.5 * PI * t;
            (x + w - r + r * a.cos(), y + h - r + r * a.sin())
        } else if s <= s5 {
            (x + w - r - (s - s4), y + h)
        } else if s <= s6 {
            let t = (s - s5) / arc_len;
            let a = 0.5 * PI + 0.5 * PI * t;
            (x + r + r * a.cos(), y + h - r + r * a.sin())
        } else if s <= s7 {
            (x, y + h - r - (s - s6))
        } else {
            let t = (s - s7) / arc_len;
            let a = PI + 0.5 * PI * t;
            (x + r + r * a.cos(), y + r + r * a.sin())
        }
    };

    let mut path = Path::new();
    if perimeter <= 0.0 {
        return path;
    }
    let raw_segment = (dash_len + gap_len).max(0.1);
    let n_cycles = (perimeter / raw_segment).round().max(1.0) as usize;
    let actual_segment = perimeter / n_cycles as f32;
    let dash_ratio = dash_len / (dash_len + gap_len);
    let actual_dash = actual_segment * dash_ratio;
    // Quality knob for the arc portions: a quarter arc is broken
    // into this many line segments so stroked dashes that span a
    // corner stay visibly curved at reasonable zooms.
    let steps_per_dash = 8;
    for cycle in 0..n_cycles {
        let s_start = cycle as f32 * actual_segment;
        let (sx, sy) = position_at(s_start);
        path.move_to(sx, sy);
        for k in 1..=steps_per_dash {
            let t = k as f32 / steps_per_dash as f32;
            let (px, py) = position_at(s_start + actual_dash * t);
            path.line_to(px, py);
        }
    }
    path
}

/// Background style behind the rendered text — picked by the user via
/// the StyleToolbar dropdown (visible only when the Text tool is
/// active). `Plain` renders the text glyphs directly on the canvas;
/// `Rounded` adds a cream-colored rounded pill behind each line of
/// text (the pill snugly fits each line's glyph metrics).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TextBackground {
    Plain,
    #[default]
    Rounded,
}

impl TextBackground {
    /// Human label for the cycle toast.
    pub fn display_name(self) -> &'static str {
        match self {
            TextBackground::Plain => "Plain",
            TextBackground::Rounded => "Rounded",
        }
    }
}

#[derive(Clone, Debug)]
/// Per-frame layout state captured during `Text::draw` and consulted by
/// `bounds`, `hit_test`, `move_handle`, etc. Grouped behind a single
/// `RefCell<LayoutCache>` on `Text` so a draw pass takes one mut borrow
/// instead of eight, and so callers that need several fields together
/// (line metrics + wrap state) read them with one shared borrow.
struct LayoutCache {
    /// Glyph bounding rect in GTK coordinates. Drives `glyph_rect()` /
    /// committed-selection bounds.
    rect: Rectangle,
    /// Wrap-area rect (top-left + size, image coords) covering the
    /// full text-box including unfilled wrap width and empty-line
    /// space. Used to render the blue editing outline and to hit-test
    /// editing handles.
    editing_rect: Rect,
    /// Line height (image-space px) from the most recent draw.
    /// `bounds()` / handle hit-tests use this to reason about the
    /// box height without re-measuring the font.
    line_height: f32,
    /// CSS-px → image-space conversion factor (= 1 /
    /// canvas.transform.average_scale × DPR). `bounds` / `move_handle`
    /// use it to translate the CSS-pixel PILL_PAD / OUTLINE_PADDING
    /// constants into image units and to convert a dragged handle
    /// position back into the underlying `text_box_width`.
    css_to_image: f32,
    /// Natural single-line width of the full text (image-space px)
    /// from the last `measure_text`. `bounds` during a handle drag
    /// uses `natural_width / wrap_width` to estimate the new line
    /// count so the outline reflects wrapping in real time.
    natural_text_width: f32,
    /// Per-line glyph rects. Outer Vec is lines, inner is glyph
    /// rects within each line.
    glyphs: Vec<Vec<Rectangle>>,
    /// Byte ranges per wrapped line, populated from
    /// `canvas.break_text_vec` at the top of each draw.
    line_ranges: Vec<Range<usize>>,
}

impl LayoutCache {
    fn new() -> Self {
        Self {
            rect: Rectangle::new(0, 0, 0, 0),
            editing_rect: Rect::default(),
            line_height: 0.0,
            // 1.0 (not 0.0) so an early `bounds()` call before the
            // first draw doesn't collapse into degenerate math.
            css_to_image: 1.0,
            natural_text_width: 0.0,
            glyphs: Vec::new(),
            line_ranges: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Text {
    pos: Vec2D,
    editing: bool,
    text_buffer: TextBuffer,
    style: Style,
    /// Captured at creation/edit time. Each line's pill is rendered
    /// per-line so wrapped text shows as a vertical stack of pills
    /// rather than one giant rect spanning the wrap area.
    background: TextBackground,
    preedit: Option<Preedit>,
    im_context: Option<InputContext>,
    /// Per-draw layout cache (see `LayoutCache`). One RefCell so a
    /// draw pass takes a single mut borrow rather than threading
    /// eight separate ones.
    layout: RefCell<LayoutCache>,
    /// Caret blink phase — independent of layout state, so it gets
    /// its own cell that the blink timer can flip without dirtying
    /// any draw-derived data.
    cursor_visible: RefCell<bool>,
    font_ids: Vec<FontId>,
    /// Explicit wrap width set on creation and by side-handle drags.
    /// When `None`, layout falls back to "use available image width from
    /// `pos.x` to the right edge" — only relevant for legacy/edge-case
    /// Text instances; new Texts always carry an explicit width.
    text_box_width: Option<f32>,
}

struct DisplayContent<'a> {
    text: Cow<'a, str>,
    cursor_byte_pos: usize,
    preedit_range: Option<Range<usize>>,
}

struct LineLayout {
    range: Range<usize>,
    baseline: f32,
    /// Image-space x where this line's glyphs start. Equals
    /// `self.pos.x + center_off` where `center_off` horizontally
    /// centers the line within the wrap area. The caret, selection
    /// rects, and click hit-test all use this so they track the
    /// rendered glyphs instead of the wrap-area left edge.
    start_x: f32,
}

struct TextDrawingContext<'a> {
    paint: &'a Paint,
    text: &'a str,
    lines: &'a [LineLayout],
    /// Wrap-area width and origin x; used for caret positioning on
    /// empty lines / between-line states where no glyph layout exists.
    wrap_width: f32,
    base_x: f32,
}

#[derive(Clone, Copy)]
struct CursorMetrics {
    /// Glyph-extent top offset from baseline (negative). Used for
    /// selection rectangles, preedit highlights, and glyph hit-test
    /// rects — anything that should hug the actual letterforms.
    top_offset: f32,
    /// Glyph-extent height. Same audience as `top_offset`.
    height: f32,
    line_height: f32,
    /// Caret-only top offset from baseline (negative). Equals
    /// `top_offset - pill_pad_y_top` so the blinking caret reaches
    /// all the way up to the top of the cream pill background
    /// instead of stopping at cap-height.
    caret_top_offset: f32,
    /// Caret-only height — spans the full pill (cap-pad above plus
    /// glyph body plus baseline-pad below, including the descender
    /// extension at large font sizes). Lets the cursor read as a
    /// full-height insertion mark, matching the standard pattern.
    caret_height: f32,
}

impl Text {
    fn new(
        pos: Vec2D,
        style: Style,
        background: TextBackground,
        im_context: Option<InputContext>,
    ) -> Self {
        let text_buffer = TextBuffer::new(None);
        text_buffer.set_enable_undo(true);

        Self {
            pos,
            text_buffer,
            editing: true,
            style,
            background,
            preedit: None,
            im_context,
            layout: RefCell::new(LayoutCache::new()),
            cursor_visible: RefCell::new(true),
            font_ids: femtovg_area::font_stack().to_vec(),
            // Start with auto-fit (None) so the wrap area hugs the
            // text from the very first keystroke instead of extending
            // out to a fixed default. Dragging the right handle sets
            // an explicit width via `move_handle`, which switches the
            // text into "user-set wrap width" mode.
            text_box_width: None,
        }
    }

    fn byte_index_from_char_index(text: &str, char_index: usize) -> usize {
        text.char_indices()
            .nth(char_index)
            .map(|(idx, _)| idx)
            .unwrap_or_else(|| text.len())
    }

    fn display_text<'a>(&self, base_text: &'a str) -> DisplayContent<'a> {
        let cursor_char_index = self.text_buffer.cursor_position() as usize;
        let base_cursor_byte = Self::byte_index_from_char_index(base_text, cursor_char_index);

        if self.editing {
            if let Some(preedit) = &self.preedit {
                if preedit.text.is_empty() {
                    return DisplayContent {
                        text: Cow::Borrowed(base_text),
                        cursor_byte_pos: base_cursor_byte,
                        preedit_range: None,
                    };
                }

                let mut composed = String::with_capacity(base_text.len() + preedit.text.len());
                composed.push_str(&base_text[..base_cursor_byte]);
                composed.push_str(&preedit.text);
                composed.push_str(&base_text[base_cursor_byte..]);

                let preedit_char_len = preedit.text.chars().count();
                let cursor_chars = preedit
                    .cursor_chars
                    .map(|value| value.min(preedit_char_len))
                    .unwrap_or(preedit_char_len);
                let preedit_cursor_byte =
                    Self::byte_index_from_char_index(&preedit.text, cursor_chars);
                let composed_cursor_byte = base_cursor_byte + preedit_cursor_byte;

                DisplayContent {
                    text: Cow::Owned(composed),
                    cursor_byte_pos: composed_cursor_byte,
                    preedit_range: Some(base_cursor_byte..base_cursor_byte + preedit.text.len()),
                }
            } else {
                DisplayContent {
                    text: Cow::Borrowed(base_text),
                    cursor_byte_pos: base_cursor_byte,
                    preedit_range: None,
                }
            }
        } else {
            DisplayContent {
                text: Cow::Borrowed(base_text),
                cursor_byte_pos: base_cursor_byte,
                preedit_range: None,
            }
        }
    }
}

impl Drawable for Text {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn kind_label(&self) -> &'static str {
        "Text"
    }
    fn icon_name(&self) -> &'static str {
        "text-case-title-regular"
    }

    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        font: FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        // (Cream pill background drawing moved below — it's now drawn
        // per-line after layout, so the pill snugly fits each
        // wrapped line of glyphs instead of the full multi-line bbox.)

        let gtext = self.text_buffer.text(
            &self.text_buffer.start_iter(),
            &self.text_buffer.end_iter(),
            false,
        );
        let base_text = gtext.as_str();
        let display = self.display_text(base_text);
        let text = display.text.as_ref();

        let mut base_paint: Paint = self.style.into();
        base_paint.set_font(&[font]);

        if self.font_ids.is_empty() {
            base_paint.set_font(&[font]);
        } else {
            base_paint.set_font(&self.font_ids);
        }

        let transform = canvas.transform();
        let canva_scale = transform.average_scale();

        // Auto-fit wrap width when the user hasn't manually resized:
        // measure the text's natural single-line width and use that
        // (clamped to the empty-text floor). When `text_box_width` is
        // Some(_) the user has set an explicit wrap via the right
        // handle drag, which we honor — including when narrower than
        // the text, in which case `break_text_vec` wraps it onto
        // multiple lines.
        //
        // `measure_text` and `break_text_vec` round sub-pixel
        // differences in opposite directions: a measured width passed
        // straight back as a wrap width will sometimes cause
        // `break_text_vec` to wrap the LAST glyph onto a second line
        // (visible to the user as a character "bumping down" mid-type
        // and snapping back up the next frame). The
        // `JITTER_BUFFER_PX` floor keeps wrap_width strictly larger
        // than the measured text so this never happens.
        const JITTER_BUFFER_PX: f32 = 4.0;
        let wrap_width = if let Some(w) = self.text_box_width {
            w.max(MIN_TEXT_BOX_WIDTH)
        } else {
            let measured = canvas
                .measure_text(self.pos.x, self.pos.y, text, &base_paint)
                .ok()
                .map(|m| m.width())
                .unwrap_or(0.0);
            if measured > 0.0 {
                (measured + JITTER_BUFFER_PX).max(MIN_TEXT_BOX_WIDTH)
            } else {
                DEFAULT_INITIAL_BOX_WIDTH
            }
        };

        let lines = canvas.break_text_vec(wrap_width, text, &base_paint)?;
        self.layout.borrow_mut().line_ranges = lines.clone();

        let font_metrics = canvas.measure_font(&base_paint)?;
        let measured_cursor = canvas
            .measure_text(self.pos.x, self.pos.y, "|", &base_paint)
            .ok();

        let mut line_height = measured_cursor
            .as_ref()
            .map(|metrics| metrics.height())
            .unwrap_or(0.0);
        if line_height <= 0.0 {
            let ascender_plus_descender = font_metrics.ascender() + font_metrics.descender();
            if ascender_plus_descender.abs() > f32::EPSILON {
                line_height = ascender_plus_descender.abs() / canva_scale;
            }
        }
        if line_height <= 0.0 {
            line_height = font_metrics.height() / canva_scale;
        }

        // Caret extent — sized to roughly cap-height (baseline →
        // top of tall glyphs) instead of the full line_height (which
        // includes the leading above ascenders). Using the full
        // line_height made the cursor overshoot well above the
        // visible glyph tops and made the cream pill (which is keyed
        // off `cursor_height`) balloon to nearly twice the rendered
        // text's height. The 0.72 factor approximates the typical
        // cap-height-to-line-height ratio across UI fonts (cap ≈
        // 0.7em, line ≈ 1.2em → 0.7/1.2 ≈ 0.58, but bumped up so
        // ascenders like "d"/"h" still fit cleanly).
        let cursor_height = if line_height.abs() > f32::EPSILON {
            line_height.abs() * 0.72
        } else {
            (font_metrics.height() / canva_scale).abs() * 0.72
        };
        let cursor_top_offset = -cursor_height;

        // CSS→image conversion factor (DPR-aware, post-zoom) shared
        // between the pill pad, outline, and caret metrics so all
        // these dimensions stay locked together at every zoom level.
        let dpr_for_pads = crate::femtovg_area::current_device_pixel_ratio().max(0.0001);
        let css_to_image_dpr_for_caret =
            dpr_for_pads / canvas.transform().average_scale().max(0.0001);
        let pill_pad_y_top_img_for_caret = PILL_PAD_Y_TOP_CSS * css_to_image_dpr_for_caret;
        let descender_estimate_img_for_caret = line_height.abs() * DESCENDER_RATIO_OF_LINE_HEIGHT;
        let pill_pad_y_bottom_img_for_caret = (PILL_PAD_Y_BOTTOM_CSS * css_to_image_dpr_for_caret)
            .max(
                descender_estimate_img_for_caret
                    + DESCENDER_SAFETY_CSS * css_to_image_dpr_for_caret,
            );
        // Caret spans the full cream pill: pad above cap-top + glyph
        // body + pad (or descender) below baseline — the blinking caret
        // reads as a full-height insertion mark rather than a short bar.
        let caret_top_offset = cursor_top_offset - pill_pad_y_top_img_for_caret;
        let caret_height =
            cursor_height + pill_pad_y_top_img_for_caret + pill_pad_y_bottom_img_for_caret;

        // Build line layouts up-front and capture each line's natural
        // width so we can compute the `center_off` that horizontally
        // centers the line within the wrap area. The cached `start_x`
        // is the single source of truth for "where this line begins
        // in image space" — used by the cursor caret, selection rects,
        // glyph hit-test, pill background, and the final glyph draw.
        // Without this, the caret/selection would sit at `self.pos.x`
        // while the glyphs/pill were shifted right by `center_off`,
        // visibly drifting the caret left of the text whenever the
        // wrap area was wider than the line.
        let mut line_widths: Vec<f32> = Vec::with_capacity(lines.len());
        let mut line_layouts: Vec<LineLayout> = Vec::with_capacity(lines.len());
        let mut baseline = self.pos.y;
        for line_range in &lines {
            let line_text = &text[line_range.clone()];
            let line_w = canvas
                .measure_text(self.pos.x, baseline, line_text, &base_paint)
                .ok()
                .map(|m| m.width())
                .unwrap_or(0.0);
            let center_off = ((wrap_width - line_w) / 2.0).max(0.0);
            line_widths.push(line_w);
            line_layouts.push(LineLayout {
                range: line_range.clone(),
                baseline,
                start_x: self.pos.x + center_off,
            });
            baseline += line_height;
        }

        let cursor_metrics = CursorMetrics {
            top_offset: cursor_top_offset,
            height: cursor_height,
            line_height,
            caret_top_offset,
            caret_height,
        };

        let layout_context = TextDrawingContext {
            paint: &base_paint,
            text,
            lines: &line_layouts,
            wrap_width,
            base_x: self.pos.x,
        };

        // Selection blanks the caret, so resolve that flag up front;
        // the rect itself is drawn below, *after* the pill, so it
        // shows on top of the cream backdrop instead of being hidden.
        let cursor_visible_now = self.text_buffer.selection_bounds().is_none();
        *self.cursor_visible.borrow_mut() = cursor_visible_now;

        //calculate rect and glyphs
        let mut draw_baseline = self.pos.y;
        let mut layout = self.layout.borrow_mut();
        // Deref once so the split borrows below see disjoint fields of
        // a plain `&mut LayoutCache` rather than two reborrows of the
        // `RefMut` (Rust's split-borrow rules don't apply through the
        // `Deref` boundary).
        let layout: &mut LayoutCache = &mut layout;
        let rect = &mut layout.rect;
        let glyphs = &mut layout.glyphs;

        glyphs.clear();
        {
            let mut top = 0;
            let mut left = 0;
            let mut width = 0;
            let mut height = 0;

            for line in &line_layouts {
                let mut line_glyphs = Vec::new();

                let start = text[..line.range.start].chars().count();
                let end = text[..line.range.end].chars().count();

                for i in start..end {
                    let segments =
                        self.segments_for_line_span(canvas, &layout_context, line, i..i + 1);

                    for (start_x, end_x) in segments {
                        let offset_y = cursor_metrics.height * 0.1;
                        let y = (line.baseline + cursor_metrics.top_offset + offset_y) as i32;
                        let h = cursor_metrics.height as i32;
                        let x = start_x as i32;
                        let w = (end_x - start_x) as i32;
                        line_glyphs.push(Rectangle::new(x, y, w, h));

                        if top == 0 {
                            top = y;
                        }

                        if left == 0 {
                            left = x;
                        }

                        width = (end_x as i32 - left).max(width);
                        height = y + h - top;
                    }
                }

                glyphs.push(line_glyphs);

                rect.set_height(height);
                rect.set_width(width);
                rect.set_x(left);
                rect.set_y(top);
            }
        }

        // Per the user spec: the pill (and outline) measure padding
        // from the BASELINE, not from descenders. Padding above
        // cap-height + below baseline = symmetric breathing room
        // around what reads as "the text"; descenders (g, p, y,
        // commas) are allowed to poke below the pill. Without this
        // rule, a single comma would force the pill to be ~25%
        // taller than visually warranted.
        let line_extent_img = cursor_height;
        let line_count = line_layouts.len().max(1) as f32;
        // Top of the first line is at `pos.y - cursor_height`. Total
        // stack for N lines = (N-1) * line_height + cursor_height
        // (the last line ends at the baseline, not below it).
        let stack_top = self.pos.y - cursor_height;
        let stack_height = (line_count - 1.0) * line_height + line_extent_img;

        // DPR-aware CSS→image factor so the editing outline + padding
        // match the on-screen size used by SelectionOverlay (which
        // also factors DPR). Without DPR the editing visuals end up
        // half-sized on retina.
        let dpr_for_pads = crate::femtovg_area::current_device_pixel_ratio().max(0.0001);
        let css_to_image_dpr = dpr_for_pads / canvas.transform().average_scale().max(0.0001);
        let pill_pad_x_img = PILL_PAD_X_CSS * css_to_image_dpr;
        let pill_pad_y_top_img = PILL_PAD_Y_TOP_CSS * css_to_image_dpr;
        // Bottom pad floors at 8 CSS px (symmetric with the top), but
        // grows when the font's descender would otherwise exceed it —
        // at 3× annotation multipliers the descender on commas /
        // apostrophes is well past 8 image px, so a fixed 8-px floor
        // would let them poke below the pill. Keeping a small safety
        // cushion above the descender keeps the glyph visibly INSIDE
        // the cream area instead of riding the edge.
        let descender_estimate_img = line_height.abs() * DESCENDER_RATIO_OF_LINE_HEIGHT;
        let pill_pad_y_bottom_floor = PILL_PAD_Y_BOTTOM_CSS * css_to_image_dpr;
        let pill_pad_y_bottom_img = pill_pad_y_bottom_floor
            .max(descender_estimate_img + DESCENDER_SAFETY_CSS * css_to_image_dpr);
        let outline_pad_img = OUTLINE_PADDING_CSS * css_to_image_dpr;
        let pad_x_img = pill_pad_x_img + outline_pad_img;
        let pad_y_top_img = pill_pad_y_top_img + outline_pad_img;
        let pad_y_bottom_img = pill_pad_y_bottom_img + outline_pad_img;
        // editing_box wraps the pill stack with `outline_pad` on
        // every side. The pill itself adds asymmetric pill_pad
        // (less above cap-height, more below baseline) so commas
        // and small descenders sit comfortably without forcing a
        // taller pill on every line.
        let editing_box = Rect {
            pos: Vec2D::new(self.pos.x - pad_x_img, stack_top - pad_y_top_img),
            size: Vec2D::new(
                wrap_width + pad_x_img * 2.0,
                stack_height + pad_y_top_img + pad_y_bottom_img,
            ),
        };
        layout.editing_rect = editing_box;
        layout.line_height = cursor_height;
        layout.css_to_image = css_to_image_dpr;
        // Measure the full text on a single line (no wrapping) so
        // bounds() can estimate live line count during a drag.
        let natural_w = canvas
            .measure_text(self.pos.x, self.pos.y, text, &base_paint)
            .ok()
            .map(|m| m.width())
            .unwrap_or(0.0);
        layout.natural_text_width = natural_w;

        // Blue editing outline + side/corner handles (replaces the legacy
        // orange debug rect): a thin rounded outline around the wrap area
        // with draggable handles, visible during text creation/editing
        // and replaced by the PointerTool's glow halo once committed.
        // Draw the blue outline whenever the text is being edited
        // OR is currently selected (renderer publishes selection
        // state via the thread-local). Doing this inside `draw`
        // means the outline geometry is always based on the SAME
        // line-break + measurements that produced the cream pills
        // and glyphs in this same frame — no more 1-frame lag from
        // a stale `render_glow` cache during handle drags.
        let is_selected = crate::femtovg_area::current_drawable_is_selected();
        let render_outline = self.editing || is_selected;
        if render_outline {
            // Scale CSS-pixel widths to image units. Use the
            // renderer-published DPR so on-screen sizing stays
            // consistent across HiDPI; without DPR we'd get a
            // half-size outline on retina displays.
            let img_to_canvas = canvas.transform().average_scale().max(0.0001);
            let css_to_image_dpr =
                crate::femtovg_area::current_device_pixel_ratio().max(0.0001) / img_to_canvas;

            // While actively editing, use a solid outline — the user
            // is interacting with this exact box. Once the text is
            // committed and just selected, switch to a dashed outline
            // so the two states read as visually distinct without
            // needing extra UI chrome.
            let path = if self.editing {
                let mut p = Path::new();
                p.rounded_rect(
                    editing_box.pos.x,
                    editing_box.pos.y,
                    editing_box.size.x,
                    editing_box.size.y,
                    OUTLINE_CORNER_RADIUS_CSS * css_to_image_dpr,
                );
                p
            } else {
                dashed_rounded_rect_path(
                    editing_box.pos.x,
                    editing_box.pos.y,
                    editing_box.size.x,
                    editing_box.size.y,
                    OUTLINE_CORNER_RADIUS_CSS * css_to_image_dpr,
                    SELECTION_DASH_LEN_CSS * css_to_image_dpr,
                    SELECTION_GAP_LEN_CSS * css_to_image_dpr,
                )
            };
            let outline_color = femtovg::Color::rgbaf(
                SELECTION_BLUE.r,
                SELECTION_BLUE.g,
                SELECTION_BLUE.b,
                TEXT_OUTLINE_ALPHA,
            );
            let mut outline = Paint::color(outline_color);
            outline.set_line_width(EDITING_OUTLINE_WIDTH * css_to_image_dpr);
            outline.set_anti_alias(true);
            canvas.stroke_path(&path, &outline);
        }

        // Per-line cream pills behind each line's glyphs (when the
        // user picks the Rounded background). Each pill is sized to
        // the line's actual text width — so wrapped multi-line text
        // shows as a stack of narrow pills — and uses the SHARED
        // `line_extent_img` (cursor_height + clamped descender) so
        // the pill height grows in lockstep with the editing
        // outline instead of diverging at large font sizes.
        //
        // When the user has dragged the wrap area wider than the
        // longest line, each line centers horizontally within the
        // wrap so the text stays balanced inside the blue outline.
        let pill_corner = PILL_CORNER_RADIUS_CSS * css_to_image_dpr;

        if matches!(self.background, TextBackground::Rounded) {
            let bg_paint = Paint::color(femtovg::Color::rgba(248, 245, 235, 255));
            for (idx, layout) in line_layouts.iter().enumerate() {
                if text[layout.range.clone()].is_empty() {
                    continue;
                }
                let line_w = line_widths[idx];
                // Pill spans cap-top (baseline - cursor_height) down
                // to baseline, with asymmetric pad: small above
                // cap, generous below baseline. Descenders extend
                // below freely.
                let pill_top = layout.baseline - cursor_height - pill_pad_y_top_img;
                let pill_height = line_extent_img + pill_pad_y_top_img + pill_pad_y_bottom_img;
                let pill_left = layout.start_x - pill_pad_x_img;
                let pill_width = line_w + pill_pad_x_img * 2.0;
                let mut bg = Path::new();
                bg.rounded_rect(pill_left, pill_top, pill_width, pill_height, pill_corner);
                canvas.fill_path(&bg, &bg_paint);
            }
        }

        // Selection + preedit backgrounds layer above the cream pill
        // (so they're visible) and below the glyphs (so text stays
        // readable). When the pill was drawn AFTER these, the cream
        // fill hid the selection highlight entirely.
        if self.editing
            && let (Some(preedit), Some(preedit_range)) = (&self.preedit, &display.preedit_range)
        {
            self.draw_preedit_background(
                canvas,
                &layout_context,
                preedit,
                preedit_range,
                cursor_metrics,
            );
        }

        if let Some((sel_start_iter, sel_end_iter)) = self.text_buffer.selection_bounds() {
            let sel_start = sel_start_iter.offset() as usize;
            let sel_end = sel_end_iter.offset() as usize;

            for line in &line_layouts {
                let start_index = text[..line.range.start].chars().count();
                let end_index = text[..line.range.end].chars().count();

                let overlap_start = sel_start.max(start_index);
                let overlap_end = sel_end.min(end_index);
                if overlap_start >= overlap_end {
                    continue;
                }

                let segments = self.segments_for_line_span(
                    canvas,
                    &layout_context,
                    line,
                    overlap_start..overlap_end,
                );
                for (start_x, end_x) in segments {
                    let mut path = Path::new();

                    let offset_y = cursor_metrics.height * 0.1;
                    let y = line.baseline + cursor_metrics.top_offset + offset_y;
                    let h = cursor_metrics.height;
                    let x = start_x;
                    let w = end_x - start_x;

                    path.rect(x, y, w, h);
                    let mut paint = Paint::color(Color::rgbaf(0.3, 0.5, 1.0, 0.3)); // transparent blue
                    paint.set_anti_alias(true);
                    canvas.fill_path(&path, &paint);
                }
            }
        }

        for (idx, line_range) in lines.iter().enumerate() {
            canvas.fill_text(
                line_layouts[idx].start_x,
                draw_baseline,
                &text[line_range.clone()],
                &base_paint,
            )?;
            draw_baseline += line_height;
        }

        if self.editing
            && let (Some(preedit), Some(preedit_range)) = (&self.preedit, &display.preedit_range)
        {
            self.draw_preedit_overlays(
                canvas,
                font,
                &layout_context,
                preedit,
                preedit_range,
                cursor_metrics,
            )?;
        }

        if self.editing {
            self.draw_cursor_and_update_ime(
                canvas,
                font,
                &layout_context,
                cursor_metrics,
                display.cursor_byte_pos,
                cursor_visible_now,
            );
            // Three editing-mode handles: left/right midpoints (drag to
            // change `text_box_width`) and bottom-right corner (drag to
            // change font size + width). Drawn last so they sit on top of
            // the text and outline.
            self.draw_editing_handles(canvas, editing_box);
        }

        Ok(())
    }

    fn bounds(&self) -> Option<Rect> {
        // Recompute the selection rect FRESH from current state on
        // every call rather than returning the cached editing_rect.
        // Reason: during a drag, `move_handle` mutates `pos` and
        // `text_box_width` but the cached editing_rect is still from
        // the last draw — and `render_glow` (which uses bounds())
        // runs BEFORE the next draw repopulates it. Returning the
        // cache made the blue outline lag a frame behind the drag,
        // visible as the outline snapping to its final size only
        // after mouse-up. Using current pos + width + cached line
        // metrics here makes the outline track the drag in real time.
        let layout = self.layout.borrow();
        let line_height = layout.line_height;
        let css_to_image = layout.css_to_image;
        let natural_w = layout.natural_text_width.max(0.0);
        let cached_line_count = layout.line_ranges.len().max(1) as f32;
        drop(layout);
        if line_height <= 0.0 || css_to_image <= 0.0 {
            return None;
        }
        let total_pad_x = SELECTION_PAD_X_CSS * css_to_image;
        let total_pad_y_top = SELECTION_PAD_Y_TOP_CSS * css_to_image;
        // Mirror the descender-aware pill bottom pad from `draw()` so
        // selection bounds / hit-test grow alongside it. Without this
        // the cached SELECTION_PAD_Y_BOTTOM_CSS would clip below the
        // real pill at large annotation sizes and the bottom-edge
        // handle wouldn't track the actual visible edge.
        let descender_estimate_img = line_height * DESCENDER_RATIO_OF_LINE_HEIGHT;
        let pill_pad_bottom_dyn = (PILL_PAD_Y_BOTTOM_CSS * css_to_image)
            .max(descender_estimate_img + DESCENDER_SAFETY_CSS * css_to_image);
        let total_pad_y_bottom = pill_pad_bottom_dyn + OUTLINE_PADDING_CSS * css_to_image;

        // Width: text_box_width when set, else the cached glyph
        // width + jitter buffer (matches the auto-fit branch in
        // draw). Floor to MIN_TEXT_BOX_WIDTH.
        let glyph_w = self.glyph_rect().map(|g| g.size.x).unwrap_or(0.0);
        let wrap_width = match self.text_box_width {
            Some(w) => w.max(MIN_TEXT_BOX_WIDTH).max(glyph_w),
            None => (glyph_w + 4.0).max(MIN_TEXT_BOX_WIDTH),
        };

        // Live line-count: estimate from the natural text width
        // divided by current wrap_width so the outline reflects
        // wrapping in real time during a drag, instead of waiting
        // for the next draw to re-run break_text_vec. The cached
        // natural-text-width covers the whole text laid out on a
        // single line; combined with current wrap_width this gives
        // the correct line count even mid-drag when the cached
        // line_ranges is still from the previous frame.
        let live_line_count = if natural_w > 0.0 && wrap_width > 0.0 {
            (natural_w / wrap_width).ceil().max(1.0)
        } else {
            cached_line_count
        };

        // Vertical: stack_top = pos.y - line_height (matches draw).
        let stack_top = self.pos.y - line_height;
        let stack_height = live_line_count * line_height;

        Some(Rect {
            pos: Vec2D::new(self.pos.x - total_pad_x, stack_top - total_pad_y_top),
            size: Vec2D::new(
                wrap_width + 2.0 * total_pad_x,
                stack_height + total_pad_y_top + total_pad_y_bottom,
            ),
        })
    }

    fn hit_test(&self, point: Vec2D, tolerance: f32) -> bool {
        // Use bounds() inflated by AT LEAST the pointer's full
        // handle hit-radius (= the visible handle's outer radius
        // plus generous slop) so clicks anywhere on or near a
        // resize handle still count as hits on the text. The
        // tolerance argument is the standard pointer slack —
        // taking the max of the two ensures we never UNDER-count.
        // Without this, a click on the OUTSIDE edge of a resize
        // handle (a few pixels past `bounds.right`) would miss the
        // text and bubble through to TextTool, which would commit
        // the current text and spawn an extra one at the click
        // position — the "phantom text box on handle drag" bug.
        let css_to_image = self.layout.borrow().css_to_image;
        // Match SelectionOverlay's outer handle radius (12/2 + 2 = 8
        // CSS px) and add a comfortable safety margin of another 8
        // px so the boundary hit-test doesn't fail at the very edge
        // of the visible handle disc.
        let handle_margin = 16.0 * css_to_image.max(1.0);
        let inflate = tolerance.max(handle_margin);
        match self.bounds() {
            Some(b) => b.inflated(inflate).contains(point),
            None => false,
        }
    }

    fn translate(&mut self, delta: Vec2D) {
        self.pos += delta;
        // Shift cached layout rects so bounds() stays valid until the
        // next draw recomputes them.
        let mut layout = self.layout.borrow_mut();
        let r = &layout.rect;
        layout.rect = Rectangle::new(
            r.x() + delta.x as i32,
            r.y() + delta.y as i32,
            r.width(),
            r.height(),
        );
        layout.editing_rect.pos += delta;
    }

    fn handles(&self) -> Vec<Handle> {
        // Use the committed-selection box (`bounds`) so handles span the
        // wrap area when the user set one, but stay snug to the glyphs
        // when they didn't. Empty/uninitialized text has no handles.
        let Some(b) = self.bounds() else {
            return Vec::new();
        };
        let center_y = b.pos.y + b.size.y / 2.0;
        let right = b.pos.x + b.size.x;
        let bottom = b.pos.y + b.size.y;
        vec![
            // Round side handles for left/right wrap-width adjust.
            Handle::new(HandleId::Left, Vec2D::new(b.pos.x, center_y)),
            Handle::new(HandleId::Right, Vec2D::new(right, center_y)),
            // Square bottom-right handle: visually distinct because
            // dragging it scales font size + width together (not a
            // pure resize).
            Handle::new(HandleId::BottomRight, Vec2D::new(right, bottom))
                .with_kind(HandleKind::Square),
        ]
    }

    fn move_handle(&mut self, handle: HandleId, to: Vec2D) {
        let Some(b) = self.bounds() else { return };
        let current_right = b.pos.x + b.size.x;
        let current_top = b.pos.y;
        let current_height = b.size.y.max(1.0);
        // Handles sit on the visible OUTLINE which is `SELECTION_PAD`
        // CSS px outside the rendered glyphs (glyph→cream pill→outline).
        // Convert a dragged handle position back into wrap-width space
        // by subtracting that pad in image-space units.
        let css_to_image = self.layout.borrow().css_to_image;
        let pad_x = SELECTION_PAD_X_CSS * css_to_image;
        // Total vertical padding (above the top of the glyphs + below
        // the bottom). Used to convert a dragged BottomRight handle
        // position back into glyph-height units: the handle sits on
        // the outline, which extends `pad_y` beyond the glyphs.
        let pad_y = (SELECTION_PAD_Y_TOP_CSS + SELECTION_PAD_Y_BOTTOM_CSS) * css_to_image;
        match handle {
            HandleId::Right => {
                let new_w = (to.x - self.pos.x - pad_x).max(MIN_TEXT_BOX_WIDTH);
                self.text_box_width = Some(new_w);
            }
            HandleId::Left => {
                // Pin the right edge; move pos.x to follow the dragged
                // left edge, and shrink/grow text_box_width to match.
                // Drag pos is at outline_left = self.pos.x − pad_x, so
                // the new self.pos.x = drag.x + pad_x.
                let new_x = (to.x + pad_x).min(current_right - MIN_TEXT_BOX_WIDTH);
                let new_w = current_right - new_x;
                self.pos.x = new_x;
                self.text_box_width = Some(new_w);
            }
            HandleId::BottomRight => {
                // Scale font size proportionally to the height change so
                // text reflows to roughly fit the dragged corner. Width
                // change is applied independently so the user can adjust
                // wrap separately from font size.
                let new_h = (to.y - current_top - pad_y).max(current_height * 0.2);
                let scale = (new_h / current_height).clamp(0.2, 5.0);
                let new_factor = (self.style.annotation_size_factor * scale).clamp(0.2, 5.0);
                self.style.annotation_size_factor = new_factor;
                let new_w = (to.x - self.pos.x - pad_x).max(MIN_TEXT_BOX_WIDTH);
                self.text_box_width = Some(new_w);
            }
            _ => {}
        }
    }

    fn set_style(&mut self, style: Style) {
        self.style = style;
    }

    fn style(&self) -> Option<Style> {
        Some(self.style)
    }

    fn set_text_background(&mut self, bg: TextBackground) {
        self.background = bg;
    }

    fn text_background(&self) -> Option<TextBackground> {
        Some(self.background)
    }

    fn tool_type(&self) -> Option<Tools> {
        Some(Tools::Text)
    }

    fn render_glow(
        &self,
        _canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: FontId,
        _bounds: (Vec2D, Vec2D),
        _device_pixel_ratio: f32,
    ) -> Result<()> {
        // Intentionally a no-op: the selection outline for Text is
        // drawn inside `Drawable::draw` (gated by the renderer's
        // CURRENT_SELECTED thread-local) so its geometry uses the
        // SAME line-break and font metrics computed in the same
        // draw call. Drawing it here would either lag a frame
        // behind during handle drags (cache from previous frame)
        // or duplicate-stroke the outline.
        let _ = GLOW_COLOR;
        Ok(())
    }
}

impl Text {
    fn draw_preedit_background(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        context: &TextDrawingContext<'_>,
        preedit: &Preedit,
        preedit_range: &Range<usize>,
        cursor: CursorMetrics,
    ) {
        for span in &preedit.spans {
            let Some(background_color) = span.background else {
                continue;
            };
            let global_start = preedit_range.start + span.range.start;
            let global_end = preedit_range.start + span.range.end;

            for line in context.lines {
                let overlap_start = global_start.max(line.range.start);
                let overlap_end = global_end.min(line.range.end);
                if overlap_start >= overlap_end {
                    continue;
                }
                let segments =
                    self.segments_for_line_span(canvas, context, line, overlap_start..overlap_end);
                for (start_x, end_x) in segments {
                    let width = (end_x - start_x).max(0.0);
                    if width <= f32::EPSILON {
                        continue;
                    }
                    let mut path = Path::new();
                    let top = line.baseline + cursor.top_offset;
                    path.rect(start_x, top, width, cursor.height);
                    let mut fill_paint = Paint::color(background_color.into());
                    fill_paint.set_anti_alias(true);
                    canvas.fill_path(&path, &fill_paint);
                }
            }
        }
    }

    fn draw_preedit_overlays(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        font: FontId,
        context: &TextDrawingContext<'_>,
        preedit: &Preedit,
        preedit_range: &Range<usize>,
        cursor: CursorMetrics,
    ) -> Result<()> {
        for span in &preedit.spans {
            let global_start = preedit_range.start + span.range.start;
            let global_end = preedit_range.start + span.range.end;

            for line in context.lines {
                let overlap_start = global_start.max(line.range.start);
                let overlap_end = global_end.min(line.range.end);
                if overlap_start >= overlap_end {
                    continue;
                }
                let segments =
                    self.segments_for_line_span(canvas, context, line, overlap_start..overlap_end);
                if segments.is_empty() {
                    continue;
                }

                if let Some(color) = span.foreground {
                    let mut overlay_paint: Paint = self.style.into();
                    overlay_paint.set_font(&[font]);
                    overlay_paint.set_color(color.into());
                    for (start_x, end_x) in &segments {
                        let width = (*end_x - *start_x).max(0.0);
                        if width <= f32::EPSILON {
                            continue;
                        }
                        canvas.save();
                        canvas.scissor(
                            (*start_x - 1.0).floor(),
                            (line.baseline + cursor.top_offset - 1.0).floor(),
                            (width + 2.0).ceil(),
                            (cursor.height + 2.0).ceil(),
                        );
                        canvas.fill_text(
                            line.start_x,
                            line.baseline,
                            &context.text[line.range.clone()],
                            &overlay_paint,
                        )?;
                        canvas.restore();
                    }
                }

                if span.underline != UnderlineKind::None {
                    let color = span
                        .underline_color
                        .or(span.foreground)
                        .unwrap_or(self.style.color);
                    self.draw_underline_segments(
                        canvas,
                        &segments,
                        line.baseline + cursor.top_offset,
                        cursor.height,
                        span.underline,
                        color,
                    );
                }
            }
        }

        Ok(())
    }

    fn draw_underline_segments(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        segments: &[(f32, f32)],
        line_top: f32,
        cursor_height: f32,
        underline: UnderlineKind,
        color: crate::style::Color,
    ) {
        if segments.is_empty() {
            return;
        }
        let mut paint = Paint::color(color.into());
        let thickness = (cursor_height * 0.08).clamp(1.0, cursor_height / 2.0);
        paint.set_line_width(thickness);
        paint.set_anti_alias(true);

        let base_y = line_top + cursor_height - thickness * 0.5;

        for &(start_x, end_x) in segments {
            if end_x - start_x <= f32::EPSILON {
                continue;
            }
            match underline {
                UnderlineKind::Double => {
                    let mut first = Path::new();
                    first.move_to(start_x, base_y - thickness);
                    first.line_to(end_x, base_y - thickness);
                    canvas.stroke_path(&first, &paint);

                    let mut second = Path::new();
                    second.move_to(start_x, base_y + thickness * 0.5);
                    second.line_to(end_x, base_y + thickness * 0.5);
                    canvas.stroke_path(&second, &paint);
                }
                UnderlineKind::None => {}
                _ => {
                    let mut path = Path::new();
                    path.move_to(start_x, base_y);
                    path.line_to(end_x, base_y);
                    canvas.stroke_path(&path, &paint);
                }
            }
        }
    }

    fn segments_for_line_span(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        context: &TextDrawingContext<'_>,
        line: &LineLayout,
        range: Range<usize>,
    ) -> Vec<(f32, f32)> {
        if range.start >= range.end {
            return Vec::new();
        }

        let chars_without_newline: Vec<(usize, char)> = context.text.char_indices().collect();

        let range_start_byte = chars_without_newline
            .get(range.start)
            .map(|(i, _)| *i)
            .unwrap_or(context.text.len());

        let range_end_byte = chars_without_newline
            .get(range.end)
            .map(|(i, _)| *i)
            .unwrap_or(context.text.len());

        let line_start = line.range.start;
        let line_end = line.range.end;
        let overlap_start = range_start_byte.max(line_start).min(line_end);
        let overlap_end = range_end_byte.max(line_start).min(line_end);

        if overlap_start >= overlap_end {
            return Vec::new();
        }

        let line_text = &context.text[line.range.clone()];

        let start_byte = overlap_start.saturating_sub(line_start);
        let end_byte = overlap_end.saturating_sub(line_start);

        let prefix = &line_text[..start_byte];
        let selected = &line_text[start_byte..end_byte].replace("\n", "");

        let start_x: f32 = line.start_x + Self::text_width(canvas, context.paint, prefix);
        let width = Self::text_width(canvas, context.paint, selected);

        vec![(start_x, start_x + width.max(0.0))]
    }

    fn caret_top_left(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        context: &TextDrawingContext<'_>,
        cursor_byte_pos: usize,
        cursor: CursorMetrics,
    ) -> (f32, f32) {
        // X for "no glyphs to position against" states (empty buffer,
        // caret on a brand-new line after Enter). When the user has
        // an explicit `text_box_width` the centered text idiom holds,
        // so we sit the caret at the horizontal middle of the wrap
        // area; in the auto-fit case the wrap is the placeholder size
        // (`DEFAULT_INITIAL_BOX_WIDTH`) and centering there would put
        // the caret far to the right of where the first typed char
        // ends up, causing a large leftward jump on the first keystroke.
        let empty_caret_x = if self.text_box_width.is_some() {
            context.base_x + context.wrap_width / 2.0
        } else {
            context.base_x
        };

        // Vertical caret top uses `caret_top_offset` (pill extent),
        // not `top_offset` (glyph extent), so the caret reaches the
        // top of the cream pill.
        let caret_top = cursor.caret_top_offset;

        if context.lines.is_empty() {
            return (empty_caret_x, self.pos.y + caret_top);
        }

        let mut newline_pending_baseline: Option<f32> = None;

        for line in context.lines {
            let line_text = &context.text[line.range.clone()];

            if cursor_byte_pos < line.range.end {
                let prefix_len = cursor_byte_pos
                    .saturating_sub(line.range.start)
                    .min(line_text.len());
                let prefix = &line_text[..prefix_len];
                let offset = Self::text_width(canvas, context.paint, prefix);
                return (line.start_x + offset, line.baseline + caret_top);
            }

            if cursor_byte_pos == line.range.end {
                if line_text.ends_with('\n') {
                    // The caret is positioned right after a manual line break,
                    // so place it on the next visual line instead.
                    newline_pending_baseline = Some(line.baseline + caret_top + cursor.line_height);
                    continue;
                }
                let offset = Self::text_width(canvas, context.paint, line_text);
                return (line.start_x + offset, line.baseline + caret_top);
            }
        }

        if let Some(baseline) = newline_pending_baseline {
            return (empty_caret_x, baseline);
        }

        if let Some(last_line) = context.lines.last() {
            let line_text = &context.text[last_line.range.clone()];
            let offset = Self::text_width(canvas, context.paint, line_text);
            (
                last_line.start_x + offset,
                last_line.baseline + caret_top + cursor.line_height,
            )
        } else {
            (empty_caret_x, self.pos.y + caret_top)
        }
    }

    fn draw_cursor_and_update_ime(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        font: FontId,
        context: &TextDrawingContext<'_>,
        cursor: CursorMetrics,
        cursor_byte_pos: usize,
        cursor_visible: bool,
    ) {
        let (cursor_x, cursor_top) = self.caret_top_left(canvas, context, cursor_byte_pos, cursor);
        let caret_height = cursor.caret_height;

        let mut caret_paint: Paint = self.style.into();
        caret_paint.set_font(&[font]);

        // Blink: combine the no-selection visibility flag with a
        // 500 ms phase from the system clock so the caret pulses
        // even between input events. The TextTool runs a tick
        // timer (cursor_blink_timer) that fires `Refresh` events
        // at 250 ms intervals so each phase boundary triggers a
        // redraw — without it the canvas would only redraw on
        // user input and the blink would be invisible.
        let blink_on = {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            (now_ms / 500).is_multiple_of(2)
        };
        if cursor_visible && blink_on {
            // Draw the caret as a 2 CSS px wide rounded rect filled
            // with the text color. Using `fill_path` on a rect (vs
            // `stroke_path` on a line) is more reliable in femtovg
            // — degenerate-line strokes can render at 0 width on
            // some backends. DPR-aware so the on-screen width is
            // consistent across HiDPI. The caret already spans the
            // full pill height via `caret_height`, so we don't add
            // extra vertical padding here — doing so used to make
            // the bar overshoot the pill by ~10%.
            let dpr = crate::femtovg_area::current_device_pixel_ratio().max(0.0001);
            let img_to_canvas = canvas.transform().average_scale().max(0.0001);
            let css_to_image_dpr = dpr / img_to_canvas;
            let half_w = 1.0 * css_to_image_dpr;
            let radius = 1.0 * css_to_image_dpr;
            let mut path = Path::new();
            path.rounded_rect(
                cursor_x - half_w,
                cursor_top,
                half_w * 2.0,
                caret_height,
                radius,
            );
            canvas.fill_path(&path, &caret_paint);
        }

        if self.editing
            && let Some(handle) = &self.im_context
        {
            let transform = canvas.transform();
            let widget_scale = handle.widget.scale_factor().max(1) as f32;
            let (x1, y1) = transform.transform_point(cursor_x, cursor_top);
            let (x2, y2) = transform.transform_point(cursor_x + 1.0, cursor_top + caret_height);
            let logical_x = (x1 / widget_scale).floor() as i32;
            let logical_y = (y1 / widget_scale).floor() as i32;
            let logical_width = ((x2 - x1).abs() / widget_scale).ceil().max(1.0) as i32;
            let logical_height = ((y2 - y1).abs() / widget_scale).ceil().max(1.0) as i32;
            let rect = Rectangle::new(logical_x, logical_y, logical_width, logical_height.max(1));
            handle.im_context.set_cursor_location(&rect);
        }
    }

    fn text_width(
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        paint: &Paint,
        text: &str,
    ) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        canvas
            .measure_text(0.0, 0.0, text, paint)
            .map(|metrics| metrics.width())
            .unwrap_or(0.0)
    }

    /// Cached glyph bounding rect in image coordinates, or `None` when
    /// the text hasn't been measured yet or is empty.
    fn glyph_rect(&self) -> Option<Rect> {
        let r = self.layout.borrow().rect;
        if r.width() <= 0 || r.height() <= 0 {
            return None;
        }
        Some(Rect {
            pos: Vec2D::new(r.x() as f32, r.y() as f32),
            size: Vec2D::new(r.width() as f32, r.height() as f32),
        })
    }

    /// Current wrap-area rect (the editing-mode box). Pulled from the
    /// per-frame cache populated in `draw`. Returns the cached value
    /// even when stale — callers that need a freshly measured rect must
    /// queue a draw first.
    fn editing_box(&self) -> Rect {
        self.layout.borrow().editing_rect
    }

    /// Build the three editing-mode handles from a wrap-area rect.
    /// Free of `&self` so both `editing_handles()` (which reads the
    /// cached editing_rect) and `draw_editing_handles` (which gets
    /// the rect by parameter, sometimes mid-draw while the layout
    /// cache is mut-borrowed) can use the same construction.
    fn editing_handles_from_box(b: Rect) -> Vec<Handle> {
        if b.size.x <= 0.0 || b.size.y <= 0.0 {
            return Vec::new();
        }
        let center_y = b.pos.y + b.size.y / 2.0;
        let right = b.pos.x + b.size.x;
        let bottom = b.pos.y + b.size.y;
        vec![
            Handle::new(HandleId::Left, Vec2D::new(b.pos.x, center_y))
                .with_hit_radius(HANDLE_HIT_RADIUS),
            Handle::new(HandleId::Right, Vec2D::new(right, center_y))
                .with_hit_radius(HANDLE_HIT_RADIUS),
            Handle::new(HandleId::BottomRight, Vec2D::new(right, bottom))
                .with_hit_radius(HANDLE_HIT_RADIUS)
                // Square — same semantic as the committed-selection
                // BR handle: scales font size + width together.
                .with_kind(HandleKind::Square),
        ]
    }

    /// Positions of the three editing-mode handles at the wrap-area
    /// edges. Used by `TextTool`'s editing-mode handle hit-test.
    fn editing_handles(&self) -> Vec<Handle> {
        Self::editing_handles_from_box(self.editing_box())
    }

    /// Render the three editing-mode handles at the wrap-area edges
    /// using the shared `tools::render_handles` so editing-mode and
    /// committed-selection (PointerTool::SelectionOverlay) handles
    /// look pixel-for-pixel identical.
    fn draw_editing_handles(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        editing_box: Rect,
    ) {
        // DPR-aware sizing — same formula PointerTool uses so the two
        // handle paths track each other across zoom + DPR changes.
        let dpr = crate::femtovg_area::current_device_pixel_ratio().max(0.0001);
        let img_to_canvas = canvas.transform().average_scale().max(0.0001);
        let css_to_image_dpr = dpr / img_to_canvas;
        let handles = Self::editing_handles_from_box(editing_box);
        crate::tools::render_handles(canvas, &handles, css_to_image_dpr);
    }
}

#[derive(Default)]
pub struct TextTool {
    text: Option<Text>,
    style: Style,
    /// Last-selected background style from the toolbar dropdown,
    /// captured into each new Text drawable on creation. Persisted
    /// for the duration of the satty session — defaults to Rounded
    /// to keep the existing look out of the box.
    background: TextBackground,
    input_enabled: bool,
    im_context: Option<InputContext>,
    sender: Option<Sender<SketchBoardInput>>,
    drag_start_pos: Vec2D,
    dragged: Rc<RefCell<bool>>,
    /// In-flight editing-mode handle drag (left/right midpoint or
    /// bottom-right corner). Snapshots the original geometry so each
    /// `UpdateDrag` recomputes from a stable baseline, mirroring
    /// `PointerTool::DragState`. `None` outside a handle drag.
    handle_drag: Option<EditHandleDrag>,
    /// glib timer that fires `Refresh` events every 250 ms while a
    /// text is being edited so the cursor blink phase (computed
    /// from system time inside `draw_cursor_and_update_ime`) gets
    /// rendered. Stored so we can cancel it on commit/deactivate
    /// — leaking the timer would keep firing redraws after the
    /// text tool exits.
    cursor_blink_timer: Option<gtk::glib::SourceId>,
    /// Set when `self.text` is a clone of an already-committed drawable
    /// (re-edit via double-click). Causes the eventual commit to emit
    /// `ModifyDrawable(id, _)` instead of `Commit(_)` so the existing
    /// stacked drawable is replaced in-place. The renderer also hides
    /// the original via `dragging_drawable_id` while editing.
    edit_target_id: Option<DrawableId>,
}

impl TextTool {
    /// Begin emitting `Refresh` events at the cursor blink cadence
    /// (250 ms — half the 500 ms blink period so each phase
    /// boundary triggers a redraw). Idempotent: cancels any
    /// existing timer first so re-entering edit mode doesn't pile
    /// up duplicate ticks.
    fn start_cursor_blink_timer(&mut self) {
        self.stop_cursor_blink_timer();
        let Some(sender) = self.sender.clone() else {
            return;
        };
        let id = gtk::glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
            let _ = sender.send(SketchBoardInput::Refresh);
            gtk::glib::ControlFlow::Continue
        });
        self.cursor_blink_timer = Some(id);
    }

    fn stop_cursor_blink_timer(&mut self) {
        if let Some(id) = self.cursor_blink_timer.take() {
            id.remove();
        }
    }
}

/// Stable snapshot of an in-progress Text's geometry at the moment a
/// handle drag began. `move_handle` is called each `UpdateDrag` with
/// `anchor + delta_from_begin`, mutating a fresh clone of these original
/// values so successive drag updates don't accumulate.
struct EditHandleDrag {
    handle: HandleId,
    original_pos: Vec2D,
    original_text_box_width: Option<f32>,
    original_size_factor: f32,
    /// Cached line height (image-space px) at BeginDrag. Restored each
    /// `UpdateDrag` so `move_handle`'s height-scale math doesn't drift
    /// as `last_line_height` updates per frame.
    original_line_height: f32,
    /// Image-space position of the handle at BeginDrag.
    anchor: Vec2D,
}

impl Tool for TextTool {
    fn get_tool_type(&self) -> super::Tools {
        Tools::Text
    }

    fn input_enabled(&self) -> bool {
        self.input_enabled
    }

    fn set_input_enabled(&mut self, value: bool) {
        self.input_enabled = value;
    }

    fn set_im_context(&mut self, context: Option<InputContext>) {
        self.im_context = context.clone();
        if let Some(text) = &mut self.text {
            text.im_context = context;
        }
    }

    fn get_drawable(&self) -> Option<&dyn Drawable> {
        match &self.text {
            Some(d) => Some(d),
            None => None,
        }
    }

    fn handle_style_event(&mut self, style: Style) -> ToolUpdateResult {
        self.style = style;
        if let Some(t) = &mut self.text {
            t.style = style;
            ToolUpdateResult::Redraw
        } else {
            ToolUpdateResult::Unmodified
        }
    }

    fn set_text_background(&mut self, bg: TextBackground) {
        // New text drawables created from now on use the picked
        // background. Also update the in-progress one if any so the
        // user sees the change live without having to click away
        // and re-create.
        self.background = bg;
        if let Some(t) = &mut self.text {
            t.background = bg;
        }
    }

    fn handle_text_event(&mut self, event: crate::sketch_board::TextEventMsg) -> ToolUpdateResult {
        if let Some(t) = &mut self.text {
            match event {
                TextEventMsg::Commit(text) => {
                    //delete selection
                    Self::handle_text_buffer_action(t, Action::Delete, ActionScope::None);
                    //update input text
                    t.preedit = None;
                    t.text_buffer.insert_at_cursor(&text);
                    ToolUpdateResult::Redraw
                }
                TextEventMsg::Preedit {
                    text,
                    cursor_chars,
                    spans,
                } => {
                    if text.is_empty() {
                        if t.preedit.take().is_some() {
                            ToolUpdateResult::Redraw
                        } else {
                            ToolUpdateResult::Unmodified
                        }
                    } else {
                        t.preedit = Some(Preedit {
                            text,
                            cursor_chars,
                            spans,
                        });
                        ToolUpdateResult::Redraw
                    }
                }
                TextEventMsg::PreeditEnd => {
                    if t.preedit.take().is_some() {
                        ToolUpdateResult::Redraw
                    } else {
                        ToolUpdateResult::Unmodified
                    }
                }
            }
        } else {
            ToolUpdateResult::Unmodified
        }
    }

    fn handle_key_event(&mut self, event: KeyEventMsg) -> ToolUpdateResult {
        let mut tool_update_result = ToolUpdateResult::StopPropagation;
        if let Some(t) = &mut self.text {
            match event.key {
                Key::Return => match event.modifier {
                    ModifierType::SHIFT_MASK => {
                        //delete selection
                        Self::handle_text_buffer_action(t, Action::Delete, ActionScope::None);
                        t.text_buffer.insert_at_cursor("\n");
                        tool_update_result = ToolUpdateResult::RedrawAndStopPropagation;
                    }
                    _ => {
                        t.preedit = None;
                        t.editing = false;
                        t.im_context = None;
                        t.text_buffer
                            .select_range(&t.text_buffer.start_iter(), &t.text_buffer.start_iter());
                        let result = t.clone_box();
                        let edit_id = self.edit_target_id.take();
                        self.text = None;
                        self.input_enabled = false;
                        self.stop_cursor_blink_timer();
                        // Clear any lingering IM preedit so the next
                        // keypress lands in the canvas key handler
                        // (not silently swallowed by `filter_keypress`
                        // because the IM still thinks we're composing).
                        if let Some(ctx) = &self.im_context {
                            ctx.im_context.reset();
                        }
                        tool_update_result = match edit_id {
                            Some(id) => ToolUpdateResult::ModifyDrawable(id, result),
                            None => ToolUpdateResult::Commit(result),
                        };
                    }
                },
                Key::Escape => {
                    tool_update_result = self.handle_deactivated();
                }
                Key::BackSpace | Key::Delete => {
                    let ctrl_mask = match event.key {
                        Key::BackSpace => ActionScope::BackwardWord,
                        Key::Delete => ActionScope::ForwardWord,
                        _ => ActionScope::None,
                    };

                    let other_mask = match event.key {
                        Key::BackSpace => ActionScope::BackwardChar,
                        Key::Delete => ActionScope::ForwardChar,
                        _ => ActionScope::None,
                    };

                    if event.modifier == ModifierType::CONTROL_MASK {
                        tool_update_result =
                            Self::handle_text_buffer_action(t, Action::Delete, ctrl_mask);
                    } else {
                        tool_update_result =
                            Self::handle_text_buffer_action(t, Action::Delete, other_mask);
                    }
                }
                Key::Left | Key::Right | Key::Up | Key::Down => {
                    let ctrl_mask = match event.key {
                        Key::Left => ActionScope::BackwardWord,
                        Key::Right => ActionScope::ForwardWord,
                        Key::Up => ActionScope::BackwardLineAndWord,
                        Key::Down => ActionScope::ForwardLineAndWord,
                        _ => ActionScope::None,
                    };

                    let other_mask = match event.key {
                        Key::Left => ActionScope::BackwardChar,
                        Key::Right => ActionScope::ForwardChar,
                        Key::Up => ActionScope::BackwardLineAndWord,
                        Key::Down => ActionScope::ForwardLineAndWord,
                        _ => ActionScope::None,
                    };

                    let combine_mask = match event.key {
                        Key::Left => ActionScope::BackwardWord,
                        Key::Right => ActionScope::ForwardWord,
                        Key::Up => ActionScope::BackwardLineAndWord,
                        Key::Down => ActionScope::ForwardLineAndWord,
                        _ => ActionScope::None,
                    };

                    let ctrl_alt_mask = match event.key {
                        Key::Left => ActionScope::Left,
                        Key::Right => ActionScope::Right,
                        Key::Up => ActionScope::Up,
                        Key::Down => ActionScope::Down,
                        _ => ActionScope::None,
                    };

                    match event.modifier {
                        ModifierType::ALT_MASK => {
                            tool_update_result = ToolUpdateResult::Unmodified;
                        }
                        ModifierType::CONTROL_MASK => {
                            tool_update_result =
                                Self::handle_text_buffer_action(t, Action::MoveCursor, ctrl_mask);
                        }
                        ModifierType::SHIFT_MASK => {
                            tool_update_result =
                                Self::handle_text_buffer_action(t, Action::Select, other_mask);
                        }
                        m if m == ModifierType::ALT_MASK | ModifierType::CONTROL_MASK => {
                            tool_update_result = Self::handle_text_buffer_action(
                                t,
                                Action::MoveOrigin,
                                ctrl_alt_mask,
                            );
                        }
                        m if m
                            == ModifierType::ALT_MASK
                                | ModifierType::CONTROL_MASK
                                | ModifierType::SHIFT_MASK =>
                        {
                            tool_update_result = Self::handle_text_buffer_action(
                                t,
                                Action::NudgeOrigin,
                                ctrl_alt_mask,
                            );
                        }
                        m if m == ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK => {
                            tool_update_result =
                                Self::handle_text_buffer_action(t, Action::Select, combine_mask);
                        }
                        _ => {
                            tool_update_result =
                                Self::handle_text_buffer_action(t, Action::MoveCursor, other_mask);
                        }
                    }
                }
                Key::Home | Key::End => {
                    let ctrl_mask = match event.key {
                        Key::Home => ActionScope::BufferStart,
                        Key::End => ActionScope::BufferEnd,
                        _ => ActionScope::None,
                    };

                    let other_mask = match event.key {
                        Key::Home => ActionScope::BackwardLine,
                        Key::End => ActionScope::ForwardLine,
                        _ => ActionScope::None,
                    };

                    match event.modifier {
                        ModifierType::CONTROL_MASK => {
                            tool_update_result =
                                Self::handle_text_buffer_action(t, Action::MoveCursor, ctrl_mask);
                        }
                        ModifierType::SHIFT_MASK => {
                            tool_update_result =
                                Self::handle_text_buffer_action(t, Action::Select, other_mask);
                        }
                        _ => {
                            tool_update_result =
                                Self::handle_text_buffer_action(t, Action::MoveCursor, other_mask);
                        }
                    }
                }
                Key::a | Key::A => {
                    if event.modifier == ModifierType::CONTROL_MASK {
                        tool_update_result = Self::handle_text_buffer_action(
                            t,
                            Action::Select,
                            ActionScope::SelectAll,
                        );
                    }
                }
                Key::v | Key::V => {
                    let display = DisplayManager::get().default_display();
                    if display.is_none() {
                        eprintln!("Cannot open default display for clipboard.");
                        return ToolUpdateResult::StopPropagation;
                    }
                    let clipboard = display.unwrap().clipboard();
                    let buffer = t.text_buffer.clone();

                    Self::handle_text_buffer_action(t, Action::Delete, ActionScope::None);

                    let sender = self.sender.clone();

                    //async clipboard read
                    relm4::gtk::glib::MainContext::default().spawn_local(async move {
                        match clipboard.read_text_future().await {
                            Ok(Some(text)) => {
                                buffer.insert_at_cursor(&text);
                                if let Some(sender) = sender {
                                    sender.emit(SketchBoardInput::Refresh);
                                }
                            }
                            Ok(None) => {
                                eprintln!("Clipboard contains no text");
                            }
                            Err(err) => {
                                eprintln!("Clipboard read error: {}", err);
                            }
                        }
                    });
                }
                Key::c | Key::C => {
                    if event.modifier == ModifierType::CONTROL_MASK
                        && let Some(text) = &self.text
                    {
                        let buffer = text.text_buffer.clone();
                        if let Some((start, end)) = buffer.selection_bounds() {
                            let selected_text = buffer.text(&start, &end, false);

                            let display = DisplayManager::get().default_display();
                            if display.is_none() {
                                eprintln!("Cannot open default display for clipboard.");
                                return ToolUpdateResult::StopPropagation;
                            }

                            let clipboard = display.unwrap().clipboard();
                            clipboard.set_text(&selected_text);
                        }
                    }
                }
                Key::x | Key::X => {
                    if event.modifier == ModifierType::CONTROL_MASK
                        && let Some(text) = &mut self.text
                    {
                        let buffer = text.text_buffer.clone();
                        if let Some((start, end)) = buffer.selection_bounds() {
                            let selected_text = buffer.text(&start, &end, false);

                            let display = DisplayManager::get().default_display();
                            if display.is_none() {
                                eprintln!("Cannot open default display for clipboard.");
                                return ToolUpdateResult::StopPropagation;
                            }

                            let clipboard = display.unwrap().clipboard();
                            clipboard.set_text(&selected_text);

                            Self::handle_text_buffer_action(
                                text,
                                Action::Delete,
                                ActionScope::None,
                            );
                            tool_update_result = ToolUpdateResult::RedrawAndStopPropagation;
                        }
                    }
                }
                Key::Insert => {
                    if event.modifier == ModifierType::SHIFT_MASK {
                        let display = DisplayManager::get().default_display();
                        if display.is_none() {
                            eprintln!("Cannot open default display for clipboard.");
                            return ToolUpdateResult::StopPropagation;
                        }
                        let selection_clipboard = display.unwrap().primary_clipboard();
                        let buffer = t.text_buffer.clone();

                        Self::handle_text_buffer_action(t, Action::Delete, ActionScope::None);

                        let sender = self.sender.clone();

                        relm4::gtk::glib::MainContext::default().spawn_local(async move {
                            match selection_clipboard.read_text_future().await {
                                Ok(Some(text)) => {
                                    buffer.insert_at_cursor(&text);
                                    if let Some(sender) = sender {
                                        sender.emit(SketchBoardInput::Refresh);
                                    }
                                }
                                Ok(None) => {
                                    eprintln!("selection_clipboard contains no text");
                                }
                                Err(err) => {
                                    eprintln!("selection_clipboard read error: {}", err);
                                }
                            }
                        });
                    }
                }
                _ => {
                    tool_update_result = ToolUpdateResult::Unmodified;
                }
            }
        } else {
            tool_update_result = ToolUpdateResult::Unmodified;
        }
        tool_update_result
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        match event.type_ {
            MouseEventType::Click => {
                match event.button {
                    MouseButton::Primary => {
                        let pos = event.pos;
                        // Suppress click-bubble when the user just
                        // clicked on (or near) an editing handle.
                        // Without this, a click-and-release on a
                        // round side handle would fall through to the
                        // "click outside rect → commit + create new"
                        // branch below and spawn an unwanted second
                        // text box near the handle position.
                        if let Some(t) = &self.text
                            && t.editing
                        {
                            let on_handle = t
                                .editing_handles()
                                .into_iter()
                                .any(|h| h.pos.distance_to(&pos) <= h.hit_radius);
                            if on_handle {
                                return ToolUpdateResult::StopPropagation;
                            }
                        }
                        if let Some(t) = &mut self.text {
                            // Hit area for click-to-place-caret is the
                            // entire editing body (the blue-outlined
                            // wrap area) during editing, not just the
                            // glyph bbox — matches the i-beam hover
                            // zone so anywhere the user sees an i-beam
                            // actually drops the caret instead of
                            // committing the text and starting fresh.
                            let in_body = if t.editing {
                                t.editing_box().contains(pos)
                            } else {
                                t.layout
                                    .borrow()
                                    .rect
                                    .contains_point(pos.x as i32, pos.y as i32)
                            };
                            if in_body {
                                //calculate text cursor position
                                let mut index = 0;
                                let mut find_index = false;

                                let layout = t.layout.borrow();
                                let glyphs = &layout.glyphs;
                                for line in 0..glyphs.len() {
                                    let line_rect = glyphs.get(line).unwrap();

                                    for glyph in line_rect.iter() {
                                        if glyph.contains_point(pos.x as i32, pos.y as i32) {
                                            find_index = true;
                                            if pos.x > glyph.x() as f32 + glyph.width() as f32 / 2.0
                                            {
                                                index += 1;
                                            }
                                            break;
                                        }
                                        index += 1;
                                    }

                                    if find_index {
                                        break;
                                    }

                                    let first_ele = line_rect.iter().next().unwrap();
                                    if pos.y <= (first_ele.y() + first_ele.height()) as f32
                                        && line != glyphs.len() - 1
                                    {
                                        index -= 1;
                                        break;
                                    }
                                }

                                let buffer = &t.text_buffer;
                                let mut cursor_iter = buffer.iter_at_mark(&buffer.get_insert());
                                cursor_iter.set_offset(index);
                                t.text_buffer.place_cursor(&cursor_iter);

                                if event.n_pressed == 2 {
                                    let mut start_itr = cursor_iter;
                                    let mut end_itr = start_itr;
                                    start_itr.backward_word_start();
                                    end_itr.forward_word_end();
                                    t.text_buffer.select_range(&start_itr, &end_itr);
                                } else if event.n_pressed == 3 {
                                    let mut start_itr = cursor_iter;
                                    let mut end_itr = start_itr;
                                    while !start_itr.is_start() {
                                        start_itr.backward_line();
                                    }
                                    end_itr.forward_to_end();
                                    t.text_buffer.select_range(&start_itr, &end_itr);
                                }

                                return ToolUpdateResult::RedrawAndStopPropagation;
                            }
                        }

                        // Click-off semantics: if a text is currently being
                        // edited, the off-canvas click *only* commits + clears
                        // it — a new text box is NOT created on the same
                        // gesture. The user clicks again to create one.
                        if let Some(l) = self.text.as_mut() {
                            l.preedit = None;
                            l.editing = false;
                            l.im_context = None;
                            l.text_buffer.select_range(
                                &l.text_buffer.start_iter(),
                                &l.text_buffer.start_iter(),
                            );
                            let committed = l.clone_box();
                            let edit_id = self.edit_target_id.take();
                            self.text = None;
                            self.set_input_enabled(false);
                            self.stop_cursor_blink_timer();
                            if let Some(ctx) = &self.im_context {
                                ctx.im_context.reset();
                            }
                            return match edit_id {
                                Some(id) => ToolUpdateResult::ModifyDrawable(id, committed),
                                None => ToolUpdateResult::Commit(committed),
                            };
                        }

                        // No text in progress: this click *creates* a new one.
                        self.text = Some(Text::new(
                            event.pos,
                            self.style,
                            self.background,
                            self.im_context.clone(),
                        ));
                        // Start cursor blink redraws.
                        self.start_cursor_blink_timer();
                        self.set_input_enabled(true);
                        ToolUpdateResult::Redraw
                    }
                    _ => ToolUpdateResult::Unmodified,
                }
            }
            MouseEventType::Release => match event.button {
                MouseButton::Middle => {
                    if let Some(t) = &mut self.text {
                        let display = DisplayManager::get().default_display();
                        if display.is_none() {
                            eprintln!("Cannot open default display for clipboard.");
                            return ToolUpdateResult::StopPropagation;
                        }
                        let selection_clipboard = display.unwrap().primary_clipboard();
                        let buffer = t.text_buffer.clone();

                        Self::handle_text_buffer_action(t, Action::Delete, ActionScope::None);

                        let sender = self.sender.clone();
                        let dragged = self.dragged.clone();

                        relm4::gtk::glib::MainContext::default().spawn_local(async move {
                            match selection_clipboard.read_text_future().await {
                                Ok(Some(text)) => {
                                    if !*dragged.borrow() {
                                        buffer.insert_at_cursor(&text);
                                        if let Some(sender) = sender {
                                            sender.emit(SketchBoardInput::Refresh);
                                        }
                                    }
                                }
                                Ok(None) => {
                                    eprintln!("selection_clipboard contains no text");
                                }
                                Err(err) => {
                                    eprintln!("selection_clipboard read error: {}", err);
                                }
                            }
                        });
                    }

                    ToolUpdateResult::StopPropagation
                }
                _ => ToolUpdateResult::Unmodified,
            },
            MouseEventType::BeginDrag => {
                self.drag_start_pos = event.pos;
                // 1. Editing-mode handle drag (left/right/bottom-right of
                //    the wrap area) takes priority over text-cursor drag.
                if let Some(t) = &self.text
                    && t.editing
                {
                    let hit = t
                        .editing_handles()
                        .into_iter()
                        .find(|h| h.pos.distance_to(&event.pos) <= h.hit_radius);
                    if let Some(handle) = hit {
                        self.handle_drag = Some(EditHandleDrag {
                            handle: handle.id,
                            original_pos: t.pos,
                            original_text_box_width: t.text_box_width,
                            original_size_factor: t.style.annotation_size_factor,
                            original_line_height: t.layout.borrow().line_height,
                            anchor: handle.pos,
                        });
                        return ToolUpdateResult::StopPropagation;
                    }
                }
                if let Some(t) = &mut self.text {
                    let rect = t.layout.borrow().rect;
                    if rect.contains_point(event.pos.x as i32, event.pos.y as i32) {
                        return ToolUpdateResult::StopPropagation;
                    }
                }
                ToolUpdateResult::Unmodified
            }
            MouseEventType::UpdateDrag => {
                // Editing-mode handle drag: rebuild from the BeginDrag
                // snapshot each frame so successive updates don't
                // accumulate. `event.pos` is the image-space delta from
                // BeginDrag (set up by sketch_board).
                if let Some(drag) = &self.handle_drag {
                    if let Some(t) = &mut self.text {
                        t.pos = drag.original_pos;
                        t.text_box_width = drag.original_text_box_width;
                        t.style.annotation_size_factor = drag.original_size_factor;
                        // Restore line_height so move_handle's
                        // height-based scale math sees the pre-drag
                        // box geometry (it drifts each frame as the
                        // factor changes).
                        t.layout.borrow_mut().line_height = drag.original_line_height;
                        let to = drag.anchor + event.pos;
                        t.move_handle(drag.handle, to);
                    }
                    return ToolUpdateResult::RedrawAndStopPropagation;
                }
                self.dragged = Rc::new(RefCell::new(true));
                if event.button == MouseButton::Primary {
                    let global_pos = self.drag_start_pos + event.pos;
                    if let Some(t) = &mut self.text {
                        let layout = t.layout.borrow();
                        let rect = &layout.rect;
                        if rect.contains_point(global_pos.x as i32, global_pos.y as i32) {
                            //calculate text cursor position
                            let mut index = 0;
                            let mut find_index = false;

                            let glyphs = &layout.glyphs;
                            for line in glyphs.iter() {
                                for glyph in line.iter() {
                                    if glyph
                                        .contains_point(global_pos.x as i32, global_pos.y as i32)
                                    {
                                        find_index = true;
                                        if global_pos.x
                                            > glyph.x() as f32 + glyph.width() as f32 / 2.0
                                        {
                                            index += 1;
                                        }
                                        break;
                                    }
                                    index += 1;
                                }

                                let first_ele = line.iter().next().unwrap();
                                if find_index
                                    || global_pos.y <= (first_ele.y() + first_ele.height()) as f32
                                {
                                    break;
                                }
                            }

                            let buffer = &t.text_buffer;
                            let mut cursor_iter = buffer.iter_at_mark(&buffer.get_insert());
                            cursor_iter.set_offset(index);

                            let start_cursor_itr = buffer.iter_at_mark(&buffer.get_insert());
                            buffer.select_range(&start_cursor_itr, &cursor_iter);

                            return ToolUpdateResult::RedrawAndStopPropagation;
                        }
                    }
                    return ToolUpdateResult::StopPropagation;
                }
                ToolUpdateResult::Unmodified
            }
            MouseEventType::EndDrag => {
                // Finish editing-mode handle drag. The Text remains in
                // edit mode; the size/width changes were already applied
                // to `self.text` during UpdateDrag.
                if self.handle_drag.take().is_some() {
                    return ToolUpdateResult::RedrawAndStopPropagation;
                }
                self.dragged = Rc::new(RefCell::new(false));
                if let Some(t) = &mut self.text {
                    let rect = t.layout.borrow().rect;
                    if rect.contains_point(event.pos.x as i32, event.pos.y as i32) {
                        return ToolUpdateResult::StopPropagation;
                    }
                }
                ToolUpdateResult::Unmodified
            }
            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn handle_deactivated(&mut self) -> ToolUpdateResult {
        self.input_enabled = false;
        self.handle_drag = None;
        self.stop_cursor_blink_timer();
        if let Some(t) = &mut self.text {
            t.preedit = None;
            t.editing = false;
            t.im_context = None;
            t.text_buffer
                .select_range(&t.text_buffer.start_iter(), &t.text_buffer.start_iter());
            let result = t.clone_box();
            let edit_id = self.edit_target_id.take();
            self.text = None;
            self.input_enabled = false;
            if let Some(ctx) = &self.im_context {
                ctx.im_context.reset();
            }
            match edit_id {
                Some(id) => ToolUpdateResult::ModifyDrawable(id, result),
                None => ToolUpdateResult::Commit(result),
            }
        } else {
            ToolUpdateResult::Unmodified
        }
    }

    fn active(&self) -> bool {
        self.text.is_some()
    }

    fn handle_undo(&mut self) -> ToolUpdateResult {
        if let Some(t) = &self.text {
            t.text_buffer.undo();
            ToolUpdateResult::Redraw
        } else {
            ToolUpdateResult::Unmodified
        }
    }

    fn handle_redo(&mut self) -> ToolUpdateResult {
        if let Some(t) = &self.text {
            t.text_buffer.redo();
            ToolUpdateResult::Redraw
        } else {
            ToolUpdateResult::Unmodified
        }
    }

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>) {
        self.sender = Some(sender);
    }

    /// Identify the committed drawable that's currently being re-edited
    /// (via double-click). The renderer hides this drawable from the
    /// normal draw loop so the editing copy in `self.text` doesn't
    /// double up with the original.
    fn dragging_drawable_id(&self) -> Option<DrawableId> {
        self.edit_target_id
    }

    fn editing_handles(&self) -> Vec<Handle> {
        self.text
            .as_ref()
            .filter(|t| t.editing)
            .map(|t| t.editing_handles())
            .unwrap_or_default()
    }

    fn editing_body_rect(&self) -> Option<Rect> {
        self.text
            .as_ref()
            .filter(|t| t.editing)
            .map(|t| t.editing_box())
    }

    fn enter_text_edit_mode(&mut self, id: DrawableId, drawable: Box<dyn Drawable>) -> bool {
        // Drop any unfinished new text or in-flight handle drag first
        // so the re-edit starts clean.
        if let Some(t) = self.text.as_mut() {
            t.editing = false;
            t.im_context = None;
        }
        self.handle_drag = None;

        // Type-erased downcast to recover the concrete Text. Bail if
        // the drawable isn't actually a Text (shouldn't happen — only
        // PointerTool's double-click path emits this).
        let Some(text) = drawable.as_any().downcast_ref::<Text>() else {
            return false;
        };
        let mut t = text.clone();
        t.editing = true;
        t.preedit = None;
        t.im_context = self.im_context.clone();
        // Place cursor at end of buffer so subsequent typing extends
        // the text — the default for re-edit.
        let buffer = t.text_buffer.clone();
        buffer.place_cursor(&buffer.end_iter());
        self.style = t.style;
        self.text = Some(t);
        self.edit_target_id = Some(id);
        self.input_enabled = true;
        // Re-edit path mirrors fresh-text creation: start the 250 ms
        // tick timer so the caret blink-phase boundaries actually
        // trigger redraws. Without this, double-clicking an existing
        // text shows a static (non-blinking) caret because the canvas
        // only redraws on user input.
        self.start_cursor_blink_timer();
        true
    }
}
enum ActionScope {
    ForwardChar,
    BackwardChar,
    ForwardLine,
    BackwardLine,
    ForwardWord,
    BackwardWord,
    ForwardLineAndWord,
    BackwardLineAndWord,
    SelectAll,
    BufferStart,
    BufferEnd,
    Left,
    Right,
    Up,
    Down,
    None,
}

enum Action {
    Delete,
    MoveCursor,
    Select,
    MoveOrigin,
    NudgeOrigin,
}

impl TextTool {
    fn handle_text_buffer_action(
        text: &mut Text,
        action: Action,
        action_scope: ActionScope,
    ) -> ToolUpdateResult {
        let text_buffer = &text.text_buffer;
        let mut start_cursor_itr = text_buffer.iter_at_mark(&text_buffer.get_insert());

        match action {
            Action::Delete => {
                let mut end_cursor_itr = start_cursor_itr;

                if let Some((start, end)) = text_buffer.selection_bounds() {
                    start_cursor_itr = start;
                    end_cursor_itr = end;
                } else {
                    match action_scope {
                        ActionScope::ForwardChar => end_cursor_itr.forward_char(),
                        ActionScope::BackwardChar => end_cursor_itr.backward_char(),
                        ActionScope::ForwardWord => end_cursor_itr.forward_word_end(),
                        ActionScope::BackwardWord => end_cursor_itr.backward_word_start(),
                        _ => false, // should normally be whether movement was possible, but it's not used anyway
                    };
                }

                if text_buffer.delete_interactive(&mut start_cursor_itr, &mut end_cursor_itr, true)
                {
                    ToolUpdateResult::RedrawAndStopPropagation
                } else {
                    ToolUpdateResult::StopPropagation
                }
            }
            Action::MoveCursor => {
                let mut cursor_itr = start_cursor_itr;
                let mut start_iter = None;
                let mut end_iter = None;

                let mut has_selection = false;
                if let Some((start, end)) = text_buffer.selection_bounds() {
                    start_iter = Some(start);
                    end_iter = Some(end);
                    has_selection = true;
                }

                match action_scope {
                    ActionScope::ForwardChar => {
                        if has_selection {
                            cursor_itr = end_iter.unwrap();
                            false
                        } else {
                            cursor_itr.forward_char()
                        }
                    }
                    ActionScope::BackwardChar => {
                        if has_selection {
                            cursor_itr = start_iter.unwrap();
                            false
                        } else {
                            cursor_itr.backward_char()
                        }
                    }
                    ActionScope::ForwardLine => cursor_itr.forward_to_line_end(),
                    ActionScope::ForwardWord => cursor_itr.forward_word_end(),
                    ActionScope::BackwardWord => cursor_itr.backward_word_start(),
                    ActionScope::BackwardLine => {
                        if cursor_itr.starts_line() {
                            cursor_itr.backward_line()
                        } else {
                            while !cursor_itr.starts_line() {
                                cursor_itr.backward_char();
                            }
                            false
                        }
                    }
                    ActionScope::BufferEnd => {
                        cursor_itr.forward_to_end();
                        false
                    }
                    ActionScope::BufferStart => {
                        while !cursor_itr.is_start() {
                            cursor_itr.backward_line();
                        }
                        false
                    }
                    ActionScope::ForwardLineAndWord => {
                        if has_selection {
                            cursor_itr = end_iter.unwrap();
                        } else {
                            let content = &text.text_buffer.text(
                                &text.text_buffer.start_iter(),
                                &text.text_buffer.end_iter(),
                                false,
                            );
                            let current_offset = cursor_itr.offset();

                            let mut next_line = 0;
                            let mut offset = 0;

                            let layout = text.layout.borrow();
                            let ranges = &layout.line_ranges;

                            for i in 0..ranges.len() {
                                let line = ranges.get(i).unwrap();

                                let start = content[..line.start].chars().count();
                                let end = content[..line.end].chars().count();

                                if current_offset >= start as i32 && current_offset <= end as i32 {
                                    offset = if i == ranges.len() - 1 {
                                        (end - start) as i32
                                    } else {
                                        let temp = current_offset - start as i32;
                                        let next_start = content
                                            [..ranges.get(i + 1).unwrap().start]
                                            .chars()
                                            .count();
                                        let next_end = content[..ranges.get(i + 1).unwrap().end]
                                            .chars()
                                            .count();

                                        let limit = (next_end - next_start) as i32;
                                        if temp > limit { limit } else { temp }
                                    };

                                    next_line = if i == ranges.len() - 1 {
                                        content[..ranges.get(i).unwrap().start].chars().count()
                                            as i32
                                    } else {
                                        content[..ranges.get(i + 1).unwrap().start].chars().count()
                                            as i32
                                    };
                                    break;
                                }
                            }

                            let move_offset = next_line + offset;

                            cursor_itr.set_offset(move_offset);
                        }

                        false
                    }
                    ActionScope::BackwardLineAndWord => {
                        if has_selection {
                            cursor_itr = start_iter.unwrap();
                        } else {
                            let content = &text.text_buffer.text(
                                &text.text_buffer.start_iter(),
                                &text.text_buffer.end_iter(),
                                false,
                            );
                            let current_offset = cursor_itr.offset();

                            let mut last_line = 0;
                            let mut offset = 0;

                            let layout = text.layout.borrow();
                            let ranges = &layout.line_ranges;

                            for i in 0..ranges.len() {
                                let line = ranges.get(i).unwrap();

                                let start = content[..line.start].chars().count();
                                let end = content[..line.end].chars().count();

                                if current_offset >= start as i32 && current_offset <= end as i32 {
                                    offset = if i == 0 {
                                        0
                                    } else {
                                        let temp = current_offset - start as i32;
                                        let last_start = content
                                            [..ranges.get(i - 1).unwrap().start]
                                            .chars()
                                            .count();
                                        let last_end = content[..ranges.get(i - 1).unwrap().end]
                                            .chars()
                                            .count();

                                        let limit = (last_end - last_start) as i32;
                                        if temp > limit { limit } else { temp }
                                    };

                                    last_line = if i == 0 {
                                        content[..ranges.get(i).unwrap().start].chars().count()
                                            as i32
                                    } else {
                                        content[..ranges.get(i - 1).unwrap().start].chars().count()
                                            as i32
                                    };
                                    break;
                                }
                            }

                            let move_offset = last_line + offset;

                            cursor_itr.set_offset(move_offset);
                        }
                        false
                    }
                    _ => false, // should normally be whether movement was possible, but it's not used anyway
                };

                text_buffer.select_range(&text_buffer.start_iter(), &text_buffer.start_iter());

                text_buffer.place_cursor(&cursor_itr);
                let new_cursor_itr = text_buffer.iter_at_mark(&text_buffer.get_insert());

                if new_cursor_itr != start_cursor_itr || has_selection {
                    ToolUpdateResult::RedrawAndStopPropagation
                } else {
                    ToolUpdateResult::StopPropagation
                }
            }
            Action::Select => {
                let mut start_cursor_itr_new = start_cursor_itr;
                let mut end_cursor_itr = start_cursor_itr;

                if let Some((start, end)) = text_buffer.selection_bounds() {
                    let insert = text_buffer.get_insert();
                    let insert_iter = text_buffer.iter_at_mark(&insert);

                    if insert_iter == start {
                        start_cursor_itr_new = start;
                        end_cursor_itr = end;
                    } else {
                        start_cursor_itr_new = end;
                        end_cursor_itr = start;
                    }
                }

                match action_scope {
                    ActionScope::ForwardChar => {
                        end_cursor_itr.forward_char();
                    }
                    ActionScope::BackwardChar => {
                        end_cursor_itr.backward_char();
                    }
                    ActionScope::ForwardLine => {
                        end_cursor_itr.forward_to_line_end();
                    }
                    ActionScope::BackwardLine => {
                        if end_cursor_itr.starts_line() {
                            end_cursor_itr.backward_line();
                        } else {
                            while !end_cursor_itr.starts_line() {
                                end_cursor_itr.backward_char();
                            }
                        }
                    }
                    ActionScope::ForwardLineAndWord => {
                        let content = &text.text_buffer.text(
                            &text.text_buffer.start_iter(),
                            &text.text_buffer.end_iter(),
                            false,
                        );
                        let current_offset = end_cursor_itr.offset();

                        let mut next_line = 0;
                        let mut offset = 0;

                        let layout = text.layout.borrow();
                        let ranges = &layout.line_ranges;

                        for i in 0..ranges.len() {
                            let line = ranges.get(i).unwrap();
                            let start = content[..line.start].chars().count();
                            let end = content[..line.end].chars().count();

                            if current_offset >= start as i32 && current_offset <= end as i32 {
                                offset = if i == ranges.len() - 1 {
                                    (end - start) as i32
                                } else {
                                    let temp = current_offset - start as i32;
                                    // current_offset - start as i32
                                    let next_start =
                                        content[..ranges.get(i + 1).unwrap().start].chars().count();
                                    let next_end =
                                        content[..ranges.get(i + 1).unwrap().end].chars().count();

                                    let limit = (next_end - next_start) as i32;
                                    if temp > limit { limit } else { temp }
                                };

                                next_line = if i == ranges.len() - 1 {
                                    content[..ranges.get(i).unwrap().start].chars().count() as i32
                                } else {
                                    content[..ranges.get(i + 1).unwrap().start].chars().count()
                                        as i32
                                };
                                break;
                            }
                        }

                        let move_offset = next_line + offset;

                        end_cursor_itr.set_offset(move_offset);
                    }
                    ActionScope::BackwardLineAndWord => {
                        let content = &text.text_buffer.text(
                            &text.text_buffer.start_iter(),
                            &text.text_buffer.end_iter(),
                            false,
                        );
                        let current_offset = end_cursor_itr.offset();

                        let mut last_line = 0;
                        let mut offset = 0;

                        let layout = text.layout.borrow();
                        let ranges = &layout.line_ranges;

                        for i in 0..ranges.len() {
                            let line = ranges.get(i).unwrap();
                            let start = content[..line.start].chars().count();
                            let end = content[..line.end].chars().count();

                            if current_offset >= start as i32 && current_offset <= end as i32 {
                                offset = if i == 0 {
                                    0
                                } else {
                                    let temp = current_offset - start as i32;
                                    let last_start =
                                        content[..ranges.get(i - 1).unwrap().start].chars().count();
                                    let last_end =
                                        content[..ranges.get(i - 1).unwrap().end].chars().count();

                                    let limit = (last_end - last_start) as i32;
                                    if temp > limit { limit } else { temp }
                                };

                                last_line = if i == 0 {
                                    content[..ranges.get(i).unwrap().start].chars().count() as i32
                                } else {
                                    content[..ranges.get(i - 1).unwrap().start].chars().count()
                                        as i32
                                };
                                break;
                            }
                        }

                        let move_offset = last_line + offset;

                        end_cursor_itr.set_offset(move_offset);
                    }
                    ActionScope::ForwardWord => {
                        end_cursor_itr.forward_word_end();
                    }
                    ActionScope::BackwardWord => {
                        end_cursor_itr.backward_word_start();
                    }
                    ActionScope::SelectAll => {
                        start_cursor_itr_new = text_buffer.start_iter();
                        end_cursor_itr = text_buffer.end_iter();
                    }
                    _ => {}
                }
                text_buffer.select_range(&start_cursor_itr_new, &end_cursor_itr);

                ToolUpdateResult::RedrawAndStopPropagation
            }
            Action::MoveOrigin | Action::NudgeOrigin => {
                let length = match action {
                    Action::MoveOrigin => APP_CONFIG.read().text_move_length(),
                    Action::NudgeOrigin => 1.0,
                    _ => 0.0,
                };
                let offset = match action_scope {
                    ActionScope::Left => Vec2D::new(-length, 0.0),
                    ActionScope::Right => Vec2D::new(length, 0.0),
                    ActionScope::Up => Vec2D::new(0.0, -length),
                    ActionScope::Down => Vec2D::new(0.0, length),
                    _ => Vec2D::new(0.0, 0.0),
                };

                if offset.is_zero() {
                    ToolUpdateResult::StopPropagation
                } else {
                    text.pos += offset;
                    ToolUpdateResult::RedrawAndStopPropagation
                }
            }
        }
    }
}
