use std::cell::RefCell;
use std::f64::consts::PI;
use std::rc::Rc;

use femtovg::{Color, Paint, Path};

use crate::sketch_board::{MouseButton, MouseEventType, SketchBoardInput};
use crate::style::Style;
use crate::{math::Vec2D, sketch_board::MouseEventMsg};

use super::{Drawable, DrawableClone, Tool, ToolUpdateResult, Tools};
use relm4::Sender;

pub struct MarkerTool {
    style: Style,
    next_number: Rc<RefCell<u16>>,
    input_enabled: bool,
    sender: Option<Sender<SketchBoardInput>>,
}

#[derive(Clone, Debug)]
pub struct Marker {
    pos: Vec2D,
    number: u16,
    style: Style,
    tool_next_number: Rc<RefCell<u16>>,
}

impl Drawable for Marker {
    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        font: femtovg::FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> anyhow::Result<()> {
        let text = format!("{}", self.number);

        let marker_color: Color = self.style.color.into();
        // https://en.wikipedia.org/wiki/Luma_(video)
        let luminance = 0.2126 * marker_color.r + 0.7152 * marker_color.g + 0.0722 * marker_color.b;
        let text_color = if luminance > 0.5 {
            Color::black()
        } else {
            Color::white()
        };

        let mut paint = Paint::color(text_color);

        paint.set_font(&[font]);
        paint.set_font_size(
            (self
                .style
                .size
                .to_text_size(self.style.annotation_size_factor)) as f32,
        );
        paint.set_text_align(femtovg::Align::Center);
        paint.set_text_baseline(femtovg::Baseline::Middle);

        let text_metrics = canvas.measure_text(self.pos.x, self.pos.y, &text, &paint)?;

        let circle_radius = (text_metrics.width() * text_metrics.width()
            + text_metrics.height() * text_metrics.height())
        .sqrt();

        let mut inner_circle_path = Path::new();
        inner_circle_path.arc(
            self.pos.x,
            self.pos.y,
            circle_radius * 0.8,
            0.0,
            2.0 * PI as f32,
            femtovg::Solidity::Solid,
        );

        let mut outer_circle_path = Path::new();
        outer_circle_path.arc(
            self.pos.x,
            self.pos.y,
            circle_radius,
            0.0,
            2.0 * PI as f32,
            femtovg::Solidity::Solid,
        );

        let circle_paint = Paint::color(marker_color).with_line_width(
            self.style
                .size
                .to_line_width(self.style.annotation_size_factor)
                * 2.0,
        );

        canvas.save();
        canvas.fill_path(&inner_circle_path, &circle_paint);
        canvas.stroke_path(&outer_circle_path, &circle_paint);
        canvas.fill_text(self.pos.x, self.pos.y, &text, &paint)?;
        canvas.restore();
        Ok(())
    }

    fn handle_undo(&mut self) {
        *self.tool_next_number.borrow_mut() = self.number;
    }

    fn handle_redo(&mut self) {
        *self.tool_next_number.borrow_mut() = self.number + 1;
    }
}

impl Tool for MarkerTool {
    fn input_enabled(&self) -> bool {
        self.input_enabled
    }

    fn set_input_enabled(&mut self, value: bool) {
        self.input_enabled = value;
    }

    fn get_tool_type(&self) -> super::Tools {
        Tools::Marker
    }

    fn get_drawable(&self) -> Option<&dyn Drawable> {
        None
    }

    fn handle_style_event(&mut self, style: Style) -> ToolUpdateResult {
        self.style = style;
        ToolUpdateResult::Unmodified
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        match event.type_ {
            MouseEventType::Click => {
                if event.button == MouseButton::Primary {
                    let marker = Marker {
                        pos: event.pos,
                        number: *self.next_number.borrow(),
                        style: self.style,
                        tool_next_number: self.next_number.clone(),
                    };

                    // increment for next
                    *self.next_number.borrow_mut() += 1;

                    ToolUpdateResult::Commit(marker.clone_box())
                } else {
                    ToolUpdateResult::Unmodified
                }
            }
            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>) {
        self.sender = Some(sender);
    }
}

impl Default for MarkerTool {
    fn default() -> Self {
        Self {
            style: Default::default(),
            next_number: Rc::new(RefCell::new(1)),
            input_enabled: true,
            sender: None,
        }
    }
}
