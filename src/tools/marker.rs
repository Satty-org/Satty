use std::cell::{Cell, RefCell};
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
    // for bounding box cache circle radius from the last draw
    radius: Cell<f32>,
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
    fn bounds(&self) -> Option<(Vec2D, Vec2D)> {
        let r = self.radius.get() + self.get_line_width() * if self.extra_ring { 2.0 } else { 0.0 };
        let r = Vec2D::new(r, r);
        Some((self.pos - r, self.pos + r))
    }

    fn hit_test(&self, pos: Vec2D, tolerance: f32) -> bool {
        let r = self.radius.get() + self.get_line_width() * if self.extra_ring { 2.0 } else { 0.0 };
        let d = (pos - self.pos) / (r + tolerance);
        d * d <= 1.0
    }

    fn translate(&mut self, delta: Vec2D) {
        self.pos += delta;
    }

    fn resize_bounds(&mut self, tl: Vec2D, br: Vec2D) {
        let Some((old_tl, old_br)) = self.bounds() else {
            return;
        };

        // Marker resize handles are semantic controls rather than geometric resize:
        // left/right adjust number, vertical drag toggles extra ring.
        let delta_left = tl.x - old_tl.x;
        let delta_right = br.x - old_br.x;

        const NUMBER_PX_THRESHOLD: f32 = 11.0;
        let left_steps = (delta_left / NUMBER_PX_THRESHOLD).floor();
        let right_steps = (delta_right / NUMBER_PX_THRESHOLD).floor();
        let delta_steps = if left_steps.abs() < right_steps.abs() {
            right_steps
        } else {
            left_steps
        };
        let new_number = self.number.saturating_add_signed(delta_steps as i16).max(1);
        self.number = new_number;

        let delta_top = tl.y - old_tl.y;
        let delta_bottom = br.y - old_br.y;
        let ring_offset = self.get_line_width();

        if delta_top <= -ring_offset || delta_bottom >= ring_offset {
            self.extra_ring = true;
        } else if delta_top > ring_offset || delta_bottom < -ring_offset {
            self.extra_ring = false;
        }
    }

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

        self.radius
            .set(circle_radius + self.style.annotation_size_factor);

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

    fn handle_reset(&mut self) {
        *self.next_number.borrow_mut() = 1;
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        if event.button != MouseButton::Primary {
            return ToolUpdateResult::Unmodified;
        }
        let font_size = self
            .style
            .size
            .to_text_size(self.style.annotation_size_factor) as f32;
        let extra_ring = event.modifier.contains(ModifierType::ALT_MASK);

        match event.type_ {
            MouseEventType::Click => {
                self.origin = event.pos;
                self.marker = Some(Marker {
                    pos: event.pos,
                    number: *self.next_number.borrow(),
                    style: self.style,
                    radius: Cell::new(font_size),
                    tool_next_number: self.next_number.clone(),
                    extra_ring,
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
                if let Some(marker) = &mut self.marker.take() {
                    let result = ToolUpdateResult::Commit(marker.clone_box());
                    self.marker = None;
                    // increment for next
                    *self.next_number.borrow_mut() += 1;
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
