use femtovg::{Color, Paint, Path};
use relm4::gtk::gdk::ModifierType;

use crate::math::Vec2D;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DragBox {
    pub top_left: Vec2D,
    pub size: Vec2D,
    pub centered: bool,
}

impl DragBox {
    pub fn from_origin_delta(origin: Vec2D, delta: Vec2D, modifier: ModifierType) -> Self {
        let centered = modifier.intersects(ModifierType::ALT_MASK);
        let uniform = modifier.intersects(ModifierType::SHIFT_MASK);

        let size = if uniform {
            let max_size = delta.x.abs().max(delta.y.abs());
            Vec2D::new(max_size * delta.x.signum(), max_size * delta.y.signum())
        } else {
            delta
        };

        let top_left = if centered {
            origin - size * 0.5
        } else {
            origin.min(origin + size)
        };

        Self {
            top_left,
            size: size.abs(),
            centered,
        }
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
