use anyhow::Result;
use femtovg::{FontId, Path};
use relm4::{Sender, gtk::gdk::Key};

use crate::{
    math::Vec2D,
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    style::Style,
};

use super::{
    Drawable, DrawableClone, Tool, ToolUpdateResult, Tools,
    drag_box::{DragBox, draw_center_marker},
};

#[derive(Clone, Copy, Debug)]
pub struct Ellipse {
    origin: Vec2D,
    middle: Vec2D,
    radii: Option<Vec2D>,
    style: Style,
    centered: bool,
    finishing: bool,
}

impl Drawable for Ellipse {
    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        let radii = match self.radii {
            Some(s) => s,
            None => return Ok(()), // early exit if none
        };

        canvas.save();
        let mut path = Path::new();
        path.ellipse(self.middle.x, self.middle.y, radii.x, radii.y);

        if !self.finishing && self.centered {
            draw_center_marker(canvas, self.middle);
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

impl Ellipse {
    fn calculate_shape(&mut self, event: &MouseEventMsg) {
        let drag_box = DragBox::from_origin_delta(self.origin, event.pos, event.modifier);
        self.centered = drag_box.centered;
        self.middle = drag_box.middle();
        self.radii = Some(drag_box.size * 0.5);
    }
}

#[derive(Default)]
pub struct EllipseTool {
    ellipse: Option<Ellipse>,
    style: Style,
    input_enabled: bool,
    sender: Option<Sender<SketchBoardInput>>,
}

impl Tool for EllipseTool {
    fn input_enabled(&self) -> bool {
        self.input_enabled
    }

    fn set_input_enabled(&mut self, value: bool) {
        self.input_enabled = value;
    }

    fn active(&self) -> bool {
        self.ellipse.is_some()
    }

    fn get_tool_type(&self) -> super::Tools {
        Tools::Ellipse
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        match event.type_ {
            MouseEventType::BeginDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                // start new
                self.ellipse = Some(Ellipse {
                    origin: event.pos,
                    middle: event.pos,
                    radii: None,
                    style: self.style,
                    centered: true,
                    finishing: false,
                });

                ToolUpdateResult::Redraw
            }
            MouseEventType::EndDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                if let Some(ellipse) = &mut self.ellipse {
                    ellipse.finishing = true;
                    if event.pos == Vec2D::zero() {
                        self.ellipse = None;

                        ToolUpdateResult::Redraw
                    } else {
                        ellipse.calculate_shape(&event);
                        let result = ellipse.clone_box();
                        self.ellipse = None;
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

                if let Some(ellipse) = &mut self.ellipse {
                    if event.pos == Vec2D::zero() {
                        return ToolUpdateResult::Unmodified;
                    }
                    ellipse.calculate_shape(&event);
                    ToolUpdateResult::Redraw
                } else {
                    ToolUpdateResult::Unmodified
                }
            }
            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn handle_key_event(&mut self, event: crate::sketch_board::KeyEventMsg) -> ToolUpdateResult {
        if event.key == Key::Escape && self.ellipse.is_some() {
            self.ellipse = None;
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
        match &self.ellipse {
            Some(d) => Some(d),
            None => None,
        }
    }

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>) {
        self.sender = Some(sender);
    }
}
