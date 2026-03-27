use std::cell::RefCell;

use anyhow::Result;
use femtovg::{Color, ImageFilter, ImageFlags, ImageId, Paint, Path, imgref::Img, rgb::Rgba};

use relm4::{Sender, gtk::gdk::{Key, ModifierType}};

use crate::{
    configuration::APP_CONFIG,
    math::{self, Vec2D},
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    style::Style,
};

use super::{Drawable, DrawableClone, Tool, ToolUpdateResult, Tools};

#[derive(Clone, Copy, Debug, PartialEq)]
enum BlurMode {
    Blur,
    Pixelate,
}

#[derive(Clone, Debug)]
pub struct Blur {
    top_left: Vec2D,
    size: Option<Vec2D>,
    style: Style,
    editing: bool,
    mode: BlurMode,
    cached_image: RefCell<Option<ImageId>>,
}

impl Blur {
    fn blur(
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        pos: Vec2D,
        size: Vec2D,
        sigma: f32,
    ) -> Result<ImageId> {
        let img = canvas.screenshot()?;

        let transformed_pos = canvas.transform().transform_point(pos.x, pos.y);
        let transformed_size = size * canvas.transform().average_scale();

        let (buf, width, height) = img
            .sub_image(
                transformed_pos.0 as usize,
                transformed_pos.1 as usize,
                (transformed_size.x as usize).max(1),
                (transformed_size.y as usize).max(1),
            )
            .to_contiguous_buf();
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

        Ok(dst_image_id)
    }

    fn pixelate(
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        pos: Vec2D,
        size: Vec2D,
        intensity: f32,
    ) -> Result<ImageId> {
        let img = canvas.screenshot()?;

        let transformed_pos = canvas.transform().transform_point(pos.x, pos.y);
        let transformed_size = size * canvas.transform().average_scale();

        let (buf, width, height) = img
            .sub_image(
                transformed_pos.0 as usize,
                transformed_pos.1 as usize,
                (transformed_size.x as usize).max(1),
                (transformed_size.y as usize).max(1),
            )
            .to_contiguous_buf();

        let factor = 0.5 / (intensity + 1.0);
        let small_w = (width as f32 * factor).max(1.0) as usize;
        let small_h = (height as f32 * factor).max(1.0) as usize;

        let mut small_buf: Vec<Rgba<u8>> = vec
![Rgba::new(0, 0, 0, 0); small_w * small_h]
;

        for y in 0..small_h {
            for x in 0..small_w {
                let mut r = 0u32;
                let mut g = 0u32;
                let mut b = 0u32;
                let mut count = 0u32;

                let sy_start = (y as f32 * height as f32 / small_h as f32) as usize;
                let sy_end = ((y + 1) as f32 * height as f32 / small_h as f32) as usize;
                let sx_start = (x as f32 * width as f32 / small_w as f32) as usize;
                let sx_end = ((x + 1) as f32 * width as f32 / small_w as f32) as usize;

                for sy in sy_start..sy_end {
                    for sx in sx_start..sx_end {
                        let idx = sy * width + sx;
                        let pixel = buf[idx];
                        r += pixel.r as u32;
                        g += pixel.g as u32;
                        b += pixel.b as u32;
                        count += 1;
                    }
                }

                let idx = y * small_w + x;
                small_buf[idx] = Rgba::new(
                    (r / count.max(1)) as u8,
                    (g / count.max(1)) as u8,
                    (b / count.max(1)) as u8,
                    255
                );
            }
        }

        let mut final_buf: Vec<Rgba<u8>> = vec
![Rgba::new(0, 0, 0, 0); width * height]
;

        for y in 0..height {
            let py = (y as f32 * small_h as f32 / height as f32) as usize;
            for x in 0..width {
                let px = (x as f32 * small_w as f32 / width as f32) as usize;

                let src_idx = py * small_w + px;
                let dst_idx = y * width + x;

                final_buf[dst_idx] = small_buf[src_idx];
            }
        }

        let final_img = Img::new(final_buf, width, height);
        let image_id = canvas.create_image(final_img.as_ref(), ImageFlags::NEAREST)?;

        Ok(image_id)
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

            // create new cached image
            if self.cached_image.borrow().is_none() {
                let intensity = self
                    .style
                    .size
                    .to_blur_factor(self.style.annotation_size_factor);

                let new_image = match self.mode {
                    BlurMode::Blur => Self::blur(canvas, pos, size, intensity)?,
                    BlurMode::Pixelate => Self::pixelate(canvas, pos, size, intensity)?,
                };

                self.cached_image.borrow_mut().replace(new_image);
            }

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
                &Paint::image(
                    self.cached_image.borrow().unwrap(), // this unwrap is safe because we placed it above
                    pos.x,
                    pos.y,
                    size.x,
                    size.y,
                    0f32,
                    1f32,
                ),
            );
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

    fn get_tool_type(&self) -> super::Tools {
        Tools::Blur
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        match event.type_ {
            MouseEventType::BeginDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                let mode = if event.modifier.contains(ModifierType::ALT_MASK) {
                    BlurMode::Pixelate
                } else {
                    BlurMode::Blur
                };

                // start new
                self.blur = Some(Blur {
                    top_left: event.pos,
                    size: None,
                    style: self.style,
                    editing: true,
                    mode,
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
