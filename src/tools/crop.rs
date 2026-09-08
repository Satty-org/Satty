use std::cell::Cell;

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

#[derive(Debug, Clone)]
pub struct Crop {
    origin: Vec2D,
    top_left: Vec2D,
    size: Vec2D,
    image_size: Vec2D,
    centered: bool,
    editing: bool,
    active: bool,
    scale: Cell<f32>,
}

#[derive(Default)]
pub struct CropTool {
    crop: Option<Crop>,
    dragging: bool,
    input_enabled: bool,
    image_size: Vec2D,
    sender: Option<Sender<SketchBoardInput>>,
}

// a odd number that I like
const SNAP_THRESHOLD: f32 = 23.0;

impl Crop {
    // `anchor` is the point that must stay fixed while size/top_left are adjusted
    // aspect ratio comes from unrounded coordinates to avoid jitter
    fn snap_to_image_bounds(&mut self, is_drag: bool, aspect_ratio: f32, anchor: Vec2D) {
        // scale independent snap threshold, i.e. in mouse coordinates
        let snap_theshold = SNAP_THRESHOLD / self.scale.get();

        let orig_tl = self.top_left;
        let orig_size = self.size;
        let br_offset = (orig_tl + orig_size) - self.image_size;

        let mut new_tl = orig_tl;
        let mut new_size = orig_size;

        let anchor_right = anchor.x > orig_tl.x + orig_size.x / 2.0;
        let anchor_bottom = anchor.y > orig_tl.y + orig_size.y / 2.0;

        // only snap if we exceed 0 - a value of 0 is already snapped
        // only snap one edge at a time to avoid conflicting adjustments
        if -snap_theshold <= orig_tl.x && orig_tl.x < 0.0 {
            new_tl.x = 0.0;
            if is_drag {
                new_size.x = orig_size.x + orig_tl.x;
                if orig_tl.x < 0.0 && aspect_ratio > 0.0 {
                    new_size.y = (new_size.x / aspect_ratio).round();
                    if anchor_bottom {
                        new_tl.y = anchor.y - new_size.y;
                    }
                }
            }
        } else if -snap_theshold <= orig_tl.y && orig_tl.y < 0.0 {
            new_tl.y = 0.0;
            if is_drag {
                new_size.y = orig_size.y + orig_tl.y;
                if orig_tl.y < 0.0 && aspect_ratio > 0.0 {
                    new_size.x = (new_size.y * aspect_ratio).round();
                    if anchor_right {
                        new_tl.x = anchor.x - new_size.x;
                    }
                }
            }
        } else if 0.0 < br_offset.x && br_offset.x <= snap_theshold {
            if is_drag {
                new_size.x = orig_size.x - br_offset.x;
                if br_offset.x > 0.0 && aspect_ratio > 0.0 {
                    new_size.y = (new_size.x / aspect_ratio).round();
                    if anchor_bottom {
                        new_tl.y = anchor.y - new_size.y;
                    }
                }
            } else {
                new_tl.x = self.image_size.x - orig_size.x;
            }
        } else if 0.0 < br_offset.y && br_offset.y <= snap_theshold {
            if is_drag {
                new_size.y = orig_size.y - br_offset.y;
                if br_offset.y > 0.0 && aspect_ratio > 0.0 {
                    new_size.x = (new_size.y * aspect_ratio).round();
                    if anchor_right {
                        new_tl.x = anchor.x - new_size.x;
                    }
                }
            } else {
                new_tl.y = self.image_size.y - orig_size.y;
            }
        }

        self.top_left = new_tl;
        self.size = new_size;
    }

    pub fn calculate_shape(&mut self, sender: &Sender<SketchBoardInput>, event: &MouseEventMsg) {
        let drag_box = DragBox::from_origin_delta(self.origin, self.size, event, sender);
        self.centered = drag_box.centered;
        self.top_left = drag_box.top_left;
        let br = (drag_box.top_left + drag_box.size).round();
        if drag_box.delta.x < 0.0 {
            self.size.x = drag_box.size.x.round();
            self.top_left.x = (br.x - self.size.x).round();
        } else {
            self.size.x = drag_box.size.x.round();
            self.top_left.x = drag_box.top_left.x.round();
        }
        if drag_box.delta.y < 0.0 {
            self.size.y = drag_box.size.y.round();
            self.top_left.y = (br.y - self.size.y).round();
        } else {
            self.size.y = drag_box.size.y.round();
            self.top_left.y = drag_box.top_left.y.round();
        }

        let aspect_ratio = if drag_box.keep_aspect && drag_box.size.y.abs() > f32::EPSILON {
            drag_box.size.x / drag_box.size.y
        } else {
            0.0
        };

        // the drag origin is the fixed corner while creating the box
        self.snap_to_image_bounds(true, aspect_ratio, self.origin);

        // Notify the sender about the updated rounded dimensions
        sender
            .send(SketchBoardInput::ShapeDimensionsUpdate(self.size))
            .ok();
    }
}

impl Drawable for Crop {
    fn get_rendering_mode(&self) -> RenderingMode {
        RenderingMode::Crop
    }

    fn bounds(&self) -> Option<(Vec2D, Vec2D)> {
        Some(math::ensure_bounding_box(
            self.top_left.round(),
            (self.top_left + self.size).round(),
        ))
    }

    fn hit_test(&self, pos: Vec2D, tolerance: f32) -> bool {
        hit_test_rectangle(pos, self.top_left, self.size, tolerance, false)
    }

    fn translate(&mut self, delta: Vec2D) {
        self.top_left = (self.top_left + delta).round();
        self.snap_to_image_bounds(false, 0.0, delta);
    }

    fn resize_bounds(&mut self, tl: Vec2D, br: Vec2D, _delta: Vec2D, keep_aspect: bool) {
        let (tl, br) = math::ensure_bounding_box(tl.round(), br.round());

        // Figure out which corner didn't move (the handle's opposite corner) -
        // that's the anchor that must stay fixed while snapping/aspect-locking.
        let orig_tl = self.top_left;
        let orig_br = self.top_left + self.size;
        let anchor = Vec2D::new(
            if (tl.x - orig_tl.x).abs() <= (br.x - orig_br.x).abs() {
                orig_tl.x
            } else {
                orig_br.x
            },
            if (tl.y - orig_tl.y).abs() <= (br.y - orig_br.y).abs() {
                orig_tl.y
            } else {
                orig_br.y
            },
        );

        // use the stable pre-drag size for the ratio, not the size we're about
        // to overwrite below, to avoid compounding rounding error.
        let orig_size = orig_br - orig_tl;
        let aspect_ratio = if keep_aspect && orig_size.y.abs() > f32::EPSILON {
            orig_size.x / orig_size.y
        } else {
            0.0
        };

        self.top_left = tl;
        self.size = br - tl;
        self.snap_to_image_bounds(true, aspect_ratio, anchor);
    }

    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: femtovg::FontId,
        bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        self.scale
            .set(canvas.transform().average_scale().max(f32::EPSILON));

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

    fn set_image_size(&mut self, size: Vec2D) {
        self.image_size = size;
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
                    image_size: self.image_size,
                    centered: false,
                    editing: true,
                    active: true,
                    scale: Cell::new(1.0),
                });
                ToolUpdateResult::Redraw
            }
            MouseEventType::EndDrag if event.button == MouseButton::Primary => {
                self.dragging = false;
                let Some(crop) = &mut self.crop else {
                    return ToolUpdateResult::Unmodified;
                };
                if crop.size.x == 0.0 || crop.size.y == 0.0 {
                    self.crop = None;
                    if let Some(sender) = &self.sender {
                        sender
                            .send(SketchBoardInput::ShapeDimensionsUpdate(self.image_size))
                            .ok();
                    }
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
