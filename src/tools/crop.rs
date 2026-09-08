use super::{
    Drawable, DrawableClone, Tool, ToolUpdateResult, Tools,
    drag_box::{DragBox, draw_center_marker},
};
use crate::{
    math::{self, Vec2D},
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    tools::{RenderingMode, hit_test_rectangle},
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
    finishing: bool,
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
    pub fn calculate_shape(&mut self, event: &MouseEventMsg) {
        let drag_box = DragBox::from_origin_delta(self.origin, event.pos, event.modifier);
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
        hit_test_rectangle(pos, self.top_left, Some(self.size), tolerance, false)
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
        if !self.finishing && self.centered {
            draw_center_marker(canvas, self.origin);
        }

        let size = self.size;

        let shadow_paint = Paint::color(Color::rgbaf(0.0, 0.0, 0.0, 0.5))
            .with_fill_rule(femtovg::FillRule::EvenOdd);
        let (img_tl, img_br) = bounds;
        let img_size = img_br - img_tl;
        let mut shadow_path = Path::new();
        shadow_path.rect(img_tl.x, img_tl.y, img_size.x, img_size.y);
        shadow_path.rect(self.top_left.x, self.top_left.y, size.x, size.y);

        let border_paint = Paint::color(Color::rgbf(0.1, 0.1, 0.1)).with_line_width(2.0);
        let mut border_path = Path::new();
        border_path.rect(self.top_left.x, self.top_left.y, size.x, size.y);

        canvas.save();
        canvas.fill_path(&shadow_path, &shadow_paint);
        canvas.stroke_path(&border_path, &border_paint);

        canvas.restore();
        Ok(())
    }
}

impl CropTool {
    fn emit_crop_dimensions_update(&self) {
        if let (Some(crop), Some(sender)) = (&self.crop, &self.sender)
            && let Some((tl, br)) = crop.bounds()
        {
            sender
                .send(SketchBoardInput::CropDimensionsUpdate((tl, br - tl)))
                .ok();
        }
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
                    finishing: false,
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
                    self.emit_crop_dimensions_update();
                    return ToolUpdateResult::Redraw;
                }
                crop.finishing = true;
                crop.calculate_shape(&event);
                ToolUpdateResult::Commit(crop.clone_box())
            }
            MouseEventType::UpdateDrag if event.button == MouseButton::Primary => {
                if event.pos == Vec2D::zero() {
                    return ToolUpdateResult::Unmodified;
                }
                let Some(crop) = &mut self.crop else {
                    return ToolUpdateResult::Unmodified;
                };
                crop.calculate_shape(&event);
                self.emit_crop_dimensions_update();
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
