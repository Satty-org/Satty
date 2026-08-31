use femtovg::{Color, Paint, Path};
use relm4::Sender;
use relm4::gtk::gdk::ModifierType;

use crate::{
    configuration::APP_CONFIG,
    math::{Vec2D, get_closest_aspect_ratio},
    sketch_board::{MouseEventMsg, SketchBoardInput},
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DragBox {
    pub top_left: Vec2D,
    pub size: Vec2D,
    pub centered: bool,
}

impl DragBox {
    pub fn from_origin_delta(
        origin: Vec2D,
        event: &MouseEventMsg,
        sender: &Sender<SketchBoardInput>,
    ) -> Self {
        let centered = event.modifier.intersects(ModifierType::ALT_MASK);
        let aspect = event.modifier.intersects(ModifierType::SHIFT_MASK);

        let mut size = event.pos;

        if aspect && size.y.abs() > f32::EPSILON {
            let sign_x = size.x.signum();
            let sign_y = size.y.signum();
            let width = size.x.abs();
            let height = size.y.abs();
            let aspect_ratio = width / height;
            let config = APP_CONFIG.read();
            let closest_aspect_ratio =
                get_closest_aspect_ratio(aspect_ratio, config.aspect_ratios());
            let aspect_ratio = closest_aspect_ratio.0 / closest_aspect_ratio.1;
            let (width, height) = if height > width / aspect_ratio {
                (height * aspect_ratio, height)
            } else {
                (width, width / aspect_ratio)
            };
            size.x = width * sign_x;
            size.y = height * sign_y;
        }

        let size_factor = if centered { 2.0 } else { 1.0 };
        size = size * size_factor;

        let top_left = if centered {
            origin.min(origin - size.abs() / size_factor)
        } else {
            origin.min(origin + size)
        };

        let drag_box = Self {
            top_left,
            size: size.abs(),
            centered,
        };
        sender
            .send(SketchBoardInput::ShapeDimensionsUpdate(drag_box.size))
            .ok();
        drag_box
    }

    pub fn middle(&self) -> Vec2D {
        self.top_left + self.size * 0.5
    }
}

pub fn draw_center_marker(canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>, center: Vec2D) {
    let mut helpers = Path::new();
    helpers.circle(center.x, center.y, 2.0);
    let paint = Paint::color(Color::rgba(128, 128, 128, 255)).with_line_width(1.0);
    canvas.stroke_path(&helpers, &paint);
}
