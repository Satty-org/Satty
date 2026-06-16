use std::cell::RefCell;

use anyhow::Result;
use femtovg::{Color, ImageFilter, ImageFlags, ImageId, Paint, Path, imgref::Img};

use relm4::{Sender, gtk::gdk::Key};

use crate::{
    configuration::APP_CONFIG,
    math::{self, Vec2D},
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    style::Style,
};

use super::{Drawable, DrawableClone, Tool, ToolUpdateResult, Tools};

#[derive(Clone, Debug)]
pub struct Blur {
    top_left: Vec2D,
    size: Option<Vec2D>,
    style: Style,
    editing: bool,
    cached_image: RefCell<Option<ImageId>>,
}

impl Blur {
    /// Sample the current render target under the blur rect and produce a blurred image of it.
    ///
    /// Returns `Ok(None)` when the rect doesn't overlap what this render target currently shows.
    /// With fullscreen="all" each monitor's canvas only renders its own slice of the image, so a
    /// blur belonging to another monitor maps entirely off this target; there is nothing to sample
    /// and nothing visible to draw. Crucially we do NOT cache in that case (see `draw`), so the
    /// full-image render used for saving/copying still recomputes the blur from real pixels.
    fn blur(
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        pos: Vec2D,
        size: Vec2D,
        sigma: f32,
    ) -> Result<Option<ImageId>> {
        let transformed_pos = canvas.transform().transform_point(pos.x, pos.y);
        let transformed_size = size * canvas.transform().average_scale();

        // Clamp the sampled region to the current render target. `width()`/`height()` report the
        // active target (the on-screen slice while drawing, the full image while exporting), which
        // is exactly what `screenshot()` below reads back. Flooring each edge independently keeps
        // `left + width <= width()` (likewise for height), which is what sub_image asserts — note a
        // forced minimum size would break this when the rect sits past the right/bottom edge.
        let target_w = canvas.width() as usize;
        let target_h = canvas.height() as usize;
        let left = (transformed_pos.0.max(0.0) as usize).min(target_w);
        let top = (transformed_pos.1.max(0.0) as usize).min(target_h);
        let right = ((transformed_pos.0 + transformed_size.x).max(0.0) as usize).min(target_w);
        let bottom = ((transformed_pos.1 + transformed_size.y).max(0.0) as usize).min(target_h);
        let width = right.saturating_sub(left);
        let height = bottom.saturating_sub(top);

        // No overlap: bail before the (expensive) screenshot read-back and without caching.
        if width == 0 || height == 0 {
            return Ok(None);
        }

        let img = canvas.screenshot()?;

        let (buf, width, height) = img.sub_image(left, top, width, height).to_contiguous_buf();
        let sub = Img::new(buf.into_owned(), width, height);

        let src_image_id = canvas.create_image(sub.as_ref(), ImageFlags::empty())?;
        let dst_image_id = canvas.create_image_empty(
            sub.width(),
            sub.height(),
            femtovg::PixelFormat::Rgba8,
            ImageFlags::empty(),
        )?;

        canvas.filter_image(
            dst_image_id,
            ImageFilter::GaussianBlur { sigma },
            src_image_id,
        );
        //canvas.delete_image(src_image_id);

        Ok(Some(dst_image_id))
    }
}

impl Drawable for Blur {
    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: femtovg::FontId,
        bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        let size = match self.size {
            Some(s) => s,
            None => return Ok(()), // early exit if none
        };
        let (pos, size) = math::rect_ensure_in_bounds(
            math::rect_ensure_positive_size(self.top_left, size),
            bounds,
        );
        if self.editing {
            // set style
            let mut color = Color::black();
            color.set_alphaf(0.6);
            let paint = Paint::color(color);

            // make rect
            let mut path = Path::new();
            path.rounded_rect(
                pos.x,
                pos.y,
                size.x,
                size.y,
                APP_CONFIG.read().corner_roundness(),
            );

            // draw
            canvas.fill_path(&path, &paint);
        } else {
            if size.x <= 0.0 || size.y <= 0.0 {
                return Ok(());
            }

            canvas.save();
            canvas.flush();

            // Create the cached image lazily. `blur` returns None when the rect isn't visible on
            // this render target; we leave the cache empty in that case so a later render that does
            // contain it (e.g. the full-image export) can still compute it.
            if self.cached_image.borrow().is_none() {
                let blurred = Self::blur(
                    canvas,
                    pos,
                    size,
                    self.style
                        .size
                        .to_blur_factor(self.style.annotation_size_factor),
                )?;
                if let Some(id) = blurred {
                    self.cached_image.borrow_mut().replace(id);
                }
            }

            if let Some(image_id) = *self.cached_image.borrow() {
                let mut path = Path::new();
                path.rounded_rect(
                    pos.x,
                    pos.y,
                    size.x,
                    size.y,
                    APP_CONFIG.read().corner_roundness(),
                );

                canvas.fill_path(
                    &path,
                    &Paint::image(image_id, pos.x, pos.y, size.x, size.y, 0f32, 1f32),
                );
            }
            canvas.restore();
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct BlurTool {
    blur: Option<Blur>,
    style: Style,
    input_enabled: bool,
    sender: Option<Sender<SketchBoardInput>>,
}

impl Tool for BlurTool {
    fn input_enabled(&self) -> bool {
        self.input_enabled
    }

    fn set_input_enabled(&mut self, value: bool) {
        self.input_enabled = value;
    }

    fn active(&self) -> bool {
        self.blur.is_some()
    }

    fn get_tool_type(&self) -> super::Tools {
        Tools::Blur
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        match event.type_ {
            MouseEventType::BeginDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                // start new
                self.blur = Some(Blur {
                    top_left: event.pos,
                    size: None,
                    style: self.style,
                    editing: true,
                    cached_image: RefCell::new(None),
                });

                ToolUpdateResult::Redraw
            }
            MouseEventType::EndDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                if let Some(a) = &mut self.blur {
                    if event.pos == Vec2D::zero() {
                        self.blur = None;

                        ToolUpdateResult::Redraw
                    } else {
                        a.size = Some(event.pos);
                        a.editing = false;

                        let result = a.clone_box();
                        self.blur = None;

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

                if let Some(a) = &mut self.blur {
                    if event.pos == Vec2D::zero() {
                        return ToolUpdateResult::Unmodified;
                    }
                    a.size = Some(event.pos);

                    ToolUpdateResult::Redraw
                } else {
                    ToolUpdateResult::Unmodified
                }
            }
            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn handle_key_event(&mut self, event: crate::sketch_board::KeyEventMsg) -> ToolUpdateResult {
        if event.key == Key::Escape && self.blur.is_some() {
            self.blur = None;
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
        match &self.blur {
            Some(d) => Some(d),
            None => None,
        }
    }

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>) {
        self.sender = Some(sender);
    }
}
