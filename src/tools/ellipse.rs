use anyhow::Result;
use femtovg::{FontId, Path};
use relm4::{Sender, gtk::gdk::Key};

use crate::{
    math::{self, Vec2D},
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    style::Style,
    tools::hit_test_ellipse,
};

use super::{
    Drawable, DrawableClone, Tool, ToolUpdateResult, Tools,
    drag_box::{DragBox, draw_center_marker},
};

#[derive(Clone, Copy, Debug)]
pub struct Ellipse {
    origin: Vec2D,
    // bounding box of the ellipse
    top_left: Vec2D,
    size: Vec2D,
    style: Style,
    centered: bool,
    editing: bool,
}

impl Drawable for Ellipse {
    fn anchors(&self) -> Option<(Vec2D, Vec2D)> {
        Some((self.top_left, self.top_left + self.size))
    }

    fn get_bounding_box(&self) -> Option<(Vec2D, Vec2D)> {
        // the border is always stroked, half of it outside
        let line_width = self
            .style
            .size
            .to_line_width(self.style.annotation_size_factor);
        Some(super::bounding_box_with_margin(
            self.top_left,
            self.top_left + self.size,
            line_width / 2.0,
        ))
    }

    fn get_selection_type(&self) -> super::SelectionType {
        super::SelectionType::Ellipse
    }

    fn hit_test(&self, pos: Vec2D, tolerance: f32) -> bool {
        hit_test_ellipse(pos, self.top_left, self.size, tolerance, self.style.fill)
    }

    fn translate(&mut self, delta: Vec2D) {
        self.top_left += delta;
        self.origin += delta;
    }

    fn resize(&mut self, tl: Vec2D, br: Vec2D, _keep_aspect: bool) {
        let (tl, br) = math::ensure_bounding_box(tl, br);
        self.top_left = tl;
        self.size = br - tl;
        self.centered = false;
        self.editing = false;
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
        let mut path = Path::new();
        let (top_left, size) = math::rect_ensure_positive_size(self.top_left, self.size);
        let (center, radii) = (top_left + size / 2.0, size / 2.0);
        path.ellipse(center.x, center.y, radii.x, radii.y);

        if self.style.fill {
            canvas.fill_path(&path, &self.style.into());
        }
        canvas.stroke_path(&path, &self.style.into());

        if self.editing && self.centered {
            draw_center_marker(canvas, center);
        }

        Ok(())
    }

    fn set_centered(&mut self, centered: bool) {
        self.centered = centered;
    }
    fn set_editing(&mut self, editing: bool) {
        self.editing = editing;
    }
}

impl Ellipse {
    // While drawing, the dragged box is the rectangle whose corners lie on the
    // ellipse, so the ellipse goes through the start point and the cursor.
    fn dragged_box(&self) -> (Vec2D, Vec2D) {
        let (tl, br) = math::rectangle_in_ellipse(self.top_left, self.top_left + self.size);
        (tl, br - tl)
    }

    fn set_dragged_box(&mut self, top_left: Vec2D, size: Vec2D) {
        let (tl, br) = math::ellipse_around_rectangle(top_left, top_left + size);
        self.top_left = tl;
        self.size = br - tl;
    }

    fn calculate_shape(&mut self, sender: &Sender<SketchBoardInput>, event: &MouseEventMsg) {
        let (_, dragged_size) = self.dragged_box();
        let drag_box = DragBox::from_delta(self.origin, dragged_size, event.pos, event.modifier);
        self.centered = drag_box.centered;
        self.set_dragged_box(drag_box.top_left, drag_box.size);
        sender
            .send(SketchBoardInput::ShapeDimensionsUpdate(self.size))
            .ok();
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

    fn handle_key_event(&mut self, event: crate::sketch_board::KeyEventMsg) -> ToolUpdateResult {
        if matches!(event.key, Key::Alt_L | Key::Alt_R)
            && let Some(ellipse) = &mut self.ellipse
        {
            ellipse.centered = true;
            let (_, size) = ellipse.dragged_box();
            ellipse.set_dragged_box(ellipse.origin - size, size * 2.0);

            return ToolUpdateResult::Redraw;
        }

        ToolUpdateResult::Unmodified
    }

    fn handle_key_release_event(
        &mut self,
        event: crate::sketch_board::KeyEventMsg,
    ) -> ToolUpdateResult {
        if matches!(event.key, Key::Alt_L | Key::Alt_R)
            && let Some(ellipse) = &mut self.ellipse
        {
            ellipse.centered = false;
            let (_, size) = ellipse.dragged_box();
            ellipse.set_dragged_box(ellipse.origin, size / 2.0);

            return ToolUpdateResult::Redraw;
        }

        ToolUpdateResult::Unmodified
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
                    top_left: event.pos,
                    size: Vec2D::zero(),
                    style: self.style,
                    centered: false,
                    editing: true,
                });

                ToolUpdateResult::Redraw
            }
            MouseEventType::EndDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                if let Some(ellipse) = &mut self.ellipse {
                    ellipse.editing = false;
                    if event.pos == Vec2D::zero() {
                        self.ellipse = None;
                        ToolUpdateResult::Redraw
                    } else {
                        ellipse.calculate_shape(self.sender.as_ref().unwrap(), &event);
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
                    ellipse.calculate_shape(self.sender.as_ref().unwrap(), &event);
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
        match &self.ellipse {
            Some(d) => Some(d),
            None => None,
        }
    }

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>) {
        self.sender = Some(sender);
    }
}
