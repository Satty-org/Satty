mod imp;

use std::{cell::RefCell, rc::Rc, sync::OnceLock};

use anyhow::Result;
use femtovg::{
    Canvas, FontId, ImageFlags, ImageId, ImageSource, PixelFormat,
    imgref::Img,
    renderer::OpenGl,
    rgb::{RGB, RGBA},
};
use gtk::glib;
use relm4::gtk::gdk_pixbuf::{Pixbuf, glib::subclass::types::ObjectSubclassIsExt};
use relm4::{
    Sender,
    gtk::{self, prelude::WidgetExt, subclass::prelude::GLAreaImpl},
};

use crate::{
    configuration::Action,
    math::Vec2D,
    sketch_board::SketchBoardInput,
    tools::{CropTool, Drawable, Tool},
};

static FONT_STACK: OnceLock<Vec<FontId>> = OnceLock::new();

pub fn set_font_stack(fonts: Vec<FontId>) {
    let _ = FONT_STACK.set(fonts);
}

pub fn font_stack() -> &'static [FontId] {
    FONT_STACK.get().map(Vec::as_slice).unwrap_or(&[])
}

pub fn create_image_from_pixbuf(canvas: &mut Canvas<OpenGl>, image: &Pixbuf) -> Result<ImageId> {
    let format = if image.has_alpha() {
        PixelFormat::Rgba8
    } else {
        PixelFormat::Rgb8
    };

    let image_id = canvas.create_image_empty(
        image.width() as usize,
        image.height() as usize,
        format,
        ImageFlags::empty(),
    )?;

    // extract values
    let width = image.width() as usize;
    let stride = image.rowstride() as usize; // stride is in bytes per row
    let height = image.height() as usize;
    let bytes_per_pixel = if image.has_alpha() { 4 } else { 3 }; // pixbuf supports rgb or rgba

    unsafe {
        let src_buffer = image.pixels();

        let row_length = width * bytes_per_pixel;
        let mut dst_buffer = if row_length == stride {
            // stride == row_length, there are no additional bytes after the end of each row
            src_buffer.to_vec()
        } else {
            // stride != row_length, there are additional bytes after the end of each row that
            // need to be truncated. We copy row by row..
            let mut dst_buffer = Vec::<u8>::with_capacity(width * height * bytes_per_pixel);

            for row in 0..height {
                let src_offset = row * stride;
                dst_buffer.extend_from_slice(&src_buffer[src_offset..src_offset + row_length]);
            }
            dst_buffer
        };

        // in almost all cases, that should be a no-op. Buf we might have additional elements after the
        // end of the buffer, e.g. after width * height * bytes_per_pixel
        dst_buffer.truncate(width * height * bytes_per_pixel);

        if image.has_alpha() {
            let img = Img::new_stride(
                dst_buffer.align_to::<RGBA<u8>>().1.to_vec(),
                width,
                height,
                width,
            );

            canvas.update_image(image_id, ImageSource::Rgba(img.as_ref()), 0, 0)?;
        } else {
            let img = Img::new_stride(
                dst_buffer.align_to::<RGB<u8>>().1.to_owned(),
                width,
                height,
                width,
            );

            canvas.update_image(image_id, ImageSource::Rgb(img.as_ref()), 0, 0)?;
        }
    }

    Ok(image_id)
}

glib::wrapper! {
    pub struct FemtoVGArea(ObjectSubclass<imp::FemtoVGArea>)
        @extends gtk::Widget, gtk::GLArea,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for FemtoVGArea {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl FemtoVGArea {
    pub fn set_active_tool(&mut self, active_tool: Rc<RefCell<dyn Tool>>) {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .set_active_tool(active_tool);
    }

    pub fn commit(&mut self, drawable: Box<dyn Drawable>) {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .commit(drawable);
    }
    pub fn undo(&mut self) -> bool {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .undo()
    }
    pub fn redo(&mut self) -> bool {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .redo()
    }
    pub fn request_render(&self, actions: &[Action]) {
        self.imp().request_render(actions);
    }
    pub fn clear_all(&mut self) -> bool {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .clear_all()
    }

    pub fn abs_canvas_to_image_coordinates(&self, input: Vec2D) -> Vec2D {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .abs_canvas_to_image_coordinates(input, self.scale_factor() as f32)
    }

    pub fn rel_canvas_to_image_coordinates(&self, input: Vec2D) -> Vec2D {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .rel_canvas_to_image_coordinates(input, self.scale_factor() as f32)
    }

    pub fn init(
        &mut self,
        sender: Sender<SketchBoardInput>,
        crop_tool: Rc<RefCell<CropTool>>,
        active_tool: Rc<RefCell<dyn Tool>>,
        background_image: Pixbuf,
    ) {
        self.imp()
            .init(sender, crop_tool, active_tool, background_image);
    }

    pub fn set_zoom_scale(&self, factor: f32) {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .set_zoom_scale(factor, false);
        //trigger resize to recalculate zoom
        self.imp().resize(0, 0);
    }

    pub fn set_pointer_offset(&self, offset: Vec2D) {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .set_pointer_offset(offset * self.scale_factor() as f32);
    }

    pub fn set_pointer_offset_center(&self) {
        let center = Vec2D::new(
            self.allocated_width() as f32 / 2.0,
            self.allocated_height() as f32 / 2.0,
        );
        self.set_pointer_offset(center);
    }

    pub fn set_drag_offset(&self, offset: Vec2D) {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .set_drag_offset(offset * self.scale_factor() as f32);
        //trigger resize to recalculate offset
        self.imp().resize(0, 0);
    }

    pub fn store_last_offset(&self) {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .store_last_offset();
    }

    pub fn set_is_drag(&self, is_drag: bool) {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .set_is_drag(is_drag);
    }

    pub fn reset_size(&self, factor: f32) {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .set_zoom_scale(factor, true);
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .reset_drag_offset();
        //trigger resize to reset
        self.imp().resize(0, 0);
    }

    pub fn resize(&self, width: i32, height: i32) {
        self.imp().resize(width, height);
    }
}
