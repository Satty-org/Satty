use anyhow::Result;
use femtovg::{
    Canvas, FontId, ImageFlags, ImageId, ImageSource, Paint, Path, PixelFormat, imgref::Img,
    renderer::OpenGl, rgb::RGBA8,
};
use relm4::gtk::gdk_pixbuf::Pixbuf;
use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use crate::math::{Rect, Vec2D};
use crate::tools::{
    Drawable, GLOW_COLOR, Handle, HandleId, bbox_handles, bbox_resize, halo_in_image_units,
};

/// Pre-converted RGBA pixel buffer, shared (cheaply cloneable) so the
/// pointer-tool's "clone the drawable for the working copy" path
/// doesn't duplicate megabytes per drag tick.
struct ImageData {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

/// User-pasted image, lives in the drawable stack like any other shape.
/// Move + resize work through the standard bbox handles; the renderer
/// uploads the RGBA buffer to femtovg lazily on first draw and caches
/// the resulting `ImageId` per-instance.
pub struct PastedImage {
    /// Top-left in image coordinates.
    pos: Vec2D,
    /// Current display size in image coords.
    size: Vec2D,
    /// Shared pixel data (RGBA8, tightly packed).
    data: Rc<ImageData>,
    /// Cached femtovg image id, lazily uploaded on first `draw`. Each
    /// clone gets a fresh cell — the pointer-tool clones the drawable
    /// per drag, and we don't want a clone to free a cache id the
    /// original is still using.
    image_id: RefCell<Option<ImageId>>,
}

impl fmt::Debug for PastedImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PastedImage")
            .field("pos", &self.pos)
            .field("size", &self.size)
            .field("source_size", &(self.data.width, self.data.height))
            .finish()
    }
}

impl Clone for PastedImage {
    fn clone(&self) -> Self {
        Self {
            pos: self.pos,
            size: self.size,
            data: self.data.clone(),
            image_id: RefCell::new(None),
        }
    }
}

impl PastedImage {
    /// Build from a Pixbuf at `pos` with an explicit `display_size` in
    /// image coordinates. The display size is decoupled from the
    /// pixbuf's native pixel dimensions so the caller can scale the
    /// paste to match whatever on-screen size it should appear at —
    /// e.g. dividing by the current canvas zoom so a screenshot of
    /// the canvas re-pastes at the same on-screen size it was
    /// captured at. Pre-converts pixels to a contiguous RGBA buffer
    /// so subsequent uploads don't have to redo the stride-aware copy.
    pub fn from_pixbuf(pixbuf: &Pixbuf, pos: Vec2D, display_size: Vec2D) -> Self {
        let width = pixbuf.width().max(1) as u32;
        let height = pixbuf.height().max(1) as u32;
        let has_alpha = pixbuf.has_alpha();
        let stride = pixbuf.rowstride() as usize;
        let bpp = if has_alpha { 4 } else { 3 };
        let row_len = width as usize * bpp;

        let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
        unsafe {
            let pixels = pixbuf.pixels();
            for row in 0..height as usize {
                let src = &pixels[row * stride..row * stride + row_len];
                if has_alpha {
                    rgba.extend_from_slice(src);
                } else {
                    // Inflate RGB → RGBA with full alpha so femtovg
                    // gets a single canonical pixel layout.
                    for px in src.chunks_exact(3) {
                        rgba.push(px[0]);
                        rgba.push(px[1]);
                        rgba.push(px[2]);
                        rgba.push(255);
                    }
                }
            }
        }

        Self {
            pos,
            size: Vec2D::new(display_size.x.max(1.0), display_size.y.max(1.0)),
            data: Rc::new(ImageData {
                rgba,
                width,
                height,
            }),
            image_id: RefCell::new(None),
        }
    }

    fn ensure_uploaded(&self, canvas: &mut Canvas<OpenGl>) -> Result<ImageId> {
        if let Some(id) = *self.image_id.borrow() {
            return Ok(id);
        }
        let id = canvas.create_image_empty(
            self.data.width as usize,
            self.data.height as usize,
            PixelFormat::Rgba8,
            ImageFlags::empty(),
        )?;
        // SAFETY: `rgba` is a tightly-packed RGBA8 buffer of
        // `width * height * 4` bytes — `align_to::<RGBA8>` is a no-op
        // reinterpret on the same memory because `RGBA8` is `#[repr(C)]`
        // of four `u8`s. We allocated the buffer ourselves in
        // `from_pixbuf`, so prefix/suffix slices are guaranteed empty.
        let pixels = unsafe { self.data.rgba.align_to::<RGBA8>().1 };
        let img = Img::new_stride(
            pixels,
            self.data.width as usize,
            self.data.height as usize,
            self.data.width as usize,
        );
        canvas.update_image(id, ImageSource::Rgba(img), 0, 0)?;
        *self.image_id.borrow_mut() = Some(id);
        Ok(id)
    }
}

impl Drawable for PastedImage {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn kind_label(&self) -> &'static str {
        "Image"
    }

    fn icon_name(&self) -> &'static str {
        "image-regular"
    }

    fn draw(
        &self,
        canvas: &mut Canvas<OpenGl>,
        _font: FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        let id = self.ensure_uploaded(canvas)?;
        let x = self.pos.x;
        let y = self.pos.y;
        let w = self.size.x;
        let h = self.size.y;
        let mut path = Path::new();
        path.rect(x, y, w, h);
        canvas.fill_path(&path, &Paint::image(id, x, y, w, h, 0.0, 1.0));
        Ok(())
    }

    fn bounds(&self) -> Option<Rect> {
        Some(Rect::new(self.pos, self.size))
    }

    fn hit_test(&self, point: Vec2D, tolerance: f32) -> bool {
        Rect::new(self.pos, self.size)
            .inflated(tolerance)
            .contains(point)
    }

    fn translate(&mut self, delta: Vec2D) {
        self.pos += delta;
    }

    fn handles(&self) -> Vec<Handle> {
        bbox_handles(Rect::new(self.pos, self.size))
    }

    fn move_handle(&mut self, handle: HandleId, to: Vec2D) {
        let cur = Rect::new(self.pos, self.size);
        let new = bbox_resize(cur, handle, to);
        // Clamp to a minimum of 1px so the user can't make the image
        // disappear into a degenerate sliver they then can't grab.
        self.pos = new.pos;
        self.size = Vec2D::new(new.size.x.abs().max(1.0), new.size.y.abs().max(1.0));
    }

    fn render_glow(
        &self,
        canvas: &mut Canvas<OpenGl>,
        _font: FontId,
        _bounds: (Vec2D, Vec2D),
        device_pixel_ratio: f32,
    ) -> Result<()> {
        let halo = halo_in_image_units(canvas, device_pixel_ratio);
        let inflate = halo / 2.0;
        let mut path = Path::new();
        path.rect(
            self.pos.x - inflate,
            self.pos.y - inflate,
            self.size.x + inflate * 2.0,
            self.size.y + inflate * 2.0,
        );
        let mut paint = Paint::color(GLOW_COLOR);
        paint.set_line_width(halo);
        paint.set_line_join(femtovg::LineJoin::Round);
        canvas.stroke_path(&path, &paint);
        Ok(())
    }
}
