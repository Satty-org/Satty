use std::cell::RefCell;

use crate::{
    math::{self, Vec2D},
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    style::Style,
    tools::Cow,
};
use anyhow::Result;
use femtovg::imgref::Img;
use femtovg::rgb::RGBA8;
use femtovg::{Color, ImageFlags, ImageId, Paint, Path, rgb::Rgba};
use relm4::gtk::gdk::ModifierType;
use relm4::{Sender, gtk::gdk::Key};

use super::{Drawable, DrawableClone, Tool, ToolUpdateResult, Tools};

static BLOCKSIZE: usize = 32;

#[derive(Clone, Debug)]
pub struct Pixelate {
    top_left: Vec2D,
    size: Option<Vec2D>,
    editing: bool,
    independent_mode: bool,
    cached_image: RefCell<Option<ImageId>>,
}

impl Pixelate {
    fn pixelate(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        pos: Vec2D,
        size: Vec2D,
    ) -> Result<Option<ImageId>> {
        let transformed_pos = canvas.transform().transform_point(pos.x, pos.y);
        let transformed_size = size * canvas.transform().average_scale();

        let pos_x = transformed_pos.0 as usize;
        let pos_y = transformed_pos.1 as usize;
        let width = (transformed_size.x as usize / BLOCKSIZE) * BLOCKSIZE;
        let height = (transformed_size.y as usize / BLOCKSIZE) * BLOCKSIZE;

        if width == 0 || height == 0 {
            return Ok(None);
        }

        let img = canvas.screenshot()?;
        let buf = if self.independent_mode {
            Self::fill_area_from_fringes(canvas, pos_x, pos_y, width, height)?
        } else {
            let (buf, _, _) = img
                .sub_image(pos_x, pos_y, width, height)
                .to_contiguous_buf();
            Some(buf)
        };

        if let Some(b) = buf
            && let Some(dest_img) = Self::pixelate_regular(b, width, height)?
        {
            let dst_image_id = canvas.create_image(dest_img.as_ref(), ImageFlags::empty())?;
            Ok(Some(dst_image_id))
        } else {
            Ok(None)
        }
    }

    fn fill_area_from_fringes(
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        pos_x: usize,
        pos_y: usize,
        width: usize,
        height: usize,
    ) -> Result<Option<Cow<'_, [RGBA8]>>> {
        //TODO: missing fringe, no luck!
        if pos_x < 1
            || pos_y < 1
            || canvas.width() as usize <= pos_x + width
            || canvas.height() as usize <= pos_y + height
        {
            return Ok(None);
        }

        let img = canvas.screenshot()?;

        let (buf_north, _, _) = img
            .sub_image(pos_x, pos_y - 1, width, 1)
            .to_contiguous_buf();
        let (buf_south, _, _) = img
            .sub_image(pos_x, pos_y + height + 1, width, 1)
            .to_contiguous_buf();
        let (buf_west, _, _) = img
            .sub_image(pos_x - 1, pos_y, 1, height)
            .to_contiguous_buf();
        let (buf_east, _, _) = img
            .sub_image(pos_x + width + 1, pos_y, 1, height)
            .to_contiguous_buf();

        let mut buf_new = vec![Rgba::new(0, 0, 0, 0); width * height];

        for y in 0..height {
            for x in 0..width {
                let pix_north = buf_north[x];
                let pix_south = buf_south[x];
                let pix_west = buf_west[y];
                let pix_east = buf_east[y];

                let weight_n: f32 = (height - y) as f32 / (height as f32);
                let weight_s: f32 = y as f32 / (height as f32);
                let weight_w: f32 = (width - x) as f32 / (width as f32);
                let weight_e: f32 = x as f32 / (width as f32);

                let new_pixel = RGBA8 {
                    r: ((pix_north.r as f32 * weight_n
                        + pix_south.r as f32 * weight_s
                        + pix_west.r as f32 * weight_w
                        + pix_east.r as f32 * weight_e)
                        / 2.0) as u8,
                    g: ((pix_north.g as f32 * weight_n
                        + pix_south.g as f32 * weight_s
                        + pix_west.g as f32 * weight_w
                        + pix_east.g as f32 * weight_e)
                        / 2.0) as u8,
                    b: ((pix_north.b as f32 * weight_n
                        + pix_south.b as f32 * weight_s
                        + pix_west.b as f32 * weight_w
                        + pix_east.b as f32 * weight_e)
                        / 2.0) as u8,
                    a: 255,
                };

                buf_new[y * width + x] = new_pixel;
            }
        }

        Ok(Some(buf_new.into()))
    }

    fn pixelate_regular(
        input_buf: Cow<[RGBA8]>,
        width: usize,
        height: usize,
    ) -> Result<Option<Img<Vec<Rgba<u8>>>>> {
        let mut buf_new = vec![Rgba::new(0, 0, 0, 0); width * height];

        let blocks_x = width / BLOCKSIZE;
        let blocks_y = height / BLOCKSIZE;

        for block_y in 0..blocks_y {
            for block_x in 0..blocks_x {
                let x0 = block_x * BLOCKSIZE;
                let y0 = block_y * BLOCKSIZE;
                let x1 = x0 + BLOCKSIZE;
                let y1 = y0 + BLOCKSIZE;

                let mut r: u64 = 0;
                let mut g: u64 = 0;
                let mut b: u64 = 0;
                let mut counter = 0;
                for y in y0..y1 {
                    for x in x0..x1 {
                        let pixel = input_buf[x + y * width];
                        r += pixel.r as u64;
                        g += pixel.g as u64;
                        b += pixel.b as u64;
                        counter += 1;
                    }
                }
                counter = counter.max(1);

                let new_pixel = RGBA8 {
                    r: (r / counter) as u8,
                    g: (g / counter) as u8,
                    b: (b / counter) as u8,
                    a: 255,
                };

                for y in y0..y1 {
                    for x in x0..x1 {
                        buf_new[y * width + x] = new_pixel;
                    }
                }
            }
        }

        let dst_image = Img::new(buf_new, width, height);
        Ok(Some(dst_image))
    }
}

impl Drawable for Pixelate {
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
            let mut color = if self.independent_mode {
                Color::white()
            } else {
                Color::black()
            };
            color.set_alphaf(0.6);
            let paint = Paint::color(color);

            // make rect
            let mut path = Path::new();
            path.rect(pos.x, pos.y, size.x, size.y);

            // draw
            canvas.fill_path(&path, &paint);
        } else {
            if size.x < BLOCKSIZE as f32 || size.y < BLOCKSIZE as f32 {
                return Ok(());
            }

            canvas.save();
            canvas.flush();

            // create new cached image
            if self.cached_image.borrow().is_none()
                && let Some(x) = self.pixelate(canvas, pos, size)?
            {
                self.cached_image.borrow_mut().replace(x);
            }

            if self.cached_image.borrow().is_some() {
                let mut path = Path::new();
                path.rect(pos.x, pos.y, size.x, size.y);

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
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct PixelateTool {
    pixelate: Option<Pixelate>,
    input_enabled: bool,
    sender: Option<Sender<SketchBoardInput>>,
}

impl Tool for PixelateTool {
    fn input_enabled(&self) -> bool {
        self.input_enabled
    }

    fn set_input_enabled(&mut self, value: bool) {
        self.input_enabled = value;
    }

    fn get_tool_type(&self) -> super::Tools {
        Tools::Pixelate
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        match event.type_ {
            MouseEventType::BeginDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                // start new
                self.pixelate = Some(Pixelate {
                    top_left: event.pos,
                    size: None,
                    editing: true,
                    independent_mode: event.modifier.intersects(ModifierType::ALT_MASK),
                    cached_image: RefCell::new(None),
                });

                ToolUpdateResult::Redraw
            }
            MouseEventType::EndDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                if let Some(a) = &mut self.pixelate {
                    if event.pos == Vec2D::zero() {
                        self.pixelate = None;

                        ToolUpdateResult::Redraw
                    } else {
                        a.size = Some(event.pos);
                        a.independent_mode = event.modifier.intersects(ModifierType::ALT_MASK);
                        a.editing = false;

                        let result = a.clone_box();
                        self.pixelate = None;

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

                if let Some(a) = &mut self.pixelate {
                    if event.pos == Vec2D::zero() {
                        return ToolUpdateResult::Unmodified;
                    }
                    a.independent_mode = event.modifier.intersects(ModifierType::ALT_MASK);
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
        if event.key == Key::Escape && self.pixelate.is_some() {
            self.pixelate = None;
            ToolUpdateResult::Redraw
        } else {
            ToolUpdateResult::Unmodified
        }
    }

    fn handle_style_event(&mut self, _style: Style) -> ToolUpdateResult {
        ToolUpdateResult::Unmodified
    }

    fn get_drawable(&self) -> Option<&dyn Drawable> {
        match &self.pixelate {
            Some(d) => Some(d),
            None => None,
        }
    }

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>) {
        self.sender = Some(sender);
    }
}
