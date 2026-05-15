use anyhow::Result;
use femtovg::{FontId, Path};
use relm4::{
    Sender,
    gtk::gdk::{Key, ModifierType},
};

use crate::{
    configuration::APP_CONFIG,
    math::{Rect, Vec2D},
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    style::Style,
};

use super::{
    Drawable, DrawableClone, GLOW_COLOR, Handle, HandleId, Tool, halo_in_image_units,
    ToolUpdateResult, Tools, bbox_handles, bbox_resize,
};

#[derive(Clone, Copy, Debug)]
pub struct Rectangle {
    origin: Vec2D,
    top_left: Vec2D,
    size: Option<Vec2D>,
    style: Style,
    centered: bool,
    finishing: bool,
}

impl Drawable for Rectangle {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn kind_label(&self) -> &'static str {
        "Rectangle"
    }
    fn icon_name(&self) -> &'static str {
        "rectangle-landscape-regular"
    }
    fn panel_preview(&self) -> crate::tools::PanelPreview {
        crate::tools::PanelPreview::Rectangle {
            filled: self.style.fill,
        }
    }

    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        let size = match self.size {
            Some(s) => s,
            None => return Ok(()), // early exit if none
        };

        canvas.save();
        let mut path = Path::new();
        path.rounded_rect(
            self.top_left.x,
            self.top_left.y,
            size.x,
            size.y,
            APP_CONFIG.read().corner_roundness(),
        );

        if self.style.fill {
            canvas.fill_path(&path, &self.style.into());
        } else {
            canvas.stroke_path(&path, &self.style.into());
        }
        canvas.restore();

        Ok(())
    }

    fn bounds(&self) -> Option<Rect> {
        self.size.map(|s| Rect::new(self.top_left, s))
    }

    fn hit_test(&self, point: Vec2D, tolerance: f32) -> bool {
        let Some(rect) = self.bounds() else {
            return false;
        };
        let stroke = self
            .style
            .size
            .to_line_width(self.style.annotation_size_factor);
        let half = stroke / 2.0 + tolerance;
        // Outer picking edge — everything outside this is a definite miss.
        if !rect.inflated(half).contains(point) {
            return false;
        }
        // Filled: any point inside the silhouette counts (the
        // interior is opaque).
        if self.style.fill {
            return true;
        }
        // Unfilled: hits only land on the stroke band. The inner
        // edge of the band is the rect deflated by `half`; we miss
        // if the point is INSIDE that inner edge (the hollow
        // middle). When the rect is small enough that the deflated
        // inner has no area, the stroke covers the whole interior —
        // any hit in the outer is a real hit.
        let inner_w = rect.size.x - 2.0 * half;
        let inner_h = rect.size.y - 2.0 * half;
        if inner_w <= 0.0 || inner_h <= 0.0 {
            return true;
        }
        let inner = Rect {
            pos: Vec2D::new(rect.pos.x + half, rect.pos.y + half),
            size: Vec2D::new(inner_w, inner_h),
        };
        !inner.contains(point)
    }

    fn translate(&mut self, delta: Vec2D) {
        self.top_left += delta;
        self.origin += delta;
    }

    fn handles(&self) -> Vec<Handle> {
        self.bounds().map(bbox_handles).unwrap_or_default()
    }

    fn move_handle(&mut self, handle: HandleId, to: Vec2D) {
        let Some(cur) = self.bounds() else { return };
        let new = bbox_resize(cur, handle, to);
        self.top_left = new.pos;
        self.size = Some(new.size);
        self.origin = new.pos;
    }

    fn set_style(&mut self, style: Style) {
        self.style = style;
    }

    fn style(&self) -> Option<Style> {
        Some(self.style)
    }

    fn tool_type(&self) -> Option<Tools> {
        Some(Tools::Rectangle)
    }

    fn render_glow(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: FontId,
        _bounds: (Vec2D, Vec2D),
        device_pixel_ratio: f32,
    ) -> Result<()> {
        let Some(rect) = self.bounds() else {
            return Ok(());
        };
        let halo = halo_in_image_units(canvas, device_pixel_ratio);
        canvas.save();
        if self.style.fill {
            // Filled: halo entirely outside the silhouette. Inflate the
            // path by halo/2 and stroke `halo` wide so the band is
            // (silhouette, silhouette + halo).
            let inflate = halo / 2.0;
            let mut path = Path::new();
            path.rounded_rect(
                rect.pos.x - inflate,
                rect.pos.y - inflate,
                rect.size.x + inflate * 2.0,
                rect.size.y + inflate * 2.0,
                APP_CONFIG.read().corner_roundness() + inflate,
            );
            let mut paint = femtovg::Paint::color(GLOW_COLOR);
            paint.set_line_width(halo);
            paint.set_line_join(femtovg::LineJoin::Round);
            canvas.stroke_path(&path, &paint);
        } else {
            // Stroked: bracket the stroke on both sides. Stroke at the
            // path with line_width + 2 * halo; the actual rect stroke
            // overdraws the middle, leaving `halo` visible inside and out.
            let line_width = self
                .style
                .size
                .to_line_width(self.style.annotation_size_factor);
            let mut path = Path::new();
            path.rounded_rect(
                rect.pos.x,
                rect.pos.y,
                rect.size.x,
                rect.size.y,
                APP_CONFIG.read().corner_roundness(),
            );
            let mut paint = femtovg::Paint::color(GLOW_COLOR);
            paint.set_line_width(line_width + 2.0 * halo);
            paint.set_line_join(femtovg::LineJoin::Round);
            canvas.stroke_path(&path, &paint);
        }
        canvas.restore();
        Ok(())
    }
}

impl Rectangle {
    fn calculate_shape(&mut self, event: &MouseEventMsg) {
        self.centered = event.modifier & ModifierType::ALT_MASK == ModifierType::ALT_MASK;
        match event.modifier & (ModifierType::ALT_MASK | ModifierType::SHIFT_MASK) {
            v if v == ModifierType::ALT_MASK | ModifierType::SHIFT_MASK => {
                let max_size = event.pos.x.abs().max(event.pos.y.abs());
                self.top_left.x = self.origin.x - max_size * event.pos.x.signum() / 2.0;
                self.top_left.y = self.origin.y - max_size * event.pos.y.signum() / 2.0;
                self.size = Some(Vec2D {
                    x: max_size * event.pos.x.signum(),
                    y: max_size * event.pos.y.signum(),
                });
            }
            ModifierType::ALT_MASK => {
                self.top_left.x = self.origin.x - event.pos.x / 2.0;
                self.top_left.y = self.origin.y - event.pos.y / 2.0;
                self.size = Some(event.pos);
            }
            ModifierType::SHIFT_MASK => {
                self.top_left = self.origin;
                let max_size = event.pos.x.abs().max(event.pos.y.abs());
                self.size = Some(Vec2D {
                    x: max_size * event.pos.x.signum(),
                    y: max_size * event.pos.y.signum(),
                });
            }
            _ => {
                self.top_left = self.origin;
                self.size = Some(event.pos);
            }
        }
    }
}

#[derive(Default)]
pub struct RectangleTool {
    rectangle: Option<Rectangle>,
    style: Style,
    input_enabled: bool,
    sender: Option<Sender<SketchBoardInput>>,
}

impl Tool for RectangleTool {
    fn input_enabled(&self) -> bool {
        self.input_enabled
    }

    fn set_input_enabled(&mut self, value: bool) {
        self.input_enabled = value;
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        match event.type_ {
            MouseEventType::BeginDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }
                // start new
                self.rectangle = Some(Rectangle {
                    origin: event.pos,
                    top_left: event.pos,
                    size: None,
                    style: self.style,
                    centered: false,
                    finishing: false,
                });

                ToolUpdateResult::Redraw
            }
            MouseEventType::EndDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                if let Some(rectangle) = &mut self.rectangle {
                    rectangle.finishing = true;
                    if event.pos == Vec2D::zero() {
                        self.rectangle = None;

                        ToolUpdateResult::Redraw
                    } else {
                        rectangle.calculate_shape(&event);
                        let result = rectangle.clone_box();
                        self.rectangle = None;
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

                if let Some(rectangle) = &mut self.rectangle {
                    if event.pos == Vec2D::zero() {
                        return ToolUpdateResult::Unmodified;
                    }
                    rectangle.calculate_shape(&event);
                    ToolUpdateResult::Redraw
                } else {
                    ToolUpdateResult::Unmodified
                }
            }
            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn handle_key_event(&mut self, event: crate::sketch_board::KeyEventMsg) -> ToolUpdateResult {
        if event.key == Key::Escape && self.rectangle.is_some() {
            self.rectangle = None;
            ToolUpdateResult::Redraw
        } else {
            ToolUpdateResult::Unmodified
        }
    }

    fn handle_style_event(&mut self, style: Style) -> ToolUpdateResult {
        self.style = style;
        ToolUpdateResult::Unmodified
    }

    fn get_drawable(&self) -> Option<&dyn Drawable> {
        match &self.rectangle {
            Some(d) => Some(d),
            None => None,
        }
    }

    fn get_tool_type(&self) -> super::Tools {
        Tools::Rectangle
    }

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>) {
        self.sender = Some(sender);
    }
}
