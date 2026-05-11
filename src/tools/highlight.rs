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

use satty_cli::command_line;

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
}

impl HighlightStroke {
    fn stroke_width(&self) -> f32 {
        self.style
            .size
            .to_highlight_width(self.style.annotation_size_factor)
    }

    /// Solid fill paint (no stroke params). The highlight is rendered
    /// as a filled polygon — see `build_highlight_path` — so the only
    /// thing the paint needs to encode is the color + per-stroke
    /// alpha that was baked in at commit time.
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

/// Radius (image-space pixels) of the rounded outer corners at each
/// end of the highlight stroke. Small enough not to noticeably eat
/// into the stroke width (the rounded corners reduce the effective
/// width at the very tips by ~1px), but visible enough that the ends
/// don't read as harshly chopped-off butt caps. Independent of the
/// stroke width on purpose: the user asked for a fixed-size soft
/// corner, not a width-scaled one.
const HIGHLIGHT_CAP_RADIUS: f32 = 4.0;

/// One Chaikin smoothing pass (corner-cutting subdivision). Each
/// interior segment `p[i] → p[i+1]` is replaced with two interpolated
/// points at 25 % and 75 % along the segment, while the polyline's
/// two endpoints are preserved verbatim so the stroke still starts
/// and ends where the user lifted the mouse. Two passes is enough to
/// take the visible jitter out of a hand-drawn polyline without
/// drifting the curve away from where the user drew it.
fn chaikin_smooth(points: &[Vec2D], iterations: usize) -> Vec<Vec2D> {
    if points.len() < 3 || iterations == 0 {
        return points.to_vec();
    }
    let mut current = points.to_vec();
    for _ in 0..iterations {
        let mut next = Vec::with_capacity(current.len() * 2);
        next.push(current[0]);
        for i in 0..current.len() - 1 {
            let a = current[i];
            let b = current[i + 1];
            next.push(a * 0.75 + b * 0.25);
            next.push(a * 0.25 + b * 0.75);
        }
        next.push(*current.last().unwrap());
        current = next;
    }
    current
}

/// Append a discretized arc to `poly`, starting from the polygon's
/// current end (which the caller must have already pushed and which
/// is assumed to equal `from`) and ending at `to`. The arc center
/// + radius pin the geometry; `steps` controls smoothness (~6 is
/// plenty for the 4-px-radius caps used here — even at 8x zoom
/// the segments are well under a pixel apart).
fn add_arc_segments(
    poly: &mut Vec<Vec2D>,
    center: Vec2D,
    r: f32,
    from: Vec2D,
    to: Vec2D,
    steps: usize,
) {
    let from_off = from - center;
    let to_off = to - center;
    let angle_from = from_off.y.atan2(from_off.x);
    let angle_to = to_off.y.atan2(to_off.x);
    // Pick the shortest-signed sweep so we always traverse the
    // quarter arc the polygon expects (the polygon's perimeter
    // turns by exactly ±π/2 at each cap corner; the full π/2
    // shows up after the modular normalization below).
    let mut delta = angle_to - angle_from;
    while delta > std::f32::consts::PI {
        delta -= 2.0 * std::f32::consts::PI;
    }
    while delta <= -std::f32::consts::PI {
        delta += 2.0 * std::f32::consts::PI;
    }
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        let angle = angle_from + delta * t;
        poly.push(Vec2D::new(
            center.x + r * angle.cos(),
            center.y + r * angle.sin(),
        ));
    }
}

/// Build a closed filled polygon for a highlight stroke: offset the
/// polyline by ±width/2 perpendicular to each segment, bevel-join
/// interior vertices, and round the two endpoint corners with
/// `cap_radius` so the ends read as soft caps instead of perfectly
/// flat butt-cap rectangles.
///
/// The rounded corners CUT INWARD into the polygon — the polyline's
/// endpoints stay at the user-drawn positions and the stroke width
/// is *not* widened to accommodate the rounding. This is what
/// "rounded ends without adding extra width" means in practice: the
/// effective width tapers slightly (by ~1 px at the very tip for a
/// 4 px radius) over the last few pixels of each end.
fn build_highlight_path(points: &[Vec2D], width: f32, cap_radius: f32) -> Option<Path> {
    if points.len() < 2 {
        return None;
    }
    let half_w = width / 2.0;

    // Drop coincident consecutive points — they produce zero-length
    // segments and a NaN tangent, which crashes the arc math below.
    let mut clean: Vec<Vec2D> = vec![points[0]];
    for &p in &points[1..] {
        if (p - *clean.last().unwrap()).norm() >= 0.5 {
            clean.push(p);
        }
    }
    if clean.len() < 2 {
        return None;
    }

    let n_segs = clean.len() - 1;
    let mut tn: Vec<(Vec2D, Vec2D)> = Vec::with_capacity(n_segs);
    for i in 0..n_segs {
        let d = clean[i + 1] - clean[i];
        let len = d.norm();
        let t = Vec2D::new(d.x / len, d.y / len);
        // CCW-90 perpendicular in math; in canvas y-down this lands
        // on the "right" of the tangent (visually).
        let n = Vec2D::new(-t.y, t.x);
        tn.push((t, n));
    }

    // Clamp the cap radius so the rounding can't (a) exceed half the
    // stroke width — the cap edge would invert — nor (b) eat past
    // the inner end of the first/last segment, which would push the
    // shortened endpoint past the other end of the segment.
    let first_seg_len = (clean[1] - clean[0]).norm();
    let last_seg_len = (clean[clean.len() - 1] - clean[clean.len() - 2]).norm();
    let r = cap_radius
        .min(half_w)
        .min(first_seg_len / 2.0)
        .min(last_seg_len / 2.0)
        .max(0.0);

    let p_first = clean[0];
    let (t_first, n_first) = tn[0];
    let p_last = clean[clean.len() - 1];
    let (t_last, n_last) = tn[n_segs - 1];

    let arc_steps = 6;
    let mut poly: Vec<Vec2D> = Vec::new();

    // -n side, traversed start → end. The polygon's first point
    // sits just past the start-cap rounded corner (r along +t_first
    // from p_first - n_first * half_w).
    poly.push(p_first - n_first * half_w + t_first * r);

    for i in 0..n_segs {
        let n = tn[i].1;
        let p_seg_end = if i == n_segs - 1 {
            // Last segment: pull the -n side endpoint inward by r so
            // the rounded end-cap corner has room.
            clean[i + 1] - n * half_w - tn[i].0 * r
        } else {
            clean[i + 1] - n * half_w
        };
        poly.push(p_seg_end);

        if i + 1 < n_segs {
            // Bevel join: straight line across to the next segment's
            // -n offset start at the shared interior vertex.
            let n_next = tn[i + 1].1;
            poly.push(clean[i + 1] - n_next * half_w);
        }
    }

    // Rounded corner at end on -n side
    let arc1_center = p_last - n_last * (half_w - r) - t_last * r;
    let arc1_from = p_last - n_last * half_w - t_last * r;
    let arc1_to = p_last - n_last * (half_w - r);
    add_arc_segments(&mut poly, arc1_center, r, arc1_from, arc1_to, arc_steps);

    // End cap edge (straight line across the cap between the two
    // rounded corners; degenerates to a single point when r == half_w)
    let arc2_from = p_last + n_last * (half_w - r);
    poly.push(arc2_from);

    // Rounded corner at end on +n side
    let arc2_center = p_last + n_last * (half_w - r) - t_last * r;
    let arc2_to = p_last + n_last * half_w - t_last * r;
    add_arc_segments(&mut poly, arc2_center, r, arc2_from, arc2_to, arc_steps);

    // +n side, traversed end → start (reverse direction)
    for i in (0..n_segs).rev() {
        let n = tn[i].1;
        let p_seg_start = if i == 0 {
            clean[i] + n * half_w + tn[i].0 * r
        } else {
            clean[i] + n * half_w
        };
        poly.push(p_seg_start);

        if i > 0 {
            let n_prev = tn[i - 1].1;
            poly.push(clean[i] + n_prev * half_w);
        }
    }

    // Rounded corner at start on +n side
    let arc3_center = p_first + n_first * (half_w - r) + t_first * r;
    let arc3_from = p_first + n_first * half_w + t_first * r;
    let arc3_to = p_first + n_first * (half_w - r);
    add_arc_segments(&mut poly, arc3_center, r, arc3_from, arc3_to, arc_steps);

    // Start cap edge
    let arc4_from = p_first - n_first * (half_w - r);
    poly.push(arc4_from);

    // Rounded corner at start on -n side (closes the loop)
    let arc4_center = p_first - n_first * (half_w - r) + t_first * r;
    let arc4_to = p_first - n_first * half_w + t_first * r;
    add_arc_segments(&mut poly, arc4_center, r, arc4_from, arc4_to, arc_steps);

    let mut path = Path::new();
    path.move_to(poly[0].x, poly[0].y);
    for p in &poly[1..] {
        path.line_to(p.x, p.y);
    }
    path.close();
    Some(path)
}

impl Drawable for HighlightStroke {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: femtovg::FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        canvas.save();
        let points = self.absolute_points();
        if let Some(path) =
            build_highlight_path(&points, self.stroke_width(), HIGHLIGHT_CAP_RADIUS)
        {
            canvas.fill_path(&path, &self.fill_paint());
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
}

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
                // Pen-style: every drag starts a fresh freehand stroke.
                // No more block/freehand toggle — the block highlighter
                // mode lives on in the Spotlight tool, which kept both
                // shape variants for its own use case.
                self.stroke = Some(HighlightStroke {
                    first: event.pos,
                    rest: Vec::new(),
                    style: self.style,
                    shift_pressed,
                });
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

                // Shift behavior carried over from the previous freehand
                // implementation: pressing Shift snaps the in-flight
                // segment to a 15° increment, and chained Shift presses
                // build a polyline of straight aligned runs.
                if shift_pressed {
                    if stroke.shift_pressed && !stroke.rest.is_empty() {
                        stroke.rest.pop();
                    }
                    let last = stroke.rest.last().copied().unwrap_or(Vec2D::zero());
                    let snapped = event.pos.sub(last).snapped_vector_15deg().add(last);
                    stroke.rest.push(snapped);
                } else {
                    // Drop micro-segments — when EndDrag fires within a
                    // pixel of the previous UpdateDrag, that tiny final
                    // segment runs at an arbitrary angle and the line's
                    // cap renders as a perpendicular block sticking off
                    // the stroke. Skipping the duplicate keeps the end
                    // clean.
                    let last = stroke.rest.last().copied().unwrap_or(Vec2D::zero());
                    if (event.pos - last).norm() >= 1.0 {
                        stroke.rest.push(event.pos);
                    }
                }
                stroke.shift_pressed = shift_pressed;

                if event.type_ == MouseEventType::UpdateDrag {
                    return ToolUpdateResult::Redraw;
                }
                // On release, smooth the raw mouse polyline with two
                // Chaikin passes — enough to take the visible jitter
                // off a fast hand-drawn arc without drifting the
                // curve away from where the user actually drew it.
                // Skip smoothing if the user was Shift-drawing (every
                // segment was already angle-snapped on purpose and
                // smoothing would un-snap the corners).
                if !stroke.shift_pressed && stroke.rest.len() >= 2 {
                    let mut absolute = Vec::with_capacity(stroke.rest.len() + 1);
                    absolute.push(stroke.first);
                    for p in &stroke.rest {
                        absolute.push(stroke.first + *p);
                    }
                    let smoothed = chaikin_smooth(&absolute, 2);
                    if let Some((&new_first, new_rest_abs)) = smoothed.split_first() {
                        stroke.first = new_first;
                        stroke.rest = new_rest_abs.iter().map(|p| *p - new_first).collect();
                    }
                }
                let committed: Box<dyn Drawable> = Box::new(stroke.clone());
                self.stroke = None;
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
}
