use anyhow::Result;
use femtovg::{FontId, Path};
use relm4::Sender;

use crate::{
    configuration::APP_CONFIG,
    math::{self, Vec2D},
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    style::Style,
    tools::hit_test_rectangle,
};

use super::{
    Drawable, DrawableClone, Tool, ToolUpdateResult, Tools,
    drag_box::{DragBox, draw_center_marker},
};

#[derive(Clone, Copy, Debug)]
pub struct Rectangle {
    origin: Vec2D,
    top_left: Vec2D,
    size: Option<Vec2D>,
    style: Style,
    centered: bool,
    finishing: bool,
}

impl Drawable for Rectangle {
    fn bounds(&self) -> Option<(Vec2D, Vec2D)> {
        let size = self.size?;
        Some(math::ensure_bounding_box(
            self.top_left,
            self.top_left + size,
        ))
    }

    fn hit_test(&self, pos: Vec2D, tolerance: f32) -> bool {
        hit_test_rectangle(pos, self.top_left, self.size, tolerance, self.style.fill)
    }

    fn translate(&mut self, delta: Vec2D) {
        self.top_left += delta;
        self.origin += delta;
    }

    fn resize_bounds(&mut self, tl: Vec2D, br: Vec2D) {
        let (tl, br) = math::ensure_bounding_box(tl, br);
        self.top_left = tl;
        self.size = Some(br - tl);
        self.origin = tl;
        self.centered = false;
        self.finishing = true;
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
    ) -> Result<()> {
        let size = match self.size {
            Some(s) => s,
            None => return Ok(()), // early exit if none
        };

        canvas.save();
        let mut path = Path::new();
        path.rounded_rect(
            self.top_left.x,
            self.top_left.y,
            size.x,
            size.y,
            APP_CONFIG.read().corner_roundness(),
        );

        if !self.finishing && self.centered {
            draw_center_marker(canvas, self.origin);
        }

        if self.style.fill {
            canvas.fill_path(&path, &self.style.into());
        }
        canvas.stroke_path(&path, &self.style.into());
        canvas.restore();

        Ok(())
    }
}

impl Rectangle {
    fn calculate_shape(&mut self, event: &MouseEventMsg) {
        let drag_box = DragBox::from_origin_delta(self.origin, event.pos, event.modifier);
        self.centered = drag_box.centered;
        self.top_left = drag_box.top_left;
        self.size = Some(drag_box.size);
    }
}

#[derive(Default)]
pub struct RectangleTool {
    rectangle: Option<Rectangle>,
    style: Style,
    input_enabled: bool,
    sender: Option<Sender<SketchBoardInput>>,
}

impl Tool for RectangleTool {
    fn input_enabled(&self) -> bool {
        self.input_enabled
    }

    fn set_input_enabled(&mut self, value: bool) {
        self.input_enabled = value;
    }

    fn active(&self) -> bool {
        self.rectangle.is_some()
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        match event.type_ {
            MouseEventType::BeginDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }
                // start new
                self.rectangle = Some(Rectangle {
                    origin: event.pos,
                    top_left: event.pos,
                    size: None,
                    style: self.style,
                    centered: false,
                    finishing: false,
                });

                ToolUpdateResult::Redraw
            }
            MouseEventType::EndDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                if let Some(rectangle) = &mut self.rectangle {
                    rectangle.finishing = true;
                    if event.pos == Vec2D::zero() {
                        self.rectangle = None;
                        ToolUpdateResult::Redraw
                    } else {
                        rectangle.calculate_shape(&event);
                        let result = rectangle.clone_box();
                        self.rectangle = None;
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

                if let Some(rectangle) = &mut self.rectangle {
                    if event.pos == Vec2D::zero() {
                        return ToolUpdateResult::Unmodified;
                    }
                    rectangle.calculate_shape(&event);
                    ToolUpdateResult::Redraw
                } else {
                    ToolUpdateResult::Unmodified
                }
            }
            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn handle_style_event(&mut self, style: Style) -> ToolUpdateResult {
        self.style = style;
        ToolUpdateResult::Unmodified
    }

    fn get_drawable(&self) -> Option<&dyn Drawable> {
        match &self.rectangle {
            Some(d) => Some(d),
            None => None,
        }
    }

    fn get_tool_type(&self) -> super::Tools {
        Tools::Rectangle
    }

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>) {
        self.sender = Some(sender);
    }
}
