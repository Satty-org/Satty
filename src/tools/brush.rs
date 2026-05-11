use std::time::Instant;

use anyhow::Result;
use femtovg::{FontId, Path};

use crate::{
    configuration::APP_CONFIG,
    math::{Rect, Vec2D, point_to_segment_distance},
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    style::Style,
};

use super::{
    Drawable, DrawableClone, GLOW_COLOR, Handle, HandleId, Tool, ToolUpdateResult, Tools,
    bbox_handles, bbox_resize, halo_in_image_units,
};
use relm4::Sender;

#[derive(Default)]
pub struct BrushTool {
    drawable: Option<BrushDrawable>,
    style: Style,
    input_enabled: bool,
    sender: Option<Sender<SketchBoardInput>>,
}

#[derive(Debug, Clone)]
pub struct BrushDrawable {
    // The start point of the brush stroke this is relative to canvas
    // after this the points are relative to the start point
    start_point: Option<Vec2D>,
    points: Vec<Vec2D>,
    smoother: Smoother,
    style: Style,
}

impl BrushDrawable {
    fn add_point(&mut self, point: Vec2D) {
        self.points.push(self.smoother.update(point));
    }
}

impl Drawable for BrushDrawable {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> anyhow::Result<()> {
        if self.points.is_empty() {
            return Ok(());
        }

        let Some(start_point) = self.start_point else {
            return Ok(());
        };

        canvas.save();
        let mut path = Path::new();

        path.move_to(start_point.x, start_point.y);
        for p in self.points.iter().skip(1) {
            path.line_to(start_point.x + p.x, start_point.y + p.y);
        }

        canvas.stroke_path(&path, &self.style.into());
        canvas.restore();
        Ok(())
    }

    fn bounds(&self) -> Option<Rect> {
        let start = self.start_point?;
        if self.points.len() < 2 {
            return None;
        }
        let mut min = start;
        let mut max = start;
        for p in self.points.iter().skip(1) {
            let abs = start + *p;
            min.x = min.x.min(abs.x);
            min.y = min.y.min(abs.y);
            max.x = max.x.max(abs.x);
            max.y = max.y.max(abs.y);
        }
        let stroke = self
            .style
            .size
            .to_line_width(self.style.annotation_size_factor);
        Some(
            Rect {
                pos: min,
                size: max - min,
            }
            .inflated(stroke / 2.0),
        )
    }

    fn hit_test(&self, point: Vec2D, tolerance: f32) -> bool {
        let Some(start) = self.start_point else {
            return false;
        };
        if self.points.len() < 2 {
            return false;
        }
        let stroke = self
            .style
            .size
            .to_line_width(self.style.annotation_size_factor);
        let pick = stroke / 2.0 + tolerance;
        // Quick reject by inflated bounds.
        if !self
            .bounds()
            .map(|b| b.inflated(tolerance).contains(point))
            .unwrap_or(false)
        {
            return false;
        }
        let mut prev = start;
        for p in self.points.iter().skip(1) {
            let cur = start + *p;
            if point_to_segment_distance(point, prev, cur) <= pick {
                return true;
            }
            prev = cur;
        }
        false
    }

    fn translate(&mut self, delta: Vec2D) {
        if let Some(start) = self.start_point.as_mut() {
            *start += delta;
        }
    }

    fn handles(&self) -> Vec<Handle> {
        // Standard 8-handle bbox so the user can clearly see a brush
        // stroke is selected and resize it. Body-drag (translate) still
        // works the same when the user clicks anywhere on the stroke
        // body away from the handles.
        self.bounds().map(bbox_handles).unwrap_or_default()
    }

    fn move_handle(&mut self, handle: HandleId, to: Vec2D) {
        // Uniform scale about the pinned corner/edge. Stroke width is
        // not scaled — adjust it with the Size selector. Uses the
        // inflated `bounds()` for both old and new sides of the
        // transform; the resulting (stroke/2 per side) drift vs. the
        // dragged handle position is imperceptible at typical widths.
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
        if let Some(start) = self.start_point.as_mut() {
            let new_x = new.pos.x + (start.x - old.pos.x) * scale_x;
            let new_y = new.pos.y + (start.y - old.pos.y) * scale_y;
            *start = Vec2D::new(new_x, new_y);
        }
        // `points[0]` is the initial raw input, unused by the draw
        // path (skip(1) below) but kept here for buffer consistency.
        // `points[1..]` are offsets relative to start_point — scale
        // each axis independently.
        for p in self.points.iter_mut().skip(1) {
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
        _font: FontId,
        _bounds: (Vec2D, Vec2D),
        device_pixel_ratio: f32,
    ) -> Result<()> {
        let Some(start) = self.start_point else {
            return Ok(());
        };
        if self.points.len() < 2 {
            return Ok(());
        }
        let halo = halo_in_image_units(canvas, device_pixel_ratio);
        canvas.save();
        let mut path = Path::new();
        path.move_to(start.x, start.y);
        for p in self.points.iter().skip(1) {
            path.line_to(start.x + p.x, start.y + p.y);
        }
        let stroke_width = self
            .style
            .size
            .to_line_width(self.style.annotation_size_factor)
            + 2.0 * halo;
        let mut paint = femtovg::Paint::color(GLOW_COLOR);
        paint.set_line_width(stroke_width);
        paint.set_line_cap(femtovg::LineCap::Round);
        paint.set_line_join(femtovg::LineJoin::Round);
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
        Tools::Brush
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        match event.type_ {
            MouseEventType::BeginDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                // BeginDrag may fire before Click after the gesture-controller
                // reorder, so create the drawable on demand here.
                let brush = self.drawable.get_or_insert_with(|| BrushDrawable {
                    start_point: None,
                    smoother: Smoother::new(APP_CONFIG.read().brush_smooth_history_size()),
                    points: vec![event.pos],
                    style: self.style,
                });
                brush.start_point = Some(event.pos);
                ToolUpdateResult::Redraw
            }
            MouseEventType::EndDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                let Some(brush) = &mut self.drawable else {
                    return ToolUpdateResult::Unmodified;
                };
                brush.add_point(event.pos);

                // commit
                let result = brush.clone_box();
                self.drawable = None;

                ToolUpdateResult::Commit(result)
            }
            MouseEventType::UpdateDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                let Some(brush) = &mut self.drawable else {
                    return ToolUpdateResult::Unmodified;
                };
                brush.add_point(event.pos);
                ToolUpdateResult::Redraw
            }
            MouseEventType::Click => {
                if event.button != MouseButton::Primary {
                    return ToolUpdateResult::Unmodified;
                }
                // BeginDrag fires before Click and may have already created
                // the drawable + set start_point — don't overwrite it.
                self.drawable.get_or_insert_with(|| BrushDrawable {
                    start_point: None,
                    smoother: Smoother::new(APP_CONFIG.read().brush_smooth_history_size()),
                    points: vec![event.pos],
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

    fn handle_style_event(&mut self, style: Style) -> ToolUpdateResult {
        self.style = style;
        ToolUpdateResult::Unmodified
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
        let distance = last.distance_to(first);
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
