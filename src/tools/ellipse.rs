use anyhow::Result;
use femtovg::{FontId, Path};
use relm4::{
    gtk::gdk::{Key, ModifierType},
    Sender,
};

use crate::{
    math::Vec2D,
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    style::Style,
};

use super::{Drawable, DrawableClone, Tool, ToolUpdateResult, Tools};

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

        if !self.finishing {
            let mut helpers = Path::new();
            if self.centered {
                helpers.circle(self.middle.x, self.middle.y, 2.0);
            } else {
                helpers.rect(self.origin.x, self.origin.y, radii.x * 2.0, radii.y * 2.0);
            }
            canvas.stroke_path(
                &helpers,
                &femtovg::Paint::color(femtovg::Color::rgba(128, 128, 128, 255))
                    .with_line_width(2.0), //TODO: hardcoding this is no good if we use this in more places
            );
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
        self.centered = event.modifier & ModifierType::ALT_MASK == ModifierType::ALT_MASK;
        match event.modifier & (ModifierType::ALT_MASK | ModifierType::SHIFT_MASK) {
            v if v == ModifierType::ALT_MASK | ModifierType::SHIFT_MASK => {
                self.middle = self.origin;
                let max_size = event.pos.x.abs().max(event.pos.y.abs());
                self.radii = Some(Vec2D {
                    x: max_size * event.pos.x.signum(),
                    y: max_size * event.pos.y.signum(),
                });
            }
            ModifierType::ALT_MASK => {
                self.middle = self.origin;
                self.radii = Some(event.pos);
            }
            ModifierType::SHIFT_MASK => {
                let max_size = (event.pos.x / 2.0).abs().max((event.pos.y / 2.0).abs());
                self.radii = Some(Vec2D {
                    x: max_size * event.pos.x.signum(),
                    y: max_size * event.pos.y.signum(),
                });
                self.middle.x = self.origin.x + max_size * event.pos.x.signum();
                self.middle.y = self.origin.y + max_size * event.pos.y.signum();
            }
            _ => {
                self.radii = Some(Vec2D {
                    x: event.pos.x / 2.0,
                    y: event.pos.y / 2.0,
                });
                self.middle.x = self.origin.x + event.pos.x / 2.0;
                self.middle.y = self.origin.y + event.pos.y / 2.0;
            }
        }
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
