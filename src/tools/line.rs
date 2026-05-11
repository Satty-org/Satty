use anyhow::Result;
use femtovg::{FontId, Path};
use relm4::{
    Sender,
    gtk::gdk::{Key, ModifierType},
};

use crate::{
    math::{Rect, Vec2D, point_to_segment_distance},
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    style::Style,
};

use super::{
    Drawable, DrawableClone, GLOW_COLOR, Handle, HandleId, Tool, ToolUpdateResult, Tools,
    halo_in_image_units,
};

#[derive(Default)]
pub struct LineTool {
    line: Option<Line>,
    style: Style,
    input_enabled: bool,
    sender: Option<Sender<SketchBoardInput>>,
}

#[derive(Clone, Copy, Debug)]
pub struct Line {
    start: Vec2D,
    direction: Option<Vec2D>,
    style: Style,
}

impl Drawable for Line {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        let direction = match self.direction {
            Some(d) => d,
            None => return Ok(()), // exit early if no direction
        };

        canvas.save();

        let mut path = Path::new();
        path.move_to(self.start.x, self.start.y);
        path.line_to(self.start.x + direction.x, self.start.y + direction.y);

        let mut paint: femtovg::Paint = self.style.into();
        paint.set_line_cap(femtovg::LineCap::Round);
        paint.set_line_join(femtovg::LineJoin::Round);
        canvas.stroke_path(&path, &paint);

        canvas.restore();

        Ok(())
    }

    fn bounds(&self) -> Option<Rect> {
        let dir = self.direction?;
        let end = self.start + dir;
        let stroke = self
            .style
            .size
            .to_line_width(self.style.annotation_size_factor);
        Some(Rect::from_corners(self.start, end).inflated(stroke / 2.0))
    }

    fn hit_test(&self, point: Vec2D, tolerance: f32) -> bool {
        let Some(dir) = self.direction else {
            return false;
        };
        let end = self.start + dir;
        let stroke = self
            .style
            .size
            .to_line_width(self.style.annotation_size_factor);
        point_to_segment_distance(point, self.start, end) <= stroke / 2.0 + tolerance
    }

    fn translate(&mut self, delta: Vec2D) {
        // direction is a relative offset, so translating the line only moves start.
        self.start += delta;
    }

    fn handles(&self) -> Vec<Handle> {
        let Some(dir) = self.direction else {
            return Vec::new();
        };
        vec![
            Handle::new(HandleId::Start, self.start),
            Handle::new(HandleId::End, self.start + dir),
        ]
    }

    fn move_handle(&mut self, handle: HandleId, to: Vec2D) {
        let Some(dir) = self.direction else { return };
        let cur_end = self.start + dir;
        match handle {
            HandleId::Start => {
                self.start = to;
                self.direction = Some(cur_end - to);
            }
            HandleId::End => {
                self.direction = Some(to - self.start);
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

    fn render_glow(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: FontId,
        _bounds: (Vec2D, Vec2D),
        device_pixel_ratio: f32,
    ) -> Result<()> {
        let Some(direction) = self.direction else {
            return Ok(());
        };
        let halo = halo_in_image_units(canvas, device_pixel_ratio);
        canvas.save();
        let mut path = Path::new();
        path.move_to(self.start.x, self.start.y);
        path.line_to(self.start.x + direction.x, self.start.y + direction.y);
        let stroke_width = self
            .style
            .size
            .to_line_width(self.style.annotation_size_factor)
            + 2.0 * halo;
        let mut paint = femtovg::Paint::color(GLOW_COLOR);
        paint.set_line_width(stroke_width);
        paint.set_line_cap(femtovg::LineCap::Round);
        canvas.stroke_path(&path, &paint);
        canvas.restore();
        Ok(())
    }
}

impl Tool for LineTool {
    fn input_enabled(&self) -> bool {
        self.input_enabled
    }

    fn set_input_enabled(&mut self, value: bool) {
        self.input_enabled = value;
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        match event.type_ {
            MouseEventType::BeginDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                // start new
                self.line = Some(Line {
                    start: event.pos,
                    direction: None,
                    style: self.style,
                });

                ToolUpdateResult::Redraw
            }
            MouseEventType::EndDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                if let Some(a) = &mut self.line {
                    if event.pos == Vec2D::zero() {
                        self.line = None;

                        ToolUpdateResult::Redraw
                    } else {
                        if event.modifier.intersects(ModifierType::SHIFT_MASK) {
                            a.direction = Some(event.pos.snapped_vector_15deg());
                        } else {
                            a.direction = Some(event.pos);
                        }
                        let result = a.clone_box();
                        self.line = None;

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

                if let Some(r) = &mut self.line {
                    if event.modifier.intersects(ModifierType::SHIFT_MASK) {
                        r.direction = Some(event.pos.snapped_vector_15deg());
                    } else {
                        r.direction = Some(event.pos);
                    }
                    ToolUpdateResult::Redraw
                } else {
                    ToolUpdateResult::Unmodified
                }
            }
            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn handle_key_event(&mut self, event: crate::sketch_board::KeyEventMsg) -> ToolUpdateResult {
        if event.key == Key::Escape && self.line.is_some() {
            self.line = None;
            ToolUpdateResult::Redraw
        } else {
            ToolUpdateResult::Unmodified
        }
    }

    fn handle_style_event(&mut self, style: Style) -> ToolUpdateResult {
        self.style = style;
        ToolUpdateResult::Unmodified
    }

    fn get_drawable(&self) -> Option<&dyn Drawable> {
        match &self.line {
            Some(d) => Some(d),
            None => None,
        }
    }

    fn get_tool_type(&self) -> super::Tools {
        Tools::Line
    }

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>) {
        self.sender = Some(sender);
    }
}
