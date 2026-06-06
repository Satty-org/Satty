use std::cell::RefCell;
use std::f64::consts::PI;
use std::rc::Rc;

use femtovg::{Color, Paint, Path};
use relm4::gtk::gdk::{Key, ModifierType};

use crate::sketch_board::{KeyEventMsg, MouseButton, MouseEventType, SketchBoardInput};
use crate::style::Style;
use crate::{math::Vec2D, sketch_board::MouseEventMsg};

use super::{Drawable, DrawableClone, Tool, ToolUpdateResult, Tools};
use relm4::Sender;

pub struct MarkerTool {
    marker: Option<Marker>,
    origin: Vec2D,
    style: Style,
    next_number: Rc<RefCell<u16>>,
    input_enabled: bool,
    sender: Option<Sender<SketchBoardInput>>,
}

#[derive(Clone, Debug)]
pub struct Marker {
    pos: Vec2D,
    number: u16,
    extra_ring: bool,
    style: Style,
    tool_next_number: Rc<RefCell<u16>>,
}

impl Marker {
    fn get_line_width(&self) -> f32 {
        self.style
            .size
            .to_line_width(self.style.annotation_size_factor)
    }
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

        let pos = self.pos;
        // avoid size jitter due to small metric differences between numbers by using "77" for 1 to 99
        let text_for_metric = format!("{}", if self.number < 100 { 77 } else { self.number });
        let text_metrics = canvas.measure_text(pos.x, pos.y, &text_for_metric, &paint)?;
        let line_width = self.get_line_width();
        let circle_radius = text_metrics.width() * 0.5 + line_width * 1.5;

        let mut inner_circle_path = Path::new();
        inner_circle_path.arc(
            pos.x,
            pos.y,
            circle_radius,
            0.0,
            2.0 * PI as f32,
            femtovg::Solidity::Solid,
        );

        let circle_paint = Paint::color(marker_color).with_line_width(line_width);

        canvas.save();

        canvas.fill_path(&inner_circle_path, &circle_paint);
        canvas.stroke_path(&inner_circle_path, &circle_paint);

        if self.extra_ring {
            let mut outer_ring_path = Path::new();
            outer_ring_path.arc(
                pos.x,
                pos.y,
                circle_radius + line_width * 2.0,
                0.0,
                2.0 * PI as f32,
                femtovg::Solidity::Solid,
            );

            canvas.stroke_path(&outer_ring_path, &circle_paint);
        }

        canvas.fill_text(pos.x, pos.y, &text, &paint)?;
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

impl MarkerTool {
    fn handle_alt_key_event(&mut self, event: KeyEventMsg, pressed: bool) -> ToolUpdateResult {
        if let Some(marker) = &mut self.marker
            && (event.key == Key::Alt_L || event.key == Key::Alt_R)
        {
            marker.extra_ring = pressed;
            return ToolUpdateResult::RedrawAndStopPropagation;
        }
        ToolUpdateResult::Unmodified
    }
}

impl Tool for MarkerTool {
    fn input_enabled(&self) -> bool {
        self.input_enabled
    }

    fn set_input_enabled(&mut self, value: bool) {
        self.input_enabled = value;
    }

    fn active(&self) -> bool {
        self.marker.is_some()
    }

    fn get_tool_type(&self) -> super::Tools {
        Tools::Marker
    }

    fn get_drawable(&self) -> Option<&dyn Drawable> {
        match &self.marker {
            Some(marker) => Some(marker),
            None => None,
        }
    }

    fn handle_style_event(&mut self, style: Style) -> ToolUpdateResult {
        self.style = style;
        ToolUpdateResult::Unmodified
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        if event.button != MouseButton::Primary {
            return ToolUpdateResult::Unmodified;
        }
        match event.type_ {
            MouseEventType::Click => {
                self.origin = event.pos;
                self.marker = Some(Marker {
                    pos: event.pos,
                    number: *self.next_number.borrow(),
                    style: self.style,
                    tool_next_number: self.next_number.clone(),
                    extra_ring: event.modifier.contains(ModifierType::ALT_MASK),
                });
                ToolUpdateResult::Redraw
            }
            MouseEventType::UpdateDrag => {
                if let Some(marker) = &mut self.marker {
                    marker.pos = self.origin + event.pos;
                    ToolUpdateResult::Redraw
                } else {
                    ToolUpdateResult::Unmodified
                }
            }
            MouseEventType::Release => {
                *self.next_number.borrow_mut() += 1;
                if let Some(marker) = &mut self.marker.take() {
                    let result = ToolUpdateResult::Commit(marker.clone_box());
                    self.marker = None;
                    result
                } else {
                    ToolUpdateResult::Unmodified
                }
            }
            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn handle_key_event(&mut self, event: KeyEventMsg) -> ToolUpdateResult {
        self.handle_alt_key_event(event, true)
    }

    fn handle_key_release_event(&mut self, event: KeyEventMsg) -> ToolUpdateResult {
        self.handle_alt_key_event(event, false)
    }

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>) {
        self.sender = Some(sender);
    }
}

impl Default for MarkerTool {
    fn default() -> Self {
        Self {
            marker: None,
            origin: Vec2D::zero(),
            style: Default::default(),
            next_number: Rc::new(RefCell::new(1)),
            input_enabled: true,
            sender: None,
        }
    }
}
