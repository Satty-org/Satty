use anyhow::Result;
use femtovg::{FontId, Path};
use relm4::{
    Sender,
    gtk::gdk::{Key, ModifierType},
};

use crate::{
    math::{self, SegmentResizeTarget, Vec2D},
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    style::Style,
};

use super::{Drawable, DrawableClone, Tool, ToolUpdateResult, Tools};

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
    fn bounds(&self) -> Option<(Vec2D, Vec2D)> {
        let dir = self.direction?;
        let end = self.start + dir;
        Some(math::ensure_bounding_box(self.start, end))
    }

    fn hit_test(&self, pos: Vec2D, tolerance: f32) -> bool {
        let Some(dir) = self.direction else {
            return false;
        };
        let end = self.start + dir;
        pos.distance_to_segment(self.start, end) <= tolerance
    }

    fn translate(&mut self, delta: Vec2D) {
        self.start += delta;
    }

    fn resize_bounds(&mut self, _tl: Vec2D, _br: Vec2D, _delta: Vec2D, _keep_aspect: bool) {
        panic!("should never be called");
    }

    fn resize_bounds_with_segment_target(
        &mut self,
        tl: Vec2D,
        br: Vec2D,
        delta: Vec2D,
        keep_aspect: bool,
        target: Option<SegmentResizeTarget>,
    ) {
        if let Some(direction) = self.direction {
            let end = self.start + direction;
            let segment = (self.start, end);
            let bounding_box = (tl, br);
            let (new_start, new_end) =
                math::resize_segment(segment, bounding_box, delta, keep_aspect, target);

            self.start = new_start;
            self.direction = Some(new_end - new_start);
        } else {
            self.start = tl;
            self.direction = Some(br - tl);
        }
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

    fn active(&self) -> bool {
        self.line.is_some()
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
