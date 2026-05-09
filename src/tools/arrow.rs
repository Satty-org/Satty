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
    Drawable, DrawableClone, GLOW_COLOR, GLOW_STROKE_WIDTH, GLOW_STROKE_WIDTH_WIDE, Handle,
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

const HEAD_ANGLE_DEG: f32 = 60.0;
/// Curvature of curved/double arrows as a fraction of the chord length.
const CURVE_AMOUNT: f32 = 0.25;

impl Arrow {
    fn head_side_length(&self) -> f32 {
        self.style
            .size
            .to_arrow_head_length(self.style.annotation_size_factor)
    }

    fn tail_width(&self) -> f32 {
        self.style
            .size
            .to_arrow_tail_width(self.style.annotation_size_factor)
    }

    fn shaft_width(&self) -> f32 {
        self.style
            .size
            .to_line_width(self.style.annotation_size_factor)
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

    /// Build the Standard-arrow path in arrow-local coords (start at origin,
    /// tip at (length, 0)). Tapered tail + triangle head with a subtle
    /// shoulder where the head meets the tail.
    fn standard_path(&self, arrow_length: f32) -> Path {
        let head_side = self.head_side_length();
        let tail_half = self.tail_width() * 0.5;
        let half_angle = Angle::from_degrees(HEAD_ANGLE_DEG) * 0.5;
        let head_back = Vec2D::new(arrow_length, 0.0) - Vec2D::from_angle(half_angle) * head_side;

        let head_outer_x = head_back.x; // x of the head's outer corners
        let head_half_width = -head_back.y;
        // Shoulder notch: the inner corner where the tail meets the head is
        // slightly forward of the head's outer base, so the silhouette has a
        // small forward-slanting joint instead of a sharp 90° step.
        let shoulder_offset = head_side * 0.12;
        let head_inner_x = head_outer_x + shoulder_offset;
        // Narrow back stub (~7% of tail width) — barely a flat back, just
        // enough that the rounded outline doesn't pinch into a sharp tip.
        let start_half = tail_half * 0.07;

        let mut path = Path::new();
        path.move_to(0.0, start_half);
        path.line_to(head_inner_x, tail_half); // tail meets head at shoulder
        path.line_to(head_outer_x, head_half_width); // head outer top corner
        path.line_to(arrow_length, 0.0); // tip
        path.line_to(head_outer_x, -head_half_width); // head outer bottom
        path.line_to(head_inner_x, -tail_half); // bottom shoulder
        path.line_to(0.0, -start_half);
        path.close();
        path
    }

    /// Paint configured for the rounded outline overlay applied on top of the
    /// solid fill — produces visually-rounded triangle/tail corners.
    fn rounded_outline_paint(&self) -> Paint {
        let mut p: Paint = self.style.into();
        p.set_line_join(LineJoin::Round);
        p.set_line_cap(LineCap::Round);
        // Stroke width controls effective corner radius (~half of width).
        p.set_line_width(8.0);
        p
    }

    /// Build a triangular arrowhead path at the tip (origin) extending back
    /// along -x. Used by curved/double arrows.
    fn head_path(&self) -> Path {
        let head_side = self.head_side_length();
        let half_angle = Angle::from_degrees(HEAD_ANGLE_DEG) * 0.5;
        let back_offset = Vec2D::from_angle(half_angle) * (-head_side);
        let mut path = Path::new();
        path.move_to(0.0, 0.0);
        path.line_to(back_offset.x, -back_offset.y); // top
        path.line_to(back_offset.x, back_offset.y); // bottom
        path.close();
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

    fn draw_standard(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        end: Vec2D,
        paint: &Paint,
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
        let path = self.standard_path(length);
        canvas.fill_path(&path, paint);
        // Rounded-corner overlay: strokes the same path with a fat round join
        // in the fill color, smoothing the triangle corners and the tail-back
        // stub without changing the silhouette dimensions noticeably.
        canvas.stroke_path(&path, &self.rounded_outline_paint());
        canvas.restore();
        Ok(())
    }

    fn draw_fancy(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        end: Vec2D,
        paint: &Paint,
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

        let head_side = self.head_side_length();
        let half_angle = Angle::from_degrees(HEAD_ANGLE_DEG) * 0.5;
        let head_back = Vec2D::new(length, 0.0) - Vec2D::from_angle(half_angle) * head_side;

        // Thin stroked shaft, stopping just before the head base.
        let mut shaft_paint = paint.clone();
        shaft_paint.set_line_width(self.shaft_width());
        shaft_paint.set_line_cap(LineCap::Round);
        let mut shaft = Path::new();
        shaft.move_to(0.0, 0.0);
        shaft.line_to(head_back.x, 0.0);
        canvas.stroke_path(&shaft, &shaft_paint);

        // Filled triangular head with rounded corners.
        let mut head = Path::new();
        head.move_to(head_back.x, -head_back.y);
        head.line_to(length, 0.0);
        head.line_to(head_back.x, head_back.y);
        head.close();
        canvas.fill_path(&head, paint);
        canvas.stroke_path(&head, &self.rounded_outline_paint());

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

        let outline = self.rounded_outline_paint();

        // Head at end, tangent points along (end - control).
        if self.orient_head(canvas, end, end - control) {
            let head = self.head_path();
            canvas.fill_path(&head, paint);
            canvas.stroke_path(&head, &outline);
            canvas.restore();
        }

        // Optional head at start, tangent points along (start - control)
        // (i.e. outward, so the tip lands at start).
        if head_at_start && self.orient_head(canvas, self.start, self.start - control) {
            let head = self.head_path();
            canvas.fill_path(&head, paint);
            canvas.stroke_path(&head, &outline);
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
            ArrowStyle::Standard => self.draw_standard(canvas, end, &paint),
            ArrowStyle::Fancy => self.draw_fancy(canvas, end, &paint),
            ArrowStyle::Curved => self.draw_curved_with_heads(canvas, end, &paint, false),
            ArrowStyle::Double => self.draw_curved_with_heads(canvas, end, &paint, true),
        }
    }

    fn bounds(&self) -> Option<Rect> {
        let end = self.end?;
        let head = self.head_side_length();
        let tail = self.tail_width();
        let pad = head.max(tail) / 2.0 + 2.0;
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
        let head = self.head_side_length();
        let tail = self.tail_width();
        let shaft = self.shaft_width();
        let pick = match self.arrow_style {
            ArrowStyle::Standard => tail.max(head) / 2.0 + tolerance,
            ArrowStyle::Fancy => shaft.max(head) / 2.0 + tolerance,
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
        let mut handles = vec![
            Handle {
                id: HandleId::Start,
                pos: self.start,
            },
            Handle {
                id: HandleId::End,
                pos: end,
            },
        ];
        // Curved / Double arrows expose a third middle handle on the Bezier
        // control point so the user can bend the arc to any angle.
        if matches!(self.arrow_style, ArrowStyle::Curved | ArrowStyle::Double)
            && let Some(c) = self.bezier_control(end)
        {
            handles.push(Handle {
                id: HandleId::Control,
                pos: c,
            });
        }
        handles
    }

    fn move_handle(&mut self, handle: HandleId, to: Vec2D) {
        match handle {
            HandleId::Start => self.start = to,
            HandleId::End => self.end = Some(to),
            HandleId::Control => self.curve_control = Some(to),
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
    ) -> Result<()> {
        let Some(end) = self.end else {
            return Ok(());
        };
        let chord = end - self.start;
        let length = chord.norm();
        if length < 1.0 {
            return Ok(());
        }

        let mut glow_paint = Paint::color(GLOW_COLOR);
        glow_paint.set_line_width(GLOW_STROKE_WIDTH);
        glow_paint.set_line_cap(LineCap::Round);
        glow_paint.set_line_join(LineJoin::Round);

        match self.arrow_style {
            ArrowStyle::Standard => {
                // Standard arrow's draw method strokes a wide rounded outline
                // overlay (8 px) at the same path. Use the wider glow stroke
                // so a halo remains visible outside the outline; the inner
                // half is masked by the arrow's fill.
                let direction = chord * (1.0 / length);
                canvas.save();
                canvas.translate(self.start.x, self.start.y);
                canvas.rotate(direction.angle().radians);
                let path = self.standard_path(length);
                let mut wide = glow_paint.clone();
                wide.set_line_width(GLOW_STROKE_WIDTH_WIDE);
                canvas.stroke_path(&path, &wide);
                canvas.restore();
            }
            ArrowStyle::Fancy => {
                let direction = chord * (1.0 / length);
                canvas.save();
                canvas.translate(self.start.x, self.start.y);
                canvas.rotate(direction.angle().radians);

                // Trace the shaft + head outline as one combined path for a
                // tight glow that follows the visible silhouette.
                let head_side = self.head_side_length();
                let half_angle = Angle::from_degrees(HEAD_ANGLE_DEG) * 0.5;
                let head_back =
                    Vec2D::new(length, 0.0) - Vec2D::from_angle(half_angle) * head_side;

                let mut shaft = Path::new();
                shaft.move_to(0.0, 0.0);
                shaft.line_to(head_back.x, 0.0);
                canvas.stroke_path(&shaft, &glow_paint);

                let mut head = Path::new();
                head.move_to(head_back.x, -head_back.y);
                head.line_to(length, 0.0);
                head.line_to(head_back.x, head_back.y);
                head.close();
                canvas.stroke_path(&head, &glow_paint);
                canvas.restore();
            }
            ArrowStyle::Curved | ArrowStyle::Double => {
                let Some(control) = self.bezier_control(end) else {
                    return Ok(());
                };
                canvas.save();
                let mut shaft = Path::new();
                shaft.move_to(self.start.x, self.start.y);
                shaft.quad_to(control.x, control.y, end.x, end.y);
                canvas.stroke_path(&shaft, &glow_paint);
                canvas.restore();

                if self.orient_head(canvas, end, end - control) {
                    let head = self.head_path();
                    canvas.stroke_path(&head, &glow_paint);
                    canvas.restore();
                }
                if matches!(self.arrow_style, ArrowStyle::Double)
                    && self.orient_head(canvas, self.start, self.start - control)
                {
                    let head = self.head_path();
                    canvas.stroke_path(&head, &glow_paint);
                    canvas.restore();
                }
            }
        }
        Ok(())
    }
}
