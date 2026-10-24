use std::time::Instant;

use super::SelectionType;
use femtovg::{Color, FontId, LineJoin, Paint, Path};
use serde::Deserialize;

use crate::{
    configuration::APP_CONFIG,
    math::Vec2D,
    sketch_board::{KeyEventMsg, MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    style::Style,
};

use super::{Drawable, DrawableClone, Straight, Tool, ToolUpdateResult, Tools};
use relm4::Sender;
use relm4::gtk::gdk::Key;

#[derive(Default, Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BrushMode {
    #[default]
    Pen,
    Highlighter,
    Brush,
}

#[derive(Default)]
pub struct BrushTool {
    mode: BrushMode,
    drawable: Option<BrushDrawable>,
    style: Style,
    input_enabled: bool,
    sender: Option<Sender<SketchBoardInput>>,
}

impl BrushTool {
    pub fn with_mode(mode: BrushMode) -> Self {
        Self {
            mode,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone)]
pub struct BrushDrawable {
    mode: BrushMode,
    // The start point of the brush stroke this is relative to canvas
    // after this the points are relative to the start point
    start_point: Vec2D,
    points: Vec<Vec2D>,
    smoother: Smoother,
    style: Style,
}

impl BrushDrawable {
    fn add_point(&mut self, point: Vec2D, straight: Option<Straight>) {
        let Some(straight) = straight else {
            self.points.push(self.smoother.update(point));
            return;
        };
        // the straight segment replaces its end point while it is drawn
        self.points.pop();
        let anchor = self.points.last().copied().unwrap_or_else(Vec2D::zero);
        self.points.push(anchor + straight.snap(point - anchor));
    }

    // a single segment is handled like a line: its bounds are start and end
    fn is_line(&self) -> bool {
        self.points.len() == 1
    }

    fn line_width(&self) -> f32 {
        let size_factor = self.style.annotation_size_factor;
        if self.mode == BrushMode::Highlighter {
            self.style.size.to_highlight_width(size_factor)
        } else {
            self.style.size.to_line_width(size_factor)
        }
    }
}

impl Drawable for BrushDrawable {
    fn anchors(&self) -> Option<(Vec2D, Vec2D)> {
        if self.is_line() {
            return Some((self.start_point, self.start_point + self.points[0]));
        }
        Some(super::calculate_line_bounds(self.start_point, &self.points))
    }

    fn get_bounding_box(&self) -> Option<(Vec2D, Vec2D)> {
        let (tl, br) = super::calculate_line_bounds(self.start_point, &self.points);
        let line_width = self.line_width();
        // round and butt caps and the round joins stay within half the width
        let mut margin = line_width / 2.0;
        if self.mode == BrushMode::Brush {
            // the soft edge, see draw()
            margin += line_width.min(16.0);
        }
        Some(super::bounding_box_with_margin(tl, br, margin))
    }

    fn get_selection_type(&self) -> SelectionType {
        if self.is_line() {
            SelectionType::Line
        } else {
            SelectionType::BoundingBoxAllHandles
        }
    }

    fn hit_test(&self, pos: Vec2D, tolerance: f32) -> bool {
        let tolerance = tolerance + self.line_width() / 2.0;
        super::hit_test_line(pos, self.start_point, &self.points, tolerance)
    }

    fn translate(&mut self, delta: Vec2D) {
        self.start_point += delta;
    }

    fn resize(&mut self, tl: Vec2D, br: Vec2D, _keep_aspect: bool) {
        if self.is_line() {
            // points are relative to the start point
            self.start_point = tl;
            self.points[0] = br - tl;
            return;
        }
        super::resize_line_bounds(tl, br, &mut self.start_point, &mut self.points);
    }

    fn get_style(&self) -> Option<&Style> {
        Some(&self.style)
    }

    fn get_style_mut(&mut self) -> Option<&mut Style> {
        Some(&mut self.style)
    }

    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> anyhow::Result<()> {
        let mut path = Path::new();
        let start_point = self.start_point;
        path.move_to(start_point.x, start_point.y);
        if self.points.iter().all(|p| *p == Vec2D::zero()) {
            // also before the first move, when there are no points yet
            // draw a point at start - a little bit hacky :-)
            path.line_to(start_point.x + 0.01, start_point.y);
        } else {
            for p in self.points.iter() {
                path.line_to(start_point.x + p.x, start_point.y + p.y);
            }
        }

        let mut paint = Paint::from(self.style).with_line_join(LineJoin::Round);

        if self.mode == BrushMode::Highlighter {
            paint.set_color(Color::rgba(
                self.style.color.r,
                self.style.color.g,
                self.style.color.b,
                (255.0 * 0.4) as u8,
            ));
            paint.set_line_width(self.line_width());
        }

        canvas.save();
        if self.mode == BrushMode::Brush {
            // soft edge: blurred shadow in the stroke color under the stroke;
            // femtovg's blur is exact only up to 16, larger values get clamped
            let blur = self.line_width().min(16.0);
            canvas.set_shadow_color(self.style.color.into());
            canvas.set_shadow_blur(blur);
            canvas.set_shadow_offset(0.0, 0.0);
            paint.set_color(Color::rgba(
                self.style.color.r,
                self.style.color.g,
                self.style.color.b,
                (255.0 * 0.8) as u8,
            ));
        }
        canvas.stroke_path(&path, &paint);
        canvas.restore();

        Ok(())
    }
}

impl Tool for BrushTool {
    fn input_enabled(&self) -> bool {
        self.input_enabled
    }

    fn set_input_enabled(&mut self, value: bool) {
        self.input_enabled = value;
    }

    fn get_tool_type(&self) -> super::Tools {
        match self.mode {
            BrushMode::Pen => Tools::Pen,
            BrushMode::Highlighter => Tools::Highlighter,
            BrushMode::Brush => Tools::Brush,
        }
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        match event.type_ {
            MouseEventType::BeginDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                let Some(brush) = &mut self.drawable else {
                    return ToolUpdateResult::Unmodified;
                };
                brush.start_point = event.pos;
                ToolUpdateResult::Redraw
            }
            MouseEventType::UpdateDrag | MouseEventType::EndDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }
                let Some(brush) = &mut self.drawable else {
                    return ToolUpdateResult::Unmodified;
                };

                let straight = Straight::from_modifier(event.modifier);
                brush.add_point(event.pos, straight);

                // with Shift or Ctrl the last segment is straight, show its dimensions;
                // points are relative to the start point
                if straight.is_some()
                    && let Some(sender) = &self.sender
                    && let Some(&last) = brush.points.last()
                {
                    let previous = match brush.points.len() {
                        1 => Vec2D::zero(),
                        n => brush.points[n - 2],
                    };
                    sender
                        .send(SketchBoardInput::LineDimensionsUpdate(last - previous))
                        .ok();
                }

                if event.type_ == MouseEventType::UpdateDrag {
                    return ToolUpdateResult::Redraw;
                }
                // commit
                let result = brush.clone_box();
                self.drawable = None;

                ToolUpdateResult::Commit(result)
            }
            MouseEventType::Click => {
                if event.button != MouseButton::Primary {
                    return ToolUpdateResult::Unmodified;
                }
                self.drawable = Some(BrushDrawable {
                    mode: self.mode,
                    start_point: event.pos,
                    smoother: Smoother::new(APP_CONFIG.read().brush_smooth_history_size()),
                    points: vec![],
                    style: self.style,
                });
                ToolUpdateResult::Unmodified
            }
            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn get_drawable(&self) -> Option<&dyn Drawable> {
        match &self.drawable {
            Some(d) => Some(d),
            None => None,
        }
    }

    fn handle_key_release_event(&mut self, event: KeyEventMsg) -> ToolUpdateResult {
        if matches!(
            event.key,
            Key::Shift_L | Key::Shift_R | Key::Control_L | Key::Control_R
        ) && let Some(brush) = &mut self.drawable
            // there are no points yet before the first move
            && let Some(&last) = brush.points.last()
        {
            // duplicate the last point to maintain the current segment
            brush.points.push(last);
            brush.smoother.reset(last);
        }
        ToolUpdateResult::Unmodified
    }

    fn handle_style_event(&mut self, style: Style) -> ToolUpdateResult {
        self.style = style;
        ToolUpdateResult::Unmodified
    }

    fn active(&self) -> bool {
        self.drawable.is_some()
    }

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>) {
        self.sender = Some(sender);
    }
}

#[derive(Debug, Clone)]
pub struct Smoother {
    history: Vec<Vec2D>, // last N raw inputs
    smoothed_point: Option<Vec2D>,
    max_history: usize,
    last_update: Option<Instant>,
}

impl Smoother {
    pub fn new(max_history: usize) -> Self {
        Self {
            history: Vec::with_capacity(max_history + 1),
            smoothed_point: None,
            max_history,
            last_update: None,
        }
    }

    pub fn reset(&mut self, start: Vec2D) {
        self.history.clear();
        self.smoothed_point = Some(start);
        self.last_update = Some(Instant::now());
    }

    pub fn update(&mut self, raw: Vec2D) -> Vec2D {
        if self.max_history == 0 {
            return raw;
        }
        // Add to history
        if self.history.len() >= self.max_history {
            self.history.remove(0);
        }
        self.history.push(raw);

        // Compute averaged raw input
        let n = self.history.len() as f32;
        let sum = self
            .history
            .iter()
            .fold(Vec2D { x: 0.0, y: 0.0 }, |acc, p| Vec2D {
                x: acc.x + p.x,
                y: acc.y + p.y,
            });
        let averaged_raw = Vec2D {
            x: sum.x / n,
            y: sum.y / n,
        };

        // Estimate speed (optional)
        let dt = if let Some(last_update) = self.last_update {
            let now = Instant::now();
            let dt = now.duration_since(last_update).as_secs_f32();
            self.last_update = Some(now);
            dt
        } else {
            self.last_update = Some(Instant::now());
            0.0
        };
        let last = *self.history.last().unwrap_or(&raw);
        let first = self.history.first().unwrap_or(&raw);
        let distance = last.distance_to(*first);
        let total_dt = dt * self.history.len() as f32;
        let speed = distance / total_dt.clamp(0.001, 1.0);

        let alpha = Self::compute_alpha(speed);

        // Smooth against previous smoothed point
        let smoothed = if let Some(prev) = self.smoothed_point {
            Vec2D {
                x: alpha * averaged_raw.x + (1.0 - alpha) * prev.x,
                y: alpha * averaged_raw.y + (1.0 - alpha) * prev.y,
            }
        } else {
            averaged_raw
        };

        self.smoothed_point = Some(smoothed);
        smoothed
    }

    fn compute_alpha(speed: f32) -> f32 {
        let min_alpha = 0.05;
        let max_alpha = 0.5;
        let clamped_speed = speed.clamp(0.01, 500.0);
        let norm = (clamped_speed / 500.0).sqrt();
        min_alpha + (max_alpha - min_alpha) * norm
    }
}
