use anyhow::Result;
use femtovg::{FontId, Path};
use relm4::gtk::gdk::{Key, ModifierType};

use crate::{
    math::Vec2D,
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType},
    style::Style,
};

use super::{Drawable, DrawableClone, Tool, ToolUpdateResult, Tools};

#[derive(Default)]
pub struct LineTool {
    line: Option<Line>,
    style: Style,
    input_enabled: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Line {
    start: Vec2D,
    direction: Option<Vec2D>,
    style: Style,
}

impl Drawable for Line {
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

        canvas.stroke_path(&path, &self.style.into());

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
}
