use std::time::Instant;

use femtovg::{FontId, Path};

use crate::{
    configuration::APP_CONFIG,
    math::Vec2D,
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType},
    style::Style,
};

use super::{Drawable, DrawableClone, Tool, ToolUpdateResult, Tools};

#[derive(Default)]
pub struct BrushTool {
    drawable: Option<BrushDrawable>,
    style: Style,
    input_enabled: bool,
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

                let Some(brush) = &mut self.drawable else {
                    return ToolUpdateResult::Unmodified;
                };
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
                self.drawable = Some(BrushDrawable {
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
