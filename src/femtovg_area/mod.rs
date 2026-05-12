mod imp;

use std::{cell::RefCell, rc::Rc, sync::OnceLock};

use femtovg::FontId;
use gtk::glib;
use relm4::gtk::gdk_pixbuf::{Pixbuf, glib::subclass::types::ObjectSubclassIsExt};
use relm4::{
    Sender,
    gtk::{
        self,
        prelude::{GLAreaExt, WidgetExt},
        subclass::prelude::GLAreaImpl,
    },
};

use crate::{
    configuration::Action,
    math::Vec2D,
    sketch_board::SketchBoardInput,
    tools::{CropTool, Drawable, DrawableId, DrawableStore, Tool},
};

static FONT_STACK: OnceLock<Vec<FontId>> = OnceLock::new();

pub fn set_font_stack(fonts: Vec<FontId>) {
    let _ = FONT_STACK.set(fonts);
}

pub fn font_stack() -> &'static [FontId] {
    FONT_STACK.get().map(Vec::as_slice).unwrap_or(&[])
}

thread_local! {
    /// Device pixel ratio published by the renderer at the start of
    /// every frame. Drawables consult this to size UI affordances
    /// (handles, outlines) in CSS pixels — `Drawable::draw` doesn't
    /// receive DPR as a parameter, and threading it through every
    /// impl just for the text/cursor case would be noisy. The
    /// thread-local is set in `imp::FemtoVgAreaMut::render_*` and
    /// read by drawables that need CSS-pixel sizing inside `draw`.
    static CURRENT_DPR: std::cell::Cell<f32> = const { std::cell::Cell::new(1.0) };
}

/// Read the most recently-published device pixel ratio. Used inside
/// `Drawable::draw` impls to size handles/outlines in CSS pixels.
pub fn current_device_pixel_ratio() -> f32 {
    CURRENT_DPR.with(|c| c.get())
}

/// Publish the device pixel ratio for the current frame. Called by
/// the renderer's `render_framebuffer` / `render_native_resolution`.
pub fn set_current_device_pixel_ratio(dpr: f32) {
    CURRENT_DPR.with(|c| c.set(dpr));
}

thread_local! {
    /// True while the renderer is drawing a selected drawable. Read
    /// inside `Drawable::draw` impls that want to render selection
    /// decorations themselves (e.g. text's blue outline) at the
    /// fresh geometry computed during the same draw — bypassing
    /// the `render_glow` path which fires BEFORE draw and so sees
    /// stale layout caches during a handle drag.
    static CURRENT_SELECTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn current_drawable_is_selected() -> bool {
    CURRENT_SELECTED.with(|c| c.get())
}

pub fn set_current_drawable_is_selected(selected: bool) {
    CURRENT_SELECTED.with(|c| c.set(selected));
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

    pub fn commit(&mut self, drawable: Box<dyn Drawable>) -> DrawableId {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .commit(drawable)
    }
    pub fn modify(&mut self, id: DrawableId, drawable: Box<dyn Drawable>) -> bool {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .modify(id, drawable)
    }
    pub fn delete(&mut self, id: DrawableId) -> bool {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .delete(id)
    }
    pub fn modify_many(&mut self, updates: Vec<(DrawableId, Box<dyn Drawable>)>) -> bool {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .modify_many(updates)
    }
    pub fn delete_many(&mut self, ids: &[DrawableId]) -> bool {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .delete_many(ids)
    }
    pub fn drawables_in_rect(&self, rect: crate::math::Rect) -> Vec<DrawableId> {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .drawables_in_rect(rect)
    }
    pub fn all_drawable_ids(&self) -> Vec<DrawableId> {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .all_drawable_ids()
    }
    pub fn hit_test(&self, point: Vec2D, tolerance: f32) -> Option<DrawableId> {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .hit_test(point, tolerance)
    }
    /// Clone of the drawable with `id`, if any. Used by the pointer tool to grab
    /// a working copy at drag-start.
    pub fn clone_drawable(&self, id: DrawableId) -> Option<Box<dyn Drawable>> {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .drawable(id)
            .map(|d| d.clone_box())
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

    pub fn flip_image_horizontal(&mut self) -> bool {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .flip_image_horizontal()
    }

    pub fn rotate_image_ccw(&mut self) -> Option<(f32, f32)> {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .rotate_image_ccw()
    }

    /// Current image-to-canvas scale factor — image-space lengths
    /// multiplied by this give canvas-pixel sizes. Used by callers
    /// that need to size on-screen UI (cursors, hit-test halos) to
    /// match the rendered geometry.
    pub fn current_render_scale(&self) -> f32 {
        self.imp()
            .inner()
            .as_ref()
            .map(|i| i.effective_scale_or_fallback())
            .unwrap_or(1.0)
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
        pointer_tool: Rc<RefCell<dyn Tool>>,
        background_image: Pixbuf,
    ) {
        self.imp().init(
            sender,
            crop_tool,
            active_tool,
            pointer_tool,
            background_image,
        );
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

    /// Pan by a canvas-space delta — wheel-scroll handler entry point.
    /// `dx`, `dy` are already in canvas pixels (the scroll handler
    /// multiplies wheel ticks by a per-tick step). Triggers a resize
    /// so `update_transformation` clamps the accumulated offset.
    pub fn pan_by(&self, dx: f32, dy: f32) {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .pan_by(dx, dy);
        self.imp().resize(0, 0);
        self.start_spring_back_if_needed();
    }

    /// Spring-back driver: while the pan drag-offset is past its
    /// hard limit, tick `~60 fps` so `update_transformation` can
    /// lerp the offset back inside the limit. The timer self-stops
    /// once the offset is within limits — i.e. the rubber band has
    /// fully recovered.
    fn start_spring_back_if_needed(&self) {
        if self.imp().spring_back_timer.borrow().is_some() {
            return;
        }
        let outside = self
            .imp()
            .inner()
            .as_ref()
            .map(|i| i.drag_offset_overshoots())
            .unwrap_or(false);
        if !outside {
            return;
        }
        let widget = self.clone();
        let id = gtk::glib::timeout_add_local(
            std::time::Duration::from_millis(imp::SPRING_BACK_TICK_MS),
            move || {
                // Each tick: trigger update_transformation (does the
                // spring-back lerp) + queue a fresh draw.
                widget.imp().resize(0, 0);
                widget.queue_render();
                let still_outside = widget
                    .imp()
                    .inner()
                    .as_ref()
                    .map(|i| i.drag_offset_overshoots())
                    .unwrap_or(false);
                if still_outside {
                    gtk::glib::ControlFlow::Continue
                } else {
                    // Once we're back within the hard limit, drop the
                    // stored source id so the next pan can re-arm.
                    *widget.imp().spring_back_timer.borrow_mut() = None;
                    gtk::glib::ControlFlow::Break
                }
            },
        );
        *self.imp().spring_back_timer.borrow_mut() = Some(id);
    }

    /// Apply a scrollbar drag — convert the scrollbar's adjustment
    /// value (offset from the top/left of the scaled image, in
    /// canvas pixels) into our centered drag_offset and rerun the
    /// transform. `is_horizontal` picks which axis.
    pub fn set_pan_from_scrollbar(&self, is_horizontal: bool, value: f32) {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .set_pan_from_scrollbar(is_horizontal, value);
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

    /// Push the current global spotlight darkness into the renderer
    /// so the next frame uses it. Caller is sketch_board, on every
    /// slider change.
    pub fn set_spotlight_darkness(&self, value: f32) {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .set_spotlight_darkness(value);
    }
}

impl DrawableStore for FemtoVGArea {
    fn hit_test(&self, point: Vec2D, tolerance: f32) -> Option<DrawableId> {
        FemtoVGArea::hit_test(self, point, tolerance)
    }

    fn clone_drawable(&self, id: DrawableId) -> Option<Box<dyn Drawable>> {
        FemtoVGArea::clone_drawable(self, id)
    }

    fn drawables_in_rect(&self, rect: crate::math::Rect) -> Vec<DrawableId> {
        FemtoVGArea::drawables_in_rect(self, rect)
    }

    fn all_drawable_ids(&self) -> Vec<DrawableId> {
        FemtoVGArea::all_drawable_ids(self)
    }
}
