use anyhow::Result;
use femtovg::{FontId, Paint, Path};
use relm4::Sender;

use crate::{
    math::Vec2D,
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    style::Style,
};

use super::{Drawable, DrawableClone, Straight, Tool, ToolUpdateResult, Tools};

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
    end: Vec2D,
    style: Style,
}

impl Drawable for Line {
    fn anchors(&self) -> Option<(Vec2D, Vec2D)> {
        Some((self.start, self.end))
    }

    fn get_bounding_box(&self) -> Option<(Vec2D, Vec2D)> {
        let line_width = self
            .style
            .size
            .to_line_width(self.style.annotation_size_factor);
        Some(super::bounding_box_with_margin(
            self.start,
            self.end,
            line_width / 2.0,
        ))
    }

    fn get_selection_type(&self) -> super::SelectionType {
        super::SelectionType::Line
    }

    fn hit_test(&self, pos: Vec2D, tolerance: f32) -> bool {
        let line_width = self
            .style
            .size
            .to_line_width(self.style.annotation_size_factor);
        pos.distance_to_segment(self.start, self.end) <= tolerance + line_width / 2.0
    }

    fn translate(&mut self, delta: Vec2D) {
        self.start += delta;
        self.end += delta;
    }

    fn resize(&mut self, tl: Vec2D, br: Vec2D, _keep_aspect: bool) {
        self.start = tl;
        self.end = br;
    }

    fn get_style(&self) -> Option<&Style> {
        Some(&self.style)
    }

    fn set_style(&mut self, style: Style) {
        self.style = style;
    }

    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        let mut path = Path::new();
        path.move_to(self.start.x, self.start.y);
        let end = if self.start == self.end {
            // also before the first move, when there are no points yet
            // draw a point at start - a little bit hacky :-)
            self.end + Vec2D::new(0.01, 0.0)
        } else {
            self.end
        };
        path.line_to(end.x, end.y);
        let paint: Paint = self.style.into();
        canvas.stroke_path(&path, &paint);

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
                    end: event.pos,
                    style: self.style,
                });

                ToolUpdateResult::Redraw
            }
            MouseEventType::EndDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                if let Some(line) = &mut self.line {
                    if event.pos == Vec2D::zero() {
                        self.line = None;

                        ToolUpdateResult::Redraw
                    } else {
                        line.end = line.start + Straight::apply(event.modifier, event.pos);
                        let result = line.clone_box();
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

                if let Some(line) = &mut self.line {
                    line.end = line.start + Straight::apply(event.modifier, event.pos);
                    if let Some(sender) = &self.sender {
                        sender
                            .send(SketchBoardInput::LineDimensionsUpdate(
                                line.end - line.start,
                            ))
                            .ok();
                    }
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
