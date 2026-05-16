use std::ops::{Add, Sub};

use anyhow::Result;
use femtovg::{Paint, Path};

use relm4::{
    Sender,
    gtk::gdk::{Key, ModifierType},
};
use serde_derive::Deserialize;

use crate::{
    math::{Rect, Vec2D, point_to_segment_distance},
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    style::Style,
};

use tensaku_cli::command_line;

use super::{
    Drawable, GLOW_COLOR, Handle, HandleId, Tool, ToolUpdateResult, Tools, bbox_handles,
    bbox_resize, halo_in_image_units,
};

/// Convert per-stroke opacity (`Style::highlighter_opacity`, set by the
/// toolbar slider, range 0.10–1.00) into an alpha byte. Each stroke
/// captures the slider value at draw time, so dragging the slider only
/// affects future strokes — existing ones keep the value they were
/// committed with.
fn opacity_alpha(style: &Style) -> u8 {
    (style.highlighter_opacity.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Block vs Freehand variants. Kept here as a public enum because the
/// Spotlight tool still uses it for its primary-shape preference; the
/// Highlighter is freehand-only and ignores the value, so the
/// `Highlighters::Block` discriminant is effectively dead from the
/// highlighter's perspective.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Highlighters {
    Block = 0,
    Freehand = 1,
}

/// Per-tool highlighter style — picks between the classic freehand
/// drawing path and the text-band snap. Persisted via
/// state.toml; double-tapping the highlighter shortcut cycles.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, serde::Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum HighlighterStyle {
    /// "Smart" highlighter. Detects the text band under the cursor 
    /// and locks the stroke horizontally at that band's center, 
    /// sized to the band's height — covers a line of text cleanly 
    /// with no vertical drift from trackpad jitter.
    #[default]
    TextLocked,
    /// Classic freehand highlighter — stroke follows the pointer in
    /// both x and y, sized by the toolbar's Size slider (XS…XXL),
    /// post-smoothed on release so the curve reads clean.
    Normal,
}

impl HighlighterStyle {
    pub fn next(self) -> Self {
        use HighlighterStyle::*;
        match self {
            TextLocked => Normal,
            Normal => TextLocked,
        }
    }

    pub fn prev(self) -> Self {
        // Only two variants — prev == next.
        self.next()
    }

    /// Short label used in the cycle toast and tooltip wording.
    pub fn display_name(self) -> &'static str {
        use HighlighterStyle::*;
        match self {
            TextLocked => "Text-locked",
            Normal => "Normal",
        }
    }
}

impl From<command_line::Highlighters> for Highlighters {
    fn from(tool: command_line::Highlighters) -> Self {
        match tool {
            command_line::Highlighters::Block => Self::Block,
            command_line::Highlighters::Freehand => Self::Freehand,
        }
    }
}

/// One translucent freehand stroke. Pen-style: continuous polyline
/// whose color and per-stroke opacity are baked in from the tool's
/// style at the moment of commit.
///
/// `first` is absolute, `rest` is offsets-from-`first`. Storing the
/// rest as offsets means `translate` is an O(1) `first += delta`
/// instead of touching every vertex.
#[derive(Clone, Debug)]
pub struct HighlightStroke {
    first: Vec2D,
    rest: Vec<Vec2D>,
    style: Style,
    /// Tracks shift-press state mid-draw so the chained-straight-line
    /// snapping behavior can detect the just-pressed and just-released
    /// transitions. Not used after commit.
    shift_pressed: bool,
    /// When `Some`, the stroke renders at this width instead of the
    /// width derived from `style.size`. Set when the user starts the
    /// stroke inside a detected text band so the highlight matches
    /// the band's measured height instead of the global tool size.
    /// Carried into the committed drawable so selection / resize /
    /// re-render all use the locked width.
    forced_width: Option<f32>,
    /// Highlighter style at commit time. Stored on the stroke so the
    /// layer panel can show "Text-locked Highlight" vs "Normal
    /// Highlight" — `forced_width.is_some()` would be a close proxy
    /// but TextLocked strokes started outside a detected text band
    /// don't get a forced width, so the proxy isn't reliable.
    highlight_style: HighlighterStyle,
}

impl HighlightStroke {
    fn stroke_width(&self) -> f32 {
        self.forced_width.unwrap_or_else(|| {
            self.style
                .size
                .to_highlight_width(self.style.annotation_size_factor)
        })
    }

    /// Color + per-stroke opacity baked into a femtovg `Paint`. Used
    /// for the native stroked path render in `draw` — line width, cap,
    /// and join are set on top of this at stroke time. The "fill"
    /// name is historical; the highlight used to render as a filled
    /// polygon, and most callers think of the paint as the fill color
    /// even though we now stroke with it.
    fn fill_paint(&self) -> Paint {
        Paint::color(femtovg::Color::rgba(
            self.style.color.r,
            self.style.color.g,
            self.style.color.b,
            opacity_alpha(&self.style),
        ))
    }

    /// Absolute polyline points (`first` is absolute; `rest` is
    /// offsets-from-`first`, so we add `first` to recover absolute
    /// positions).
    fn absolute_points(&self) -> Vec<Vec2D> {
        let mut points = Vec::with_capacity(self.rest.len() + 1);
        points.push(self.first);
        for p in &self.rest {
            points.push(self.first + *p);
        }
        points
    }
}

impl Drawable for HighlightStroke {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn kind_label(&self) -> &'static str {
        "Highlight"
    }
    fn icon_name(&self) -> &'static str {
        "highlight-regular"
    }
    fn panel_label_kind(&self) -> String {
        format!("{} Highlight", self.highlight_style.display_name())
    }
    fn highlighter_style(&self) -> Option<HighlighterStyle> {
        Some(self.highlight_style)
    }

    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: femtovg::FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        canvas.save();
        let points = self.absolute_points();
        if points.len() >= 2 {
            // Native GPU stroke with round caps + bevel joins.
            // Round caps protrude past the polyline's endpoints by
            // `stroke_width / 2`, producing the hemispherical ends a
            // physical highlighter leaves on paper — and unlike the
            // old custom inward-eating polygon, this never degenerates
            // to a butt cap when the first/last segment is short
            // (which used to happen on every fresh drag because the
            // user accelerated from rest and the first sample lay
            // only 1–2 px from the click point). Bevel joins keep
            // interior corners flat so a zig-zag stroke doesn't
            // balloon at vertices. The fill_paint() already encodes
            // color + per-stroke opacity from when the stroke was
            // committed.
            let mut path = femtovg::Path::new();
            path.move_to(points[0].x, points[0].y);
            for p in &points[1..] {
                path.line_to(p.x, p.y);
            }
            let mut paint = self.fill_paint();
            paint.set_line_width(self.stroke_width());
            paint.set_line_cap(femtovg::LineCap::Round);
            paint.set_line_join(femtovg::LineJoin::Bevel);
            canvas.stroke_path(&path, &paint);
        }
        canvas.restore();
        Ok(())
    }

    fn bounds(&self) -> Option<Rect> {
        if self.rest.is_empty() {
            return None;
        }
        let mut min = self.first;
        let mut max = self.first;
        for p in &self.rest {
            let abs = self.first + *p;
            min.x = min.x.min(abs.x);
            min.y = min.y.min(abs.y);
            max.x = max.x.max(abs.x);
            max.y = max.y.max(abs.y);
        }
        let stroke = self.stroke_width();
        Some(
            Rect {
                pos: min,
                size: max - min,
            }
            .inflated(stroke / 2.0),
        )
    }

    fn hit_test(&self, point: Vec2D, tolerance: f32) -> bool {
        if self.rest.is_empty() {
            return false;
        }
        let stroke = self.stroke_width();
        let pick = stroke / 2.0 + tolerance;
        let mut prev = self.first;
        for p in &self.rest {
            let cur = self.first + *p;
            if point_to_segment_distance(point, prev, cur) <= pick {
                return true;
            }
            prev = cur;
        }
        false
    }

    fn translate(&mut self, delta: Vec2D) {
        self.first += delta;
    }

    fn handles(&self) -> Vec<Handle> {
        // Standard 8-handle bbox. Provides explicit visual affordance
        // for "this freehand stroke is selected and movable" and a
        // resize path via `move_handle`. Body-drag still works the same.
        self.bounds().map(bbox_handles).unwrap_or_default()
    }

    fn move_handle(&mut self, handle: HandleId, to: Vec2D) {
        // Resize by uniformly scaling all vertices about the pinned
        // corner/edge implied by the dragged handle. Stroke width is
        // intentionally not scaled — the user adjusts that via the Size
        // selector. The math uses the inflated `bounds()` rect on both
        // sides of the transform, so a slight (stroke/2 per side)
        // discrepancy can appear vs. the dragged handle position; it's
        // imperceptible for typical stroke widths.
        let Some(old) = self.bounds() else { return };
        let new = bbox_resize(old, handle, to);
        let scale_x = if old.size.x > f32::EPSILON {
            new.size.x / old.size.x
        } else {
            1.0
        };
        let scale_y = if old.size.y > f32::EPSILON {
            new.size.y / old.size.y
        } else {
            1.0
        };
        let new_first_x = new.pos.x + (self.first.x - old.pos.x) * scale_x;
        let new_first_y = new.pos.y + (self.first.y - old.pos.y) * scale_y;
        self.first = Vec2D::new(new_first_x, new_first_y);
        // `rest` entries are offsets from `first` — scale each axis
        // independently to mirror the bbox transform.
        for p in self.rest.iter_mut() {
            p.x *= scale_x;
            p.y *= scale_y;
        }
    }

    fn set_style(&mut self, style: Style) {
        self.style = style;
    }

    fn style(&self) -> Option<Style> {
        Some(self.style)
    }

    fn tool_type(&self) -> Option<Tools> {
        Some(Tools::Highlighter)
    }

    fn render_glow(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: femtovg::FontId,
        _bounds: (Vec2D, Vec2D),
        device_pixel_ratio: f32,
    ) -> anyhow::Result<()> {
        let Some(b) = self.bounds() else {
            return Ok(());
        };
        let halo = halo_in_image_units(canvas, device_pixel_ratio);
        let inflate = halo / 2.0;
        canvas.save();
        let mut path = Path::new();
        path.rounded_rect(
            b.pos.x - inflate,
            b.pos.y - inflate,
            b.size.x + inflate * 2.0,
            b.size.y + inflate * 2.0,
            6.0,
        );
        let mut paint = Paint::color(GLOW_COLOR);
        paint.set_line_width(halo);
        canvas.stroke_path(&path, &paint);
        canvas.restore();
        Ok(())
    }
}

#[derive(Default, Clone, Debug)]
pub struct HighlightTool {
    stroke: Option<HighlightStroke>,
    style: Style,
    input_enabled: bool,
    sender: Option<Sender<SketchBoardInput>>,
    /// The text band, if any, the active stroke is locked to. Set on
    /// BeginDrag (TextLocked mode only) when the cursor lands inside
    /// a detected band; the in-flight stroke then renders at the
    /// band's height and snaps every UpdateDrag to the band's center
    /// y. Cleared on EndDrag.
    locked_band: Option<crate::text_bands::TextBand>,
    /// Absolute image-space position of the active stroke's BeginDrag
    /// event. Kept separately from `stroke.first` because TextLocked
    /// mode snaps `stroke.first.y` to the locked anchor (band center,
    /// or the click y when no band) before pushing any rest entries
    /// — which makes `stroke.first.y` no longer equal to the click's
    /// y. `event.pos` in UpdateDrag is delta-from-BeginDrag, so the
    /// absolute-x calc still needs the original click x as the
    /// anchor; that's what `drag_anchor` stores. None when no drag is
    /// in flight.
    drag_anchor: Option<Vec2D>,
    /// Active style: TextLocked (smart highlighter) vs Normal 
    /// (classic freehand). Persisted via state.toml; the toolbar 
    /// dropdown and double-tap-shortcut cycle both drive
    /// `set_highlighter_style` to update this. Branches in 
    /// `handle_mouse_event` dispatch on it at BeginDrag, snapshotted 
    /// into `drag_style` so a mid-drag toolbar flip doesn't change 
    /// behavior of the in-flight stroke.
    highlight_style: HighlighterStyle,
    /// Snapshot of `highlight_style` taken at BeginDrag. UpdateDrag /
    /// EndDrag read this instead of the live `highlight_style` so a
    /// mid-drag style change from the toolbar doesn't reshape the
    /// stroke in flight. None when no drag is in flight.
    drag_style: Option<HighlighterStyle>,
}

/// Minimum total drag motion (image px, end-to-end Euclidean) for a
/// highlight stroke to commit on release. Below this the user
/// effectively just clicked — common while positioning the cursor
/// before the "real" drag — and committing the resulting tiny stroke
/// would scatter half-circle ink blobs across the canvas (one per
/// failed positioning attempt). Real highlight intent always involves
/// a deliberate horizontal motion well past this floor.
const MIN_COMMIT_LENGTH_PX: f32 = 4.0;

impl Tool for HighlightTool {
    fn input_enabled(&self) -> bool {
        self.input_enabled
    }

    fn set_input_enabled(&mut self, value: bool) {
        self.input_enabled = value;
    }

    fn get_tool_type(&self) -> super::Tools {
        Tools::Highlighter
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        let shift_pressed = event.modifier.intersects(ModifierType::SHIFT_MASK);
        match event.type_ {
            MouseEventType::BeginDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }
                // Snapshot the current style so a mid-drag flip via
                // the toolbar doesn't reshape this stroke.
                self.drag_style = Some(self.highlight_style);
                self.drag_anchor = Some(event.pos);
                match self.highlight_style {
                    HighlighterStyle::TextLocked => {
                        // Strict horizontal lock. Anchor y =
                        // band.center_y if a band is detected at the
                        // click, otherwise the click's y. `stroke.first`
                        // is placed at (click.x, anchor.y) so the very
                        // first vertex sits on the locked line — no
                        // vertical lump at stroke start.
                        let band = crate::text_bands::detect_local_band(
                            event.pos.x,
                            event.pos.y,
                        );
                        let anchor_y = band
                            .map(|b| b.center_y())
                            .unwrap_or(event.pos.y);
                        self.locked_band = band;
                        let pad = band
                            .map(|b| {
                                2.0 * b.height()
                                    * crate::text_bands::BAND_PAD_PERCENT_PER_SIDE
                            })
                            .unwrap_or(0.0);
                        self.stroke = Some(HighlightStroke {
                            first: Vec2D::new(event.pos.x, anchor_y),
                            rest: Vec::new(),
                            style: self.style,
                            shift_pressed,
                            forced_width: band.map(|b| b.height() + pad),
                            highlight_style: HighlighterStyle::TextLocked,
                        });
                    }
                    HighlighterStyle::Normal => {
                        // Classic freehand. No band detection, no axis
                        // lock — the polyline follows the pointer in
                        // both x and y. Width comes from the toolbar's
                        // size slider (Style::size → to_highlight_width).
                        // Post-smoothing on EndDrag cleans up trackpad
                        // jitter.
                        self.locked_band = None;
                        self.stroke = Some(HighlightStroke {
                            first: event.pos,
                            rest: Vec::new(),
                            style: self.style,
                            shift_pressed,
                            forced_width: None,
                            highlight_style: HighlighterStyle::Normal,
                        });
                    }
                }
                ToolUpdateResult::Redraw
            }
            MouseEventType::UpdateDrag | MouseEventType::EndDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }
                let Some(stroke) = self.stroke.as_mut() else {
                    return ToolUpdateResult::Unmodified;
                };
                if event.pos == Vec2D::zero() {
                    return ToolUpdateResult::Unmodified;
                }
                let Some(drag_anchor) = self.drag_anchor else {
                    return ToolUpdateResult::Unmodified;
                };

                // The rest entry — what gets pushed onto the polyline
                // as an offset-from-stroke.first — depends on the
                // mode snapshotted at BeginDrag.
                //   * TextLocked: x follows the pointer, y locked to
                //     the anchor (zero offset from stroke.first.y),
                //     so the recorded rest entry's y is always 0.
                //   * Normal: rest entry is the raw delta from
                //     BeginDrag (event.pos as reported), so the
                //     polyline tracks pointer in both axes.
                let rest_entry = match self.drag_style {
                    Some(HighlighterStyle::TextLocked) => {
                        let abs_x = drag_anchor.x + event.pos.x;
                        Vec2D::new(abs_x - stroke.first.x, 0.0)
                    }
                    _ => event.pos,
                };

                if shift_pressed {
                    // Shift kept for the existing 15°-angle-snap
                    // behavior. In TextLocked mode the y is locked
                    // so the snap collapses to ±x (effectively a
                    // no-op); in Normal mode it works as the classic
                    // angle quantization for chained straight runs.
                    if stroke.shift_pressed && !stroke.rest.is_empty() {
                        stroke.rest.pop();
                    }
                    let last = stroke.rest.last().copied().unwrap_or(Vec2D::zero());
                    let snapped =
                        rest_entry.sub(last).snapped_vector_15deg().add(last);
                    stroke.rest.push(snapped);
                } else {
                    let last = stroke.rest.last().copied().unwrap_or(Vec2D::zero());
                    if (rest_entry - last).norm() >= 1.0 {
                        stroke.rest.push(rest_entry);
                    }
                }
                stroke.shift_pressed = shift_pressed;

                if event.type_ == MouseEventType::UpdateDrag {
                    return ToolUpdateResult::Redraw;
                }
                // Reject strokes that look like accidental clicks /
                // tap-then-tiny-wiggle: end-to-end Euclidean distance
                // below `MIN_COMMIT_LENGTH_PX`. With round caps even a
                // 1-px stroke renders as a visible half_w-radius
                // half-circle, so the canvas otherwise accumulates ink
                // blobs every time the user repositions before the
                // real drag.
                let end_offset = stroke.rest.last().copied().unwrap_or(Vec2D::zero());
                if end_offset.norm() < MIN_COMMIT_LENGTH_PX {
                    self.stroke = None;
                    self.locked_band = None;
                    self.drag_anchor = None;
                    self.drag_style = None;
                    crate::text_bands::clear_local_band_cache();
                    return ToolUpdateResult::Redraw;
                }
                // Normal mode: post-stroke smoothing via the same
                // RDP+Chaikin pipeline the brush tool uses. Hard-coded
                // smoothing level — picked from the brush's 0..=6
                // scale where 4 sits in the "noticeable smoothing
                // with light RDP simplification" band; enough to
                // clean up trackpad jitter without drifting the curve
                // far enough from input to mis-cover what the user
                // intended to highlight. Skipped when the user was
                // Shift-drawing (segments already angle-snapped on
                // purpose) and skipped for TextLocked mode (polyline
                // is already a perfect horizontal line — smoothing
                // would just resample the same line).
                if matches!(self.drag_style, Some(HighlighterStyle::Normal))
                    && !stroke.shift_pressed
                    && stroke.rest.len() >= 2
                {
                    const HIGHLIGHT_SMOOTH_LEVEL: usize = 4;
                    let mut absolute = Vec::with_capacity(stroke.rest.len() + 1);
                    absolute.push(stroke.first);
                    for p in &stroke.rest {
                        absolute.push(stroke.first + *p);
                    }
                    let smoothed = crate::tools::brush::smooth_polyline(
                        &absolute,
                        HIGHLIGHT_SMOOTH_LEVEL,
                    );
                    if let Some((&new_first, new_rest_abs)) = smoothed.split_first() {
                        stroke.first = new_first;
                        stroke.rest =
                            new_rest_abs.iter().map(|p| *p - new_first).collect();
                    }
                }
                let committed: Box<dyn Drawable> = Box::new(stroke.clone());
                self.stroke = None;
                self.locked_band = None;
                self.drag_anchor = None;
                self.drag_style = None;
                // Drop the band-detection hysteresis cache so the
                // next hover after release re-evaluates from scratch.
                // Without this, a release that happened far from the
                // last cached anchor would leave the cursor showing
                // the stale band on the next motion event.
                crate::text_bands::clear_local_band_cache();
                ToolUpdateResult::Commit(committed)
            }
            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn handle_key_event(&mut self, event: crate::sketch_board::KeyEventMsg) -> ToolUpdateResult {
        if event.key == Key::Escape && self.stroke.is_some() {
            self.stroke = None;
            return ToolUpdateResult::Redraw;
        }
        ToolUpdateResult::Unmodified
    }

    fn handle_key_release_event(
        &mut self,
        event: crate::sketch_board::KeyEventMsg,
    ) -> ToolUpdateResult {
        // Releasing Shift mid-stroke either drops or duplicates the
        // most-recent point so the user can chain multiple aligned
        // segments without having to nudge the cursor between them.
        if (event.key == Key::Shift_L || event.key == Key::Shift_R)
            && let Some(stroke) = &mut self.stroke
            && stroke.rest.len() >= 2
        {
            let n = stroke.rest.len();
            let last = stroke.rest[n - 1];
            let second_last = stroke.rest[n - 2];
            if last == second_last {
                stroke.rest.pop();
            } else {
                stroke.rest.push(last);
            }
            return ToolUpdateResult::Redraw;
        }
        ToolUpdateResult::Unmodified
    }

    fn handle_style_event(&mut self, style: Style) -> ToolUpdateResult {
        self.style = style;
        ToolUpdateResult::Unmodified
    }

    fn get_drawable(&self) -> Option<&dyn Drawable> {
        self.stroke.as_ref().map(|s| s as &dyn Drawable)
    }

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>) {
        self.sender = Some(sender);
    }

    fn locked_text_band(&self) -> Option<crate::text_bands::TextBand> {
        self.locked_band
    }

    fn set_highlighter_style(&mut self, style: HighlighterStyle) {
        self.highlight_style = style;
    }

    fn highlighter_style(&self) -> Option<HighlighterStyle> {
        Some(self.highlight_style)
    }
}
