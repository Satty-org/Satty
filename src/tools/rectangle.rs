use anyhow::Result;
use femtovg::{FontId, Path};
use relm4::{Sender, gtk::gdk::Key};

use crate::{
    configuration::APP_CONFIG,
    math::Vec2D,
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    style::Style,
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
        } else {
            canvas.stroke_path(&path, &self.style.into());
        }
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

    fn handle_key_event(&mut self, event: crate::sketch_board::KeyEventMsg) -> ToolUpdateResult {
        if event.key == Key::Escape && self.rectangle.is_some() {
            self.rectangle = None;
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
