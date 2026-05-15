use std::cell::RefCell;

use anyhow::Result;
use femtovg::{Color, ImageFilter, ImageFlags, ImageId, Paint, Path, imgref::Img, rgb::RGBA8};
use serde_derive::Deserialize;

use relm4::{Sender, gtk::gdk::Key};

use crate::{
    configuration::APP_CONFIG,
    math::{self, Rect, Vec2D},
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    style::Style,
};

use super::{
    Drawable, DrawableClone, GLOW_COLOR, Handle, HandleId, Tool, ToolUpdateResult, Tools,
    bbox_handles, bbox_resize, halo_in_image_units,
};

/// Algorithm used to obscure the region covered by a Blur drawable.
///
/// Two reversibility tiers:
///
/// **Reversible (cosmetic).** `Gaussian` is the historical behaviour —
/// a `femtovg` `GaussianBlur` over a screenshot of the region. Looks
/// soft, but is a linear convolution; modern AI deblurring can recover
/// legible text or faces. Use it when the goal is "soften", not "hide".
///
/// **Irreversible (redaction-grade).** Three flavours that destroy
/// enough information that ML attacks have nothing useful to invert:
///
/// - `Pixelate` — coarse block-mean mosaic with 4-bit-per-channel
///   quantisation (see `Blur::pixelate`).
/// - `SecureBlur` — fills the region with a distance-weighted
///   interpolation of pixels sampled *outside* the selection, then
///   Gaussian-blurs the result (see `Blur::secure_blur`). No pixel
///   from inside the selection contributes to the output, so there
///   is mathematically nothing to recover.
/// - `BlackOut` — solid black fill. Trivially irreversible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BlurStyle {
    /// Pixelated mosaic — coarse blocks with quantized colors.
    Pixelate,
    /// Boundary-sampled blur. Irreversible: the region is filled from
    /// pixels outside the selection and then Gaussian-blurred. Looks
    /// like a blur, behaves like a redaction.
    SecureBlur,
    /// Gaussian blur — soft, natural-looking, but reversible.
    #[default]
    Gaussian,
    /// Solid black fill. Trivially irreversible.
    BlackOut,
}

impl BlurStyle {
    /// Human label for the cycle toast — one word per variant.
    pub fn display_name(self) -> &'static str {
        use BlurStyle::*;
        match self {
            Pixelate => "Pixelate",
            SecureBlur => "Secure Blur",
            Gaussian => "Gaussian",
            BlackOut => "Black Out",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Blur {
    top_left: Vec2D,
    size: Option<Vec2D>,
    style: Style,
    /// Obscuring algorithm baked in at creation. Existing Blur
    /// drawables keep their original algorithm even if the toolbar
    /// dropdown is later switched — matches how `arrow_style` works.
    blur_style: BlurStyle,
    editing: bool,
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

    /// Build an irreversible blur of the canvas region `(pos, size)` by
    /// sampling **only** the four 1-pixel-wide strips immediately
    /// outside the selection and then Gaussian-blurring the result.
    ///
    /// Why: a Gaussian over the original pixels is a linear convolution
    /// and is therefore invertible in principle — ML deblurring models
    /// recover legible text from heavily blurred input. By seeding the
    /// region from out-of-bounds samples and *then* blurring, no pixel
    /// of the original content reaches the rendered output. There is
    /// nothing inside to recover.
    ///
    /// Algorithm:
    /// 1. Read four 1-px fringe strips (N/S/E/W) just outside the rect.
    /// 2. For each interior pixel `(x, y)`, blend the four matching
    ///    edge pixels with weights proportional to inverse distance:
    ///    `w_n = (h - y) / h`, etc. Weights sum to 2 (one unit per
    ///    axis), so we divide the weighted sum by 2.
    /// 3. Upload as a femtovg image and apply `GaussianBlur` so the
    ///    result reads as a soft blur rather than a four-corner
    ///    gradient.
    ///
    /// Fallback: if the rect is flush against any canvas edge we can't
    /// sample a fringe outside the screenshot, so we degrade to solid
    /// black. The "no interior pixel contributes" contract is preserved.
    fn secure_blur(
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        pos: Vec2D,
        size: Vec2D,
        sigma: f32,
    ) -> Result<ImageId> {
        let transformed_pos = canvas.transform().transform_point(pos.x, pos.y);
        let transformed_size = size * canvas.transform().average_scale();

        let pos_x = transformed_pos.0 as usize;
        let pos_y = transformed_pos.1 as usize;
        let width = (transformed_size.x as usize).max(1);
        let height = (transformed_size.y as usize).max(1);

        let canvas_w = canvas.width() as usize;
        let canvas_h = canvas.height() as usize;

        if pos_x < 1
            || pos_y < 1
            || pos_x + width + 1 >= canvas_w
            || pos_y + height + 1 >= canvas_h
        {
            let buf = vec![RGBA8::new(0, 0, 0, 255); width * height];
            let img = Img::new(buf, width, height);
            return Ok(canvas.create_image(img.as_ref(), ImageFlags::empty())?);
        }

        let screenshot = canvas.screenshot()?;
        let (buf_n, _, _) = screenshot
            .sub_image(pos_x, pos_y - 1, width, 1)
            .to_contiguous_buf();
        let (buf_s, _, _) = screenshot
            .sub_image(pos_x, pos_y + height, width, 1)
            .to_contiguous_buf();
        let (buf_w, _, _) = screenshot
            .sub_image(pos_x - 1, pos_y, 1, height)
            .to_contiguous_buf();
        let (buf_e, _, _) = screenshot
            .sub_image(pos_x + width, pos_y, 1, height)
            .to_contiguous_buf();

        let mut out: Vec<RGBA8> = Vec::with_capacity(width * height);
        let w_f = width as f32;
        let h_f = height as f32;
        for y in 0..height {
            for x in 0..width {
                let pn = buf_n[x];
                let ps = buf_s[x];
                let pw = buf_w[y];
                let pe = buf_e[y];

                let wn = (height - y) as f32 / h_f;
                let ws = y as f32 / h_f;
                let ww = (width - x) as f32 / w_f;
                let we = x as f32 / w_f;
                let mix = |cn: u8, cs: u8, cww: u8, cee: u8| -> u8 {
                    ((cn as f32 * wn + cs as f32 * ws + cww as f32 * ww + cee as f32 * we) / 2.0)
                        as u8
                };
                out.push(RGBA8::new(
                    mix(pn.r, ps.r, pw.r, pe.r),
                    mix(pn.g, ps.g, pw.g, pe.g),
                    mix(pn.b, ps.b, pw.b, pe.b),
                    255,
                ));
            }
        }

        let src_img = Img::new(out, width, height);
        let src_image_id = canvas.create_image(src_img.as_ref(), ImageFlags::empty())?;
        let dst_image_id = canvas.create_image_empty(
            width,
            height,
            femtovg::PixelFormat::Rgba8,
            ImageFlags::empty(),
        )?;
        canvas.filter_image(
            dst_image_id,
            ImageFilter::GaussianBlur { sigma },
            src_image_id,
        );
        Ok(dst_image_id)
    }

    /// Build a pixelated (block-mean mosaic) image of the canvas region
    /// `(pos, size)` for irreversibility-grade redaction.
    ///
    /// Implementation:
    /// 1. Grab the same screenshot sub-image the Gaussian path uses,
    ///    so the source is the rasterized canvas under the blur region.
    /// 2. Downsample to `(src_w / cell_px, src_h / cell_px)` by averaging
    ///    each `cell_px × cell_px` source block's pixels — true mean of
    ///    the underlying content. Output is one quantized RGBA per cell.
    /// 3. Quantize each channel to 4 bits (16 levels) by masking the
    ///    low nibble (`c & 0xF0 | c >> 4`). This destroys the
    ///    fine-grained mean information that ML depixelation models
    ///    rely on — recovery degrades from "near-perfect for known
    ///    content" to "extremely lossy". Visually, the user mostly
    ///    sees mild palette banding at coarse cell sizes.
    /// 4. Upload with `ImageFlags::NEAREST` so the GPU upsample back
    ///    to the destination rect preserves crisp block edges instead
    ///    of smoothing them via bilinear filtering — the pixelated
    ///    look the user expects.
    ///
    /// `cell_px` is in *canvas* (post-transform) pixels so the visible
    /// block size stays constant regardless of zoom, matching how the
    /// Gaussian sigma is interpreted.
    fn pixelate(
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        pos: Vec2D,
        size: Vec2D,
        cell_px: f32,
    ) -> Result<ImageId> {
        let img = canvas.screenshot()?;

        let transformed_pos = canvas.transform().transform_point(pos.x, pos.y);
        let transformed_size = size * canvas.transform().average_scale();
        let canvas_cell = (cell_px * canvas.transform().average_scale())
            .round()
            .max(2.0) as usize;

        let src_w = (transformed_size.x as usize).max(1);
        let src_h = (transformed_size.y as usize).max(1);

        let (buf, _, _) = img
            .sub_image(
                transformed_pos.0 as usize,
                transformed_pos.1 as usize,
                src_w,
                src_h,
            )
            .to_contiguous_buf();
        let src = buf.as_ref();

        let dst_w = src_w.div_ceil(canvas_cell);
        let dst_h = src_h.div_ceil(canvas_cell);
        let mut down: Vec<RGBA8> = Vec::with_capacity(dst_w * dst_h);

        for dy in 0..dst_h {
            for dx in 0..dst_w {
                let mut sum_r: u32 = 0;
                let mut sum_g: u32 = 0;
                let mut sum_b: u32 = 0;
                let mut sum_a: u32 = 0;
                let mut count: u32 = 0;
                let x0 = dx * canvas_cell;
                let y0 = dy * canvas_cell;
                let x1 = (x0 + canvas_cell).min(src_w);
                let y1 = (y0 + canvas_cell).min(src_h);
                for sy in y0..y1 {
                    let row = sy * src_w;
                    for sx in x0..x1 {
                        let p = src[row + sx];
                        sum_r += p.r as u32;
                        sum_g += p.g as u32;
                        sum_b += p.b as u32;
                        sum_a += p.a as u32;
                        count += 1;
                    }
                }
                let r = sum_r.checked_div(count).unwrap_or(0) as u8;
                let g = sum_g.checked_div(count).unwrap_or(0) as u8;
                let b = sum_b.checked_div(count).unwrap_or(0) as u8;
                let a = sum_a.checked_div(count).unwrap_or(0) as u8;
                // 4-bit-per-channel quantization. `c & 0xF0` zeroes the
                // low nibble; `| c >> 4` smears the high nibble into it
                // so the 16 representable values span 0..=255 evenly
                // (0x00, 0x11, 0x22, …, 0xFF) instead of clustering at
                // the low end.
                let q = |c: u8| (c & 0xF0) | (c >> 4);
                down.push(RGBA8::new(q(r), q(g), q(b), q(a)));
            }
        }

        let down_img = Img::new(down, dst_w, dst_h);
        let dst_image_id = canvas.create_image(down_img.as_ref(), ImageFlags::NEAREST)?;
        Ok(dst_image_id)
    }
}

impl Drawable for Blur {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn kind_label(&self) -> &'static str {
        "Blur"
    }
    fn icon_name(&self) -> &'static str {
        // Mirrors `blur_style_icon` in toolbars — keeps the panel row's
        // kind icon visually distinct between Pixelate / SecureBlur /
        // Gaussian / BlackOut. Re-using the same icon set the style
        // dropdown already uses also means we don't add four more icons
        // to the bundle.
        use BlurStyle::*;
        match self.blur_style {
            Pixelate => "tetris-app-regular",
            SecureBlur => "shield-lock-regular",
            Gaussian => "drop-regular",
            BlackOut => "weather-moon-regular",
        }
    }
    fn panel_label_kind(&self) -> String {
        // Style-prefixed label so users can tell Pixelate vs Gaussian
        // at a glance. "Secure Blur" / "Black Out" already read as
        // standalone — appending "Blur" would just stutter.
        use BlurStyle::*;
        match self.blur_style {
            Pixelate => "Pixelate Blur".into(),
            SecureBlur => "Secure Blur".into(),
            Gaussian => "Gaussian Blur".into(),
            BlackOut => "Black Out".into(),
        }
    }
    fn panel_swatch(&self) -> crate::tools::PanelSwatch {
        // Blur doesn't have a "color" — its effect is to obscure
        // whatever sits beneath. The transparency-checker pattern
        // visually reads as "the content shows through, modified".
        crate::tools::PanelSwatch::Checkerboard
    }

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

            // Black Out is a constant fill — no screenshot, no cache.
            if self.blur_style == BlurStyle::BlackOut {
                let mut path = Path::new();
                path.rounded_rect(
                    pos.x,
                    pos.y,
                    size.x,
                    size.y,
                    APP_CONFIG.read().corner_roundness(),
                );
                canvas.fill_path(&path, &Paint::color(Color::black()));
                canvas.restore();
                return Ok(());
            }

            // create new cached image
            if self.cached_image.borrow().is_none() {
                let id = match self.blur_style {
                    BlurStyle::Gaussian => Self::blur(
                        canvas,
                        pos,
                        size,
                        self.style
                            .size
                            .to_blur_factor(self.style.annotation_size_factor),
                    )?,
                    BlurStyle::SecureBlur => Self::secure_blur(
                        canvas,
                        pos,
                        size,
                        self.style
                            .size
                            .to_blur_factor(self.style.annotation_size_factor),
                    )?,
                    BlurStyle::Pixelate => Self::pixelate(
                        canvas,
                        pos,
                        size,
                        self.style
                            .size
                            .to_pixelate_cell_size(self.style.annotation_size_factor),
                    )?,
                    BlurStyle::BlackOut => unreachable!("handled above"),
                };
                self.cached_image.borrow_mut().replace(id);
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

    fn bounds(&self) -> Option<Rect> {
        self.size.map(|s| Rect::new(self.top_left, s))
    }

    fn translate(&mut self, delta: Vec2D) {
        self.top_left += delta;
        // Invalidate the cached blurred image — its sample location changed.
        self.cached_image.borrow_mut().take();
    }

    fn handles(&self) -> Vec<Handle> {
        self.bounds().map(bbox_handles).unwrap_or_default()
    }

    fn move_handle(&mut self, handle: HandleId, to: Vec2D) {
        let Some(cur) = self.bounds() else { return };
        let new = bbox_resize(cur, handle, to);
        self.top_left = new.pos;
        self.size = Some(new.size);
        self.cached_image.borrow_mut().take();
    }

    fn set_style(&mut self, style: Style) {
        self.style = style;
        // Style affects blur sigma → invalidate cache.
        self.cached_image.borrow_mut().take();
    }

    fn style(&self) -> Option<Style> {
        Some(self.style)
    }

    fn blur_style(&self) -> Option<BlurStyle> {
        Some(self.blur_style)
    }

    fn set_blur_style_on_drawable(&mut self, style: BlurStyle) {
        self.blur_style = style;
        // Algorithm change forces a re-blur.
        self.cached_image.borrow_mut().take();
    }

    fn tool_type(&self) -> Option<Tools> {
        Some(Tools::Blur)
    }

    fn render_glow(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: femtovg::FontId,
        _bounds: (Vec2D, Vec2D),
        device_pixel_ratio: f32,
    ) -> anyhow::Result<()> {
        let Some(rect) = self.bounds() else {
            return Ok(());
        };
        let halo = halo_in_image_units(canvas, device_pixel_ratio);
        let inflate = halo / 2.0;
        canvas.save();
        let mut path = Path::new();
        path.rounded_rect(
            rect.pos.x - inflate,
            rect.pos.y - inflate,
            rect.size.x + inflate * 2.0,
            rect.size.y + inflate * 2.0,
            APP_CONFIG.read().corner_roundness() + inflate,
        );
        let mut paint = Paint::color(GLOW_COLOR);
        paint.set_line_width(halo);
        paint.set_line_join(femtovg::LineJoin::Round);
        canvas.stroke_path(&path, &paint);
        canvas.restore();
        Ok(())
    }
}

#[derive(Default)]
pub struct BlurTool {
    blur: Option<Blur>,
    style: Style,
    /// Currently-selected obscuring algorithm. Captured into each new
    /// `Blur` at creation; committed Blurs keep their original style
    /// even if the toolbar later switches.
    blur_style: BlurStyle,
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

                // start new
                self.blur = Some(Blur {
                    top_left: event.pos,
                    size: None,
                    style: self.style,
                    blur_style: self.blur_style,
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

    fn set_blur_style(&mut self, style: BlurStyle) {
        self.blur_style = style;
    }
}
