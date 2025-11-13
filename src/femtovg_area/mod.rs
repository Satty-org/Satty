mod imp;

use std::{cell::RefCell, rc::Rc};

use gdk_pixbuf::{glib::subclass::types::ObjectSubclassIsExt, Pixbuf};
use gtk::glib;
use relm4::{
    gtk::{self, prelude::WidgetExt, subclass::prelude::GLAreaImpl},
    Sender,
};

use crate::{
    configuration::Action,
    math::Vec2D,
    sketch_board::SketchBoardInput,
    tools::{CropTool, Drawable, Tool},
};

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
    pub fn reset(&mut self) -> bool {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .reset()
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
}
