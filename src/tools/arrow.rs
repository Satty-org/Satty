use anyhow::Result;
use femtovg::{FontId, LineCap, LineJoin, Paint, Path};
use relm4::{
    Sender,
    gtk::gdk::{Key, ModifierType},
};
use serde_derive::Deserialize;

use crate::{
    math::{Angle, Rect, Vec2D, point_to_segment_distance},
    sketch_board::{KeyEventMsg, MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    style::Style,
};

use super::{
    Drawable, DrawableClone, GLOW_COLOR, Handle, halo_in_image_units,
    HandleId, Tool, ToolUpdateResult, Tools,
};

/// Arrow geometry variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArrowStyle {
    /// Solid filled arrow with a tapered tail (point at start, widening to the
    /// arrowhead) and a triangular head. The default style.
    #[default]
    Standard,
    /// Thin stroked shaft with a filled triangular head.
    Fancy,
    /// Quadratic Bezier curve with a single filled head at the end.
    Curved,
    /// Quadratic Bezier curve with filled heads at both ends.
    Double,
}

impl ArrowStyle {
    pub fn next(self) -> Self {
        use ArrowStyle::*;
        match self {
            Standard => Fancy,
            Fancy => Curved,
            Curved => Double,
            Double => Standard,
        }
    }

    pub fn prev(self) -> Self {
        use ArrowStyle::*;
        match self {
            Standard => Double,
            Fancy => Standard,
            Curved => Fancy,
            Double => Curved,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Arrow {
    start: Vec2D,
    end: Option<Vec2D>,
    style: Style,
    arrow_style: ArrowStyle,
    /// User-overridden Bezier control point for curved/double arrows. `None`
    /// means "compute the default perpendicular-offset control point."
    curve_control: Option<Vec2D>,
}

#[derive(Default)]
pub struct ArrowTool {
    arrow: Option<Arrow>,
    style: Style,
    arrow_style: ArrowStyle,
    input_enabled: bool,
    sender: Option<Sender<SketchBoardInput>>,
}

impl Tool for ArrowTool {
    fn input_enabled(&self) -> bool {
        self.input_enabled
    }

    fn set_input_enabled(&mut self, value: bool) {
        self.input_enabled = value;
    }

    fn get_tool_type(&self) -> super::Tools {
        Tools::Arrow
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        match event.type_ {
            MouseEventType::BeginDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }
                self.arrow = Some(Arrow {
                    start: event.pos,
                    end: None,
                    style: self.style,
                    arrow_style: self.arrow_style,
                    curve_control: None,
                });
                ToolUpdateResult::Redraw
            }
            MouseEventType::EndDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }
                if let Some(a) = &mut self.arrow {
                    if event.pos == Vec2D::zero() {
                        self.arrow = None;
                        ToolUpdateResult::Redraw
                    } else {
                        if event.modifier.intersects(ModifierType::SHIFT_MASK) {
                            a.end = Some(a.start + event.pos.snapped_vector_15deg());
                        } else {
                            a.end = Some(a.start + event.pos);
                        }
                        let result = a.clone_box();
                        self.arrow = None;
                        ToolUpdateResult::Commit(result)
                    }
                } else {
                    ToolUpdateResult::Unmodified
                }
            }
            MouseEventType::UpdateDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }
                if let Some(a) = &mut self.arrow {
                    if event.pos == Vec2D::zero() {
                        return ToolUpdateResult::Unmodified;
                    }
                    if event.modifier.intersects(ModifierType::SHIFT_MASK) {
                        a.end = Some(a.start + event.pos.snapped_vector_15deg());
                    } else {
                        a.end = Some(a.start + event.pos);
                    }
                    ToolUpdateResult::Redraw
                } else {
                    ToolUpdateResult::Unmodified
                }
            }
            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn handle_key_event(&mut self, event: KeyEventMsg) -> ToolUpdateResult {
        if event.key == Key::Escape && self.arrow.is_some() {
            self.arrow = None;
            return ToolUpdateResult::Redraw;
        }
        ToolUpdateResult::Unmodified
    }

    fn set_arrow_style(&mut self, style: ArrowStyle) {
        self.arrow_style = style;
        if let Some(a) = self.arrow.as_mut() {
            a.arrow_style = style;
        }
    }

    fn get_drawable(&self) -> Option<&dyn Drawable> {
        match &self.arrow {
            Some(d) => Some(d),
            None => None,
        }
    }

    fn handle_style_event(&mut self, style: Style) -> ToolUpdateResult {
        self.style = style;
        ToolUpdateResult::Unmodified
    }

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>) {
        self.sender = Some(sender);
    }
}

/// Apex angle of the Standard arrowhead, in degrees. ~53° gives an isoceles
/// triangle whose length along the shaft equals its full base width.
const HEAD_FULL_ANGLE_DEG: f32 = 53.13;
/// Apex angle of the open V-tip used by Curved/Double arrows (path-space).
/// the standard V-tips render at ~85° visible apex; the round line-join
/// narrows the visible angle vs the path angle slightly. Empirically tuned
/// to 90° to minimize per-pixel diff against the reference across all sizes.
const CURVED_HEAD_FULL_ANGLE_DEG: f32 = 90.0;
/// Forward slant of the back-of-head edge for Standard, expressed as a
/// fraction of head_length. 0.0 = perpendicular cut (sharp shoulder); larger
/// values rake the back of the head forward toward the tip.
const STANDARD_SHOULDER_RATIO: f32 = 0.05;
/// Forward slant for Fancy. Zero means the body shoulder sits at the same
/// column as the head's outer corner — matching the reference where
/// body and head meet via a vertical step plus a swept-back ear (rather
/// than a slanted forward shoulder).
const FANCY_SHOULDER_RATIO: f32 = 0.0;
/// Fancy wing: how far behind the head's perpendicular base the wing tip
/// sits, as a fraction of head_length. Matches convention's per-size
/// geometric ratios (averaged ~0.213 across sizes), with a small bump for
/// anti-aliased edge coverage.
const FANCY_WING_BACK_RATIO: f32 = 0.22;
/// Fancy wing: how much higher the wing tip sits than the head's plain
/// outer corner, expressed as a fraction of `head_half_height`. Matches
/// the standard per-size ratios (averaged ~0.212), with a small bump for
/// AA coverage. Pairing this with `FANCY_WING_BACK_RATIO` at the same
/// value keeps the wing's front-edge slope equal to the head triangle's
/// natural angle (head_half_height / head_length).
const FANCY_WING_HEIGHT_RATIO: f32 = 0.22;
/// Curvature of curved/double arrows as a fraction of the chord length.
const CURVE_AMOUNT: f32 = 0.25;

impl Arrow {
    /// Length of the arrowhead along the shaft (tip → back of head triangle).
    /// Fancy uses a slightly longer head than Standard (calibrated separately
    /// against the design reference).
    fn head_length(&self) -> f32 {
        match self.arrow_style {
            ArrowStyle::Fancy => self
                .style
                .size
                .to_arrow_fancy_head_length(self.style.annotation_size_factor),
            _ => self
                .style
                .size
                .to_arrow_head_length(self.style.annotation_size_factor),
        }
    }

    /// Half of the head's full perpendicular height at its base. Standard
    /// and Fancy each store their own per-size table — the standard heads
    /// don't match a single apex angle (Standard uses ~49°, Fancy ranges
    /// 51-53° per size). Curved/Double fall back to the 53° apex constant
    /// for the open V-tip path.
    fn head_half_height(&self) -> f32 {
        match self.arrow_style {
            ArrowStyle::Fancy => {
                self.style
                    .size
                    .to_arrow_fancy_head_full_height(self.style.annotation_size_factor)
                    * 0.5
            }
            ArrowStyle::Standard => {
                self.style
                    .size
                    .to_arrow_head_full_height(self.style.annotation_size_factor)
                    * 0.5
            }
            _ => {
                let half_angle = Angle::from_degrees(HEAD_FULL_ANGLE_DEG) * 0.5;
                self.head_length() * (half_angle.sin() / half_angle.cos())
            }
        }
    }

    /// Visible body width at the head intersection. Standard and Fancy
    /// pull from separate calibration tables in `style.rs`; Fancy is wider
    /// so the body reads as a long continuous taper into the swept-back
    /// head, matching the design reference.
    fn body_max_width(&self) -> f32 {
        match self.arrow_style {
            ArrowStyle::Fancy => self
                .style
                .size
                .to_arrow_fancy_tail_width(self.style.annotation_size_factor),
            _ => self
                .style
                .size
                .to_arrow_tail_width(self.style.annotation_size_factor),
        }
    }

    /// Visible thickness of the back of the tail. For Standard, this is the
    /// rounded-cap diameter (the body tapers to a sharp point in the path
    /// and the rounded-outline stroke widens that point into a half-circle
    /// of this diameter). For Fancy, this is the flat back-edge thickness —
    /// the path closes with a vertical edge of this height instead of a
    /// single back point, matching the standard fancy tails.
    fn body_back_width(&self) -> f32 {
        match self.arrow_style {
            ArrowStyle::Fancy => self
                .style
                .size
                .to_arrow_fancy_tail_back_width(self.style.annotation_size_factor),
            _ => self
                .style
                .size
                .to_arrow_tail_back_width(self.style.annotation_size_factor),
        }
    }

    fn shaft_width(&self) -> f32 {
        match self.arrow_style {
            ArrowStyle::Curved | ArrowStyle::Double => self
                .style
                .size
                .to_arrow_curved_shaft_width(self.style.annotation_size_factor),
            _ => self
                .style
                .size
                .to_line_width(self.style.annotation_size_factor),
        }
    }

    /// Control point for curved/double arrows. Uses the user-overridden value
    /// if set (via the middle handle), otherwise the default perpendicular-
    /// offset point at `CURVE_AMOUNT * length` from the chord midpoint.
    fn bezier_control(&self, end: Vec2D) -> Option<Vec2D> {
        if let Some(c) = self.curve_control {
            return Some(c);
        }
        let chord = end - self.start;
        let len = chord.norm();
        if len < 1.0 {
            return None;
        }
        let midpoint = (self.start + end) * 0.5;
        // Perpendicular: rotate (dx, dy) by +90° → (-dy, dx).
        let perp = Vec2D::new(-chord.y, chord.x) * (1.0 / len);
        Some(midpoint + perp * (len * CURVE_AMOUNT))
    }

    /// Sample N+1 points along the quadratic Bezier (start → control → end).
    fn bezier_sample(&self, end: Vec2D, control: Vec2D, n: usize) -> Vec<Vec2D> {
        (0..=n)
            .map(|i| {
                let t = i as f32 / n as f32;
                let one_minus_t = 1.0 - t;
                self.start * (one_minus_t * one_minus_t)
                    + control * (2.0 * one_minus_t * t)
                    + end * (t * t)
            })
            .collect()
    }

    /// Build a solid-fill arrow path in arrow-local coords (start at origin,
    /// tip at (length, 0)).
    ///
    /// - `shoulder_ratio`: forward slant of the body shoulder corner, as a
    ///   fraction of head_length.
    /// - `wing_back_ratio` / `wing_height_ratio`: when both > 0, the head's
    ///   outer corner is *extended* backward by `wing_back_ratio × head_length`
    ///   and outward by `wing_height_ratio × head_full_height`, producing a
    ///   single swept-back wing tip per side (no kink). When 0/0, the head
    ///   is a plain isoceles triangle (Standard).
    /// - `stroke_compensation`: inset for the body so the rounded outline
    ///   stroke (Standard) widens it back to `body_max_width`. 0 for Fancy.
    /// - `back_half_width`: 0 → single back point (Standard, widened into
    ///   a rounded cap by the stroke); positive → flat back edge of
    ///   thickness `2 × back_half_width` (Fancy).
    ///
    /// Head dimensions come from `self.head_length()` and
    /// `self.head_half_height()`, both style-aware.
    fn solid_filled_path(
        &self,
        arrow_length: f32,
        shoulder_ratio: f32,
        wing_back_ratio: f32,
        wing_height_ratio: f32,
        stroke_compensation: f32,
        back_half_width: f32,
    ) -> Path {
        let head_length = self.head_length();
        let head_half_height = self.head_half_height();
        // Inset the body's max half-width by half the stroke so that
        // visible (path + stroke) equals body_max_width at the head.
        let body_max_half = (self.body_max_width() - stroke_compensation).max(0.0) * 0.5;

        let head_length = head_length.min(arrow_length * 0.95);
        let head_outer_x = arrow_length - head_length;
        let shoulder_offset = head_length * shoulder_ratio;
        let head_inner_x = head_outer_x + shoulder_offset;
        // Extended head outer corner (swept back and up). When the wing
        // ratios are 0, this collapses to the plain (head_outer_x, head_half_height).
        let wing_x = head_outer_x - head_length * wing_back_ratio;
        let wing_half_height = head_half_height * (1.0 + wing_height_ratio);

        let mut path = Path::new();
        path.move_to(0.0, back_half_width);
        path.line_to(head_inner_x, body_max_half);
        path.line_to(wing_x, wing_half_height);
        path.line_to(arrow_length, 0.0);
        path.line_to(wing_x, -wing_half_height);
        path.line_to(head_inner_x, -body_max_half);
        path.line_to(0.0, -back_half_width);
        path.close();
        path
    }

    /// Stroke width used by the rounded-outline overlay on Standard arrows.
    /// Equals `body_back_width` so that the back of the path (a single point
    /// at (0, 0)) gets rounded into a half-circle whose visible diameter
    /// equals `body_back_width`. The stroke also rounds the head corners and
    /// widens the body uniformly — `solid_filled_path` insets the path by
    /// this amount so the visible body_max still matches `body_max_width`.
    fn rounded_outline_stroke(&self) -> f32 {
        self.body_back_width()
    }

    /// Paint configured for the rounded outline overlay applied on top of the
    /// solid fill — produces visually-rounded triangle/tail corners. Only
    /// used by Standard; Fancy keeps sharp polygon corners.
    fn rounded_outline_paint(&self) -> Paint {
        let mut p: Paint = self.style.into();
        p.set_line_join(LineJoin::Round);
        p.set_line_cap(LineCap::Round);
        p.set_line_width(self.rounded_outline_stroke());
        p
    }

    /// Build an open V arrowhead path (two line segments from the tip back
    /// to the head corners). Used by Curved/Double arrows. Side length and
    /// apex come from a Curved-specific calibration table + constant — the
    /// V-tip is much wider (~85°) than the Standard filled triangle and
    /// has its own per-size length progression.
    fn head_v_path(&self) -> Path {
        let head_side = self
            .style
            .size
            .to_arrow_curved_head_side(self.style.annotation_size_factor);
        let half_angle = Angle::from_degrees(CURVED_HEAD_FULL_ANGLE_DEG) * 0.5;
        let back_offset = Vec2D::from_angle(half_angle) * (-head_side);
        let mut path = Path::new();
        path.move_to(back_offset.x, -back_offset.y); // top corner
        path.line_to(0.0, 0.0); // tip
        path.line_to(back_offset.x, back_offset.y); // bottom corner
        path
    }

    /// Translate + rotate the canvas so a triangular head can be drawn at
    /// `tip` pointing in `dir`. Caller must canvas.restore() afterwards.
    fn orient_head(&self, canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>, tip: Vec2D, dir: Vec2D) -> bool {
        let len = dir.norm();
        if len < f32::EPSILON {
            return false;
        }
        let unit = dir * (1.0 / len);
        canvas.save();
        canvas.translate(tip.x, tip.y);
        canvas.rotate(unit.angle().radians);
        true
    }

    fn draw_solid(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        end: Vec2D,
        paint: &Paint,
        shoulder_ratio: f32,
        wing_back_ratio: f32,
        wing_height_ratio: f32,
        round_corners: bool,
    ) -> Result<()> {
        let chord = end - self.start;
        let length = chord.norm();
        if length < 1.0 {
            return Ok(());
        }
        let direction = chord * (1.0 / length);
        canvas.save();
        canvas.translate(self.start.x, self.start.y);
        canvas.rotate(direction.angle().radians);
        // Standard's path is inset by the stroke width so that, once the
        // rounded-outline stroke is drawn on top, the visible body matches
        // `body_max_width` at the head. Fancy uses no stroke, so no inset.
        // Standard collapses the back to a single point so the round stroke
        // can grow it into a cap; Fancy keeps a finite flat back edge.
        let (stroke_compensation, back_half_width) = if round_corners {
            (self.rounded_outline_stroke(), 0.0)
        } else {
            (0.0, self.body_back_width() * 0.5)
        };
        let path = self.solid_filled_path(
            length,
            shoulder_ratio,
            wing_back_ratio,
            wing_height_ratio,
            stroke_compensation,
            back_half_width,
        );
        canvas.fill_path(&path, paint);
        if round_corners {
            canvas.stroke_path(&path, &self.rounded_outline_paint());
        }
        canvas.restore();
        Ok(())
    }

    fn draw_curved_with_heads(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        end: Vec2D,
        paint: &Paint,
        head_at_start: bool,
    ) -> Result<()> {
        let Some(control) = self.bezier_control(end) else {
            return Ok(());
        };

        // Curved shaft.
        canvas.save();
        let mut shaft = Path::new();
        shaft.move_to(self.start.x, self.start.y);
        shaft.quad_to(control.x, control.y, end.x, end.y);
        let mut shaft_paint = paint.clone();
        shaft_paint.set_line_width(self.shaft_width());
        shaft_paint.set_line_cap(LineCap::Round);
        shaft_paint.set_line_join(LineJoin::Round);
        canvas.stroke_path(&shaft, &shaft_paint);
        canvas.restore();

        // Curved/Double heads are an open V (two stroked lines from the
        // tip back to the head corners), not a filled triangle. Keeps the
        // shaft+head silhouette slim and consistent.
        let mut head_paint = paint.clone();
        head_paint.set_line_width(self.shaft_width());
        head_paint.set_line_cap(LineCap::Round);
        head_paint.set_line_join(LineJoin::Round);

        // Head at end, tangent points along (end - control).
        if self.orient_head(canvas, end, end - control) {
            canvas.stroke_path(&self.head_v_path(), &head_paint);
            canvas.restore();
        }

        // Optional head at start, tangent points along (start - control)
        // (i.e. outward, so the tip lands at start).
        if head_at_start && self.orient_head(canvas, self.start, self.start - control) {
            canvas.stroke_path(&self.head_v_path(), &head_paint);
            canvas.restore();
        }

        Ok(())
    }
}

impl Drawable for Arrow {
    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        let Some(end) = self.end else {
            return Ok(());
        };
        let paint: Paint = self.style.into();
        match self.arrow_style {
            ArrowStyle::Standard => self.draw_solid(
                canvas,
                end,
                &paint,
                STANDARD_SHOULDER_RATIO,
                0.0,
                0.0,
                true,
            ),
            ArrowStyle::Fancy => self.draw_solid(
                canvas,
                end,
                &paint,
                FANCY_SHOULDER_RATIO,
                FANCY_WING_BACK_RATIO,
                FANCY_WING_HEIGHT_RATIO,
                false,
            ),
            ArrowStyle::Curved => self.draw_curved_with_heads(canvas, end, &paint, false),
            ArrowStyle::Double => self.draw_curved_with_heads(canvas, end, &paint, true),
        }
    }

    fn bounds(&self) -> Option<Rect> {
        let end = self.end?;
        let head = self.head_length();
        let body = self.body_max_width();
        let pad = head.max(body) / 2.0 + 2.0;
        match self.arrow_style {
            ArrowStyle::Standard | ArrowStyle::Fancy => {
                Some(Rect::from_corners(self.start, end).inflated(pad))
            }
            ArrowStyle::Curved | ArrowStyle::Double => {
                let Some(control) = self.bezier_control(end) else {
                    return Some(Rect::from_corners(self.start, end).inflated(pad));
                };
                let pts = self.bezier_sample(end, control, 16);
                let mut min = pts[0];
                let mut max = pts[0];
                for p in &pts {
                    min.x = min.x.min(p.x);
                    min.y = min.y.min(p.y);
                    max.x = max.x.max(p.x);
                    max.y = max.y.max(p.y);
                }
                Some(
                    Rect {
                        pos: min,
                        size: max - min,
                    }
                    .inflated(pad),
                )
            }
        }
    }

    fn hit_test(&self, point: Vec2D, tolerance: f32) -> bool {
        let Some(end) = self.end else {
            return false;
        };
        let head = self.head_length();
        let body = self.body_max_width();
        let shaft = self.shaft_width();
        let pick = match self.arrow_style {
            ArrowStyle::Standard | ArrowStyle::Fancy => body.max(head) / 2.0 + tolerance,
            ArrowStyle::Curved | ArrowStyle::Double => shaft.max(head) / 2.0 + tolerance,
        };
        match self.arrow_style {
            ArrowStyle::Standard | ArrowStyle::Fancy => {
                point_to_segment_distance(point, self.start, end) <= pick
            }
            ArrowStyle::Curved | ArrowStyle::Double => {
                let Some(control) = self.bezier_control(end) else {
                    return point_to_segment_distance(point, self.start, end) <= pick;
                };
                let pts = self.bezier_sample(end, control, 24);
                pts.windows(2)
                    .any(|w| point_to_segment_distance(point, w[0], w[1]) <= pick)
            }
        }
    }

    fn translate(&mut self, delta: Vec2D) {
        self.start += delta;
        if let Some(end) = self.end.as_mut() {
            *end += delta;
        }
        if let Some(c) = self.curve_control.as_mut() {
            *c += delta;
        }
    }

    fn handles(&self) -> Vec<Handle> {
        let Some(end) = self.end else {
            return Vec::new();
        };
        // Curved/Double arrows get bigger hit targets on all three handles —
        // their shafts are wide and the midpoint sits on the visible shaft,
        // so the default 12 px radius is hard to grab without precision.
        let curved = matches!(self.arrow_style, ArrowStyle::Curved | ArrowStyle::Double);
        let radius = if curved {
            crate::tools::HANDLE_HIT_RADIUS * 2.0
        } else {
            crate::tools::HANDLE_HIT_RADIUS
        };
        let mut handles = vec![
            Handle::new(HandleId::Start, self.start).with_hit_radius(radius),
            Handle::new(HandleId::End, end).with_hit_radius(radius),
        ];
        // Curved / Double arrows expose a third middle handle so the user
        // can bend the arc. The handle sits *on the curve* (at t=0.5)
        // rather than on the off-curve Bezier control point so it tracks
        // the visible shaft.
        if curved
            && let Some(c) = self.bezier_control(end)
        {
            // B(0.5) for a quadratic Bezier with control C = 0.25 S + 0.5 C + 0.25 E.
            let midpoint = self.start * 0.25 + c * 0.5 + end * 0.25;
            handles.push(Handle::new(HandleId::Control, midpoint).with_hit_radius(radius));
        }
        handles
    }

    fn move_handle(&mut self, handle: HandleId, to: Vec2D) {
        match handle {
            HandleId::Start => self.start = to,
            HandleId::End => self.end = Some(to),
            HandleId::Control => {
                // The handle is rendered on the curve at t=0.5 (the
                // midpoint). The user-visible drag target is therefore the
                // curve midpoint M; we back-solve the Bezier control point
                // from M = 0.25 S + 0.5 C + 0.25 E → C = 2M − 0.5(S + E).
                let Some(end) = self.end else { return };
                let new_control = to * 2.0 - (self.start + end) * 0.5;
                self.curve_control = Some(new_control);
            }
            _ => {}
        }
    }

    fn set_style(&mut self, style: Style) {
        self.style = style;
    }

    fn render_glow(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: FontId,
        _bounds: (Vec2D, Vec2D),
        device_pixel_ratio: f32,
    ) -> Result<()> {
        let Some(end) = self.end else {
            return Ok(());
        };
        let chord = end - self.start;
        let length = chord.norm();
        if length < 1.0 {
            return Ok(());
        }

        // Visible halo width outside the silhouette, in image units, scaled
        // so the on-screen halo is halo_pad CSS pixels regardless of zoom
        // or DPR. For Standard the rounded-outline overlay extends half its
        // stroke outside the fill, so the halo brush has to cover that too.
        let halo_pad = halo_in_image_units(canvas, device_pixel_ratio);

        let mut glow_paint = Paint::color(GLOW_COLOR);
        glow_paint.set_line_cap(LineCap::Round);
        glow_paint.set_line_join(LineJoin::Round);

        match self.arrow_style {
            ArrowStyle::Standard | ArrowStyle::Fancy => {
                let (
                    shoulder_ratio,
                    wing_back_ratio,
                    wing_height_ratio,
                    stroke_compensation,
                    outline_half,
                    back_half_width,
                ) = match self.arrow_style {
                    ArrowStyle::Fancy => (
                        FANCY_SHOULDER_RATIO,
                        FANCY_WING_BACK_RATIO,
                        FANCY_WING_HEIGHT_RATIO,
                        0.0,
                        0.0,
                        self.body_back_width() * 0.5,
                    ),
                    _ => {
                        let stroke = self.rounded_outline_stroke();
                        (
                            STANDARD_SHOULDER_RATIO,
                            0.0,
                            0.0,
                            stroke,
                            stroke * 0.5,
                            0.0,
                        )
                    }
                };
                let direction = chord * (1.0 / length);
                canvas.save();
                canvas.translate(self.start.x, self.start.y);
                canvas.rotate(direction.angle().radians);
                let path = self.solid_filled_path(
                    length,
                    shoulder_ratio,
                    wing_back_ratio,
                    wing_height_ratio,
                    stroke_compensation,
                    back_half_width,
                );
                let mut p = glow_paint.clone();
                p.set_line_width(2.0 * (outline_half + halo_pad));
                canvas.stroke_path(&path, &p);
                canvas.restore();
            }
            ArrowStyle::Curved | ArrowStyle::Double => {
                let Some(control) = self.bezier_control(end) else {
                    return Ok(());
                };
                let mut shaft_glow = glow_paint.clone();
                shaft_glow.set_line_width(self.shaft_width() + 2.0 * halo_pad);

                canvas.save();
                let mut shaft = Path::new();
                shaft.move_to(self.start.x, self.start.y);
                shaft.quad_to(control.x, control.y, end.x, end.y);
                canvas.stroke_path(&shaft, &shaft_glow);
                canvas.restore();

                // Open-V head: stroked at shaft_width, so the glow brush
                // is shaft_width + 2 * halo_pad to bracket the V on both
                // sides (the head isn't filled — both sides are visible).
                let mut head_glow = glow_paint.clone();
                head_glow.set_line_width(self.shaft_width() + 2.0 * halo_pad);

                if self.orient_head(canvas, end, end - control) {
                    canvas.stroke_path(&self.head_v_path(), &head_glow);
                    canvas.restore();
                }
                if matches!(self.arrow_style, ArrowStyle::Double)
                    && self.orient_head(canvas, self.start, self.start - control)
                {
                    canvas.stroke_path(&self.head_v_path(), &head_glow);
                    canvas.restore();
                }
            }
        }
        Ok(())
    }
}
