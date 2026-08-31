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
    pub keep_aspect: bool,
}

impl DragBox {
    pub fn from_origin_delta(
        origin: Vec2D,
        orig_size: Vec2D,
        event: &MouseEventMsg,
        sender: &Sender<SketchBoardInput>,
    ) -> Self {
        let aspect = event.modifier.intersects(ModifierType::SHIFT_MASK);
        let keep_aspect = event.modifier.intersects(ModifierType::CONTROL_MASK);
        let centered = event.modifier.intersects(ModifierType::ALT_MASK);

        let mut new_size = event.pos;
        let sign_x = new_size.x.signum();
        let sign_y = new_size.y.signum();
        let width = new_size.x.abs();
        let height = new_size.y.abs();

        if keep_aspect {
            let aspect_ratio =
                if orig_size.x.abs() <= f32::EPSILON || orig_size.y.abs() <= f32::EPSILON {
                    // start with square
                    1.0
                } else {
                    orig_size.x.abs() / orig_size.y.abs()
                };
            if width >= height * aspect_ratio {
                new_size.y = width / aspect_ratio * sign_y;
            } else {
                new_size.x = height * aspect_ratio * sign_x;
            }
        } else if aspect && new_size.y.abs() > f32::EPSILON {
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
            new_size.x = width * sign_x;
            new_size.y = height * sign_y;
        }

        let size_factor = if centered { 2.0 } else { 1.0 };
        new_size = new_size * size_factor;

        let top_left = if centered {
            origin.min(origin - new_size.abs() / size_factor)
        } else {
            origin.min(origin + new_size)
        };

        let drag_box = Self {
            top_left,
            size: new_size.abs(),
            centered,
            keep_aspect: aspect || keep_aspect,
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
