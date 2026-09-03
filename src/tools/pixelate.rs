use super::{
    Drawable, DrawableClone, InputContext, RenderingMode, Tool, ToolUpdateResult, Tools,
    hit_test_rectangle,
};
use crate::tools::drag_box::{DragBox, draw_center_marker};
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
use relm4::adw::gdk::ModifierType;
use relm4::gtk::gdk::Cursor;
use relm4::gtk::prelude::WidgetExt;
use relm4::{Sender, gtk, gtk::gdk::Key};
use std::cell::{Cell, RefCell};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PixelateMode {
    #[default]
    Pixelate,
    FringePixelate,
    Fringe,
}

#[derive(Clone, Debug)]
pub struct Pixelate {
    origin: Vec2D,
    top_left: Vec2D,
    size: Option<Vec2D>,
    style: Style,
    centered: bool,
    editing: bool,
    mode: PixelateMode,
    cached_image: RefCell<Option<(ImageId, Vec2D, Vec2D)>>,
    renderable: Cell<bool>,
}

impl Pixelate {
    fn is_pixelate(&self) -> bool {
        matches!(
            self.mode,
            PixelateMode::Pixelate | PixelateMode::FringePixelate
        )
    }

    fn is_fringe(&self) -> bool {
        matches!(
            self.mode,
            PixelateMode::Fringe | PixelateMode::FringePixelate
        )
    }

    fn calculate_shape(&mut self, pos: Vec2D, modifier: ModifierType) {
        let drag_box = DragBox::from_origin_delta(self.origin, pos, modifier);
        self.centered = drag_box.centered;
        self.top_left = drag_box.top_left;
        self.size = Some(drag_box.size);
    }

    fn pixelate(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        pos: Vec2D,
        size: Vec2D,
    ) -> Result<Option<(ImageId, Vec2D, Vec2D)>> {
        let transform = canvas.transform();
        let transformed_pos = transform.transform_point(pos.x, pos.y);
        let average_scale = transform.average_scale();
        let transformed_size = size * average_scale;

        let blocksize = self
            .style
            .size
            .to_blocksize(self.style.annotation_size_factor);

        let pos_x = transformed_pos.0.round() as usize;
        let pos_y = transformed_pos.1.round() as usize;
        let width = (transformed_size.x.round() as usize / blocksize) * blocksize;
        let height = (transformed_size.y.round() as usize / blocksize) * blocksize;

        if width == 0 || height == 0 {
            return Ok(None);
        }

        let img = canvas.screenshot()?;
        let buf = if self.is_fringe() {
            Self::fill_area_from_fringes(canvas, pos_x, pos_y, width, height)?
        } else {
            let (buf, _, _) = img
                .sub_image(pos_x, pos_y, width, height)
                .to_contiguous_buf();
            Some(Cow::Owned(buf.into_owned()))
        };

        let Some(b) = buf else {
            return Ok(None);
        };

        let dest_img = if self.is_pixelate() {
            match Self::pixelate_regular(b, width, height, blocksize)? {
                Some(img) => img,
                None => return Ok(None),
            }
        } else {
            Img::new(b.into_owned(), width, height)
        };

        let dst_image_id = canvas.create_image(dest_img.as_ref(), ImageFlags::empty())?;
        let origin = transform.transform_point(0.0, 0.0);
        let paint_pos = Vec2D::new(
            (pos_x as f32 - origin.0) / average_scale,
            (pos_y as f32 - origin.1) / average_scale,
        );
        let paint_size = Vec2D::new(width as f32 / average_scale, height as f32 / average_scale);
        Ok(Some((dst_image_id, paint_pos, paint_size)))
    }

    fn fill_area_from_fringes(
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        pos_x: usize,
        pos_y: usize,
        width: usize,
        height: usize,
    ) -> Result<Option<Cow<'_, [RGBA8]>>> {
        let img = canvas.screenshot()?;

        /*eprintln!(
            "pos_x={pos_x}, pos_y={pos_y}, width={width}, height={height}, img: {:?} {:?}",
            img.width(),
            img.height()
        );*/

        let buf_north = if pos_y >= 1 {
            let (b, _, _) = img
                .sub_image(pos_x, pos_y - 1, width, 1)
                .to_contiguous_buf();
            b
        } else {
            Cow::Owned(vec![RGBA8::new(128, 128, 128, 128); width])
        };
        let buf_south = if pos_y + height < canvas.height() as usize {
            let (b, _, _) = img
                .sub_image(pos_x, pos_y + height, width, 1)
                .to_contiguous_buf();
            b
        } else {
            Cow::Owned(vec![RGBA8::new(128, 128, 128, 128); width])
        };
        let buf_west = if pos_x >= 1 {
            let (b, _, _) = img
                .sub_image(pos_x - 1, pos_y, 1, height)
                .to_contiguous_buf();
            b
        } else {
            Cow::Owned(vec![RGBA8::new(128, 128, 128, 128); height])
        };
        let buf_east = if pos_x + width <= canvas.width() as usize {
            let (b, _, _) = img
                .sub_image(pos_x + width, pos_y, 1, height)
                .to_contiguous_buf();
            b
        } else {
            Cow::Owned(vec![RGBA8::new(128, 128, 128, 128); height])
        };

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
        blocksize: usize,
    ) -> Result<Option<Img<Vec<Rgba<u8>>>>> {
        let mut buf_new = vec![Rgba::new(0, 0, 0, 0); width * height];

        let blocks_x = width / blocksize;
        let blocks_y = height / blocksize;

        for block_y in 0..blocks_y {
            for block_x in 0..blocks_x {
                let x0 = block_x * blocksize;
                let y0 = block_y * blocksize;
                let x1 = x0 + blocksize;
                let y1 = y0 + blocksize;

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
    fn is_renderable(&self) -> bool {
        self.renderable.get()
    }

    fn get_rendering_mode(&self) -> RenderingMode {
        RenderingMode::Blur
    }

    fn bounds(&self) -> Option<(Vec2D, Vec2D)> {
        let size = self.size?;
        Some(math::ensure_bounding_box(
            self.top_left,
            self.top_left + size,
        ))
    }

    fn hit_test(&self, pos: Vec2D, tolerance: f32) -> bool {
        hit_test_rectangle(pos, self.top_left, self.size, tolerance, true)
    }

    fn translate(&mut self, delta: Vec2D) {
        self.top_left += delta;
        // invalidate cached blur image since position changed
        *self.cached_image.borrow_mut() = None;
    }

    fn resize_bounds(&mut self, tl: Vec2D, br: Vec2D) {
        let (tl, br) = math::ensure_bounding_box(tl, br);
        self.top_left = tl;
        self.size = Some(br - tl);
        *self.cached_image.borrow_mut() = None;
    }

    fn get_style(&self) -> Option<&Style> {
        Some(&self.style)
    }

    fn get_style_mut(&mut self) -> Option<&mut Style> {
        *self.cached_image.borrow_mut() = None;
        Some(&mut self.style)
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
        let blocksize = self
            .style
            .size
            .to_blocksize(self.style.annotation_size_factor);
        let (pos, size) = math::rect_ensure_in_bounds(
            math::rect_ensure_positive_size(self.top_left, size),
            bounds,
        );
        let big_enough = if self.is_pixelate() {
            size.x >= blocksize as f32 && size.y >= blocksize as f32
        } else {
            size.x.abs() > f32::EPSILON && size.y.abs() > f32::EPSILON
        };

        let can_perform = big_enough;
        self.renderable.set(can_perform);
        if self.editing {
            if self.centered {
                draw_center_marker(canvas, self.origin);
            }
            // set style
            let mut color = if can_perform {
                Color::white()
            } else {
                Color::rgb(255, 0, 0)
            };
            let border_color = if can_perform {
                Color::black()
            } else {
                Color::rgb(0, 255, 255)
            };
            color.set_alphaf(0.6);
            let paint = Paint::color(color);
            let paint_border = Paint::color(border_color);

            // make rect
            let mut path = Path::new();
            path.rect(pos.x, pos.y, size.x, size.y);

            // draw
            canvas.fill_path(&path, &paint);
            canvas.stroke_path(&path, &paint_border);
        } else {
            if !can_perform {
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

            if let Some((image_id, paint_pos, paint_size)) = *self.cached_image.borrow() {
                let mut path = Path::new();
                path.rect(paint_pos.x, paint_pos.y, paint_size.x, paint_size.y);

                canvas.fill_path(
                    &path,
                    &Paint::image(
                        image_id,
                        paint_pos.x,
                        paint_pos.y,
                        paint_size.x,
                        paint_size.y,
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
    style: Style,
    input_enabled: bool,
    sender: Option<Sender<SketchBoardInput>>,
    mode: PixelateMode,
    cursor_widget: Option<gtk::Widget>,
}

impl PixelateTool {
    pub fn with_mode(mode: PixelateMode) -> Self {
        PixelateTool {
            pixelate: None,
            style: Style::default(),
            input_enabled: false,
            sender: None,
            mode,
            cursor_widget: None,
        }
    }

    fn update_drag_cursor(&self) {
        let Some(widget) = &self.cursor_widget else {
            return;
        };

        let Some(p) = self.pixelate.as_ref() else {
            return;
        };

        if p.renderable.get() {
            widget.set_cursor(None);
        } else {
            let cursor = ["not-allowed", "no-drop"]
                .iter()
                .find_map(|name| Cursor::from_name(name, None));
            widget.set_cursor(cursor.as_ref());
        }
    }

    fn clear_cursor(&self) {
        if let Some(widget) = &self.cursor_widget {
            widget.set_cursor(None);
        }
    }
}

impl Tool for PixelateTool {
    fn input_enabled(&self) -> bool {
        self.input_enabled
    }

    fn set_input_enabled(&mut self, value: bool) {
        self.input_enabled = value;
    }

    fn get_tool_type(&self) -> super::Tools {
        match self.mode {
            PixelateMode::Pixelate => Tools::Pixelate,
            PixelateMode::FringePixelate => Tools::FringePixelate,
            PixelateMode::Fringe => Tools::Fringe,
        }
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        match event.type_ {
            MouseEventType::BeginDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                // start new
                self.pixelate = Some(Pixelate {
                    origin: event.pos,
                    top_left: event.pos,
                    size: None,
                    centered: false,
                    editing: true,
                    style: self.style,
                    cached_image: RefCell::new(None),
                    mode: self.mode,
                    renderable: Cell::new(false),
                });

                ToolUpdateResult::Redraw
            }
            MouseEventType::EndDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                self.clear_cursor();
                if let Some(a) = &mut self.pixelate {
                    a.calculate_shape(event.pos, event.modifier);
                    if event.pos == Vec2D::zero() || !a.renderable.get() {
                        self.pixelate = None;
                        ToolUpdateResult::Redraw
                    } else {
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
                    a.calculate_shape(event.pos, event.modifier);
                    self.update_drag_cursor();

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

    fn handle_style_event(&mut self, style: Style) -> ToolUpdateResult {
        self.style = style;
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

    fn active(&self) -> bool {
        self.pixelate.is_some()
    }

    fn set_im_context(&mut self, context: Option<InputContext>) {
        self.cursor_widget = context.map(|ctx| ctx.widget);
        self.clear_cursor();
    }
}
