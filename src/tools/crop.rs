use super::{
    Drawable, DrawableClone, Tool, ToolUpdateResult, Tools,
    drag_box::{DragBox, draw_center_marker},
};
use crate::{
    math::{self, Vec2D},
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    tools::{RenderingMode, drag_box::draw_rect_marker, hit_test_rectangle},
};
use anyhow::Result;
use femtovg::{Color, Paint, Path};
use relm4::Sender;

#[derive(Debug, Clone, Copy)]
pub struct Crop {
    origin: Vec2D,
    top_left: Vec2D,
    size: Vec2D,
    centered: bool,
    editing: bool,
    active: bool,
}

#[derive(Default)]
pub struct CropTool {
    crop: Option<Crop>,
    dragging: bool,
    input_enabled: bool,
    sender: Option<Sender<SketchBoardInput>>,
}

impl Crop {
    pub fn calculate_shape(&mut self, sender: &Sender<SketchBoardInput>, event: &MouseEventMsg) {
        let drag_box = DragBox::from_origin_delta(self.origin, self.size, event, sender);
        self.centered = drag_box.centered;
        self.top_left = drag_box.top_left;
        self.size = drag_box.size;
    }
}

impl Drawable for Crop {
    fn get_rendering_mode(&self) -> RenderingMode {
        RenderingMode::Crop
    }

    fn bounds(&self) -> Option<(Vec2D, Vec2D)> {
        Some(math::ensure_bounding_box(
            self.top_left,
            self.top_left + self.size,
        ))
    }

    fn hit_test(&self, pos: Vec2D, tolerance: f32) -> bool {
        hit_test_rectangle(pos, self.top_left, self.size, tolerance, false)
    }

    fn translate(&mut self, delta: Vec2D) {
        self.top_left += delta;
    }

    fn resize_bounds(&mut self, tl: Vec2D, br: Vec2D) {
        let (tl, br) = math::ensure_bounding_box(tl, br);
        self.top_left = tl;
        self.size = br - tl;
    }

    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: femtovg::FontId,
        bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        let shadow_paint = Paint::color(Color::rgbaf(
            0.0,
            0.0,
            0.0,
            if self.editing { 0.5 } else { 0.9 },
        ))
        .with_fill_rule(femtovg::FillRule::EvenOdd);

        let (img_tl, img_br) = bounds;
        // increase it a bit as otherwise subpixel of the image will still be visible
        let shadow_tl = (img_tl).min(self.top_left) - 1.0;
        let shadow_br = (img_br).max(self.top_left + self.size);
        let shadow_size = shadow_br - shadow_tl + 2.0;
        let mut shadow_path = Path::new();
        // the outer rectangle of the shadow
        shadow_path.rect(shadow_tl.x, shadow_tl.y, shadow_size.x, shadow_size.y);
        let tl = self.top_left;
        let size = self.size;
        // the inner rectangle of the shadow
        shadow_path.rect(tl.x, tl.y, size.x, size.y);

        canvas.fill_path(&shadow_path, &shadow_paint);

        if self.editing && self.centered {
            draw_center_marker(canvas, self.origin);
        }

        draw_rect_marker(canvas, tl, size, false);

        Ok(())
    }

    fn set_centered(&mut self, centered: bool) {
        self.centered = centered;
        self.origin = self.top_left + self.size / 2.0;
    }
    fn set_editing(&mut self, editing: bool) {
        self.editing = editing;
    }
}

impl Tool for CropTool {
    fn active(&self) -> bool {
        if let Some(c) = &self.crop {
            c.active
        } else {
            false
        }
    }

    fn input_enabled(&self) -> bool {
        self.input_enabled
    }

    fn set_input_enabled(&mut self, value: bool) {
        self.input_enabled = value;
    }

    fn get_tool_type(&self) -> super::Tools {
        Tools::Crop
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        match event.type_ {
            MouseEventType::BeginDrag if event.button == MouseButton::Primary => {
                self.dragging = true;
                self.crop = Some(Crop {
                    origin: event.pos,
                    top_left: event.pos,
                    size: Vec2D::zero(),
                    centered: false,
                    editing: true,
                    active: true,
                });
                ToolUpdateResult::Redraw
            }
            MouseEventType::EndDrag if event.button == MouseButton::Primary => {
                self.dragging = false;
                let Some(crop) = &mut self.crop else {
                    return ToolUpdateResult::Unmodified;
                };
                if crop.size == Vec2D::zero() {
                    self.crop = None;
                    return ToolUpdateResult::Redraw;
                }
                crop.editing = false;
                crop.calculate_shape(self.sender.as_ref().unwrap(), &event);
                ToolUpdateResult::Commit(crop.clone_box())
            }
            MouseEventType::UpdateDrag if event.button == MouseButton::Primary => {
                if event.pos == Vec2D::zero() {
                    return ToolUpdateResult::Unmodified;
                }
                let Some(crop) = &mut self.crop else {
                    return ToolUpdateResult::Unmodified;
                };
                crop.calculate_shape(self.sender.as_ref().unwrap(), &event);
                ToolUpdateResult::Redraw
            }
            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn get_drawable(&self) -> Option<&dyn Drawable> {
        if self.dragging {
            self.crop.as_ref().map(|crop| crop as &dyn Drawable)
        } else {
            None
        }
    }

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>) {
        self.sender = Some(sender);
    }
}
