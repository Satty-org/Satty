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
    pub fn modify_coalesce(&mut self, id: DrawableId, drawable: Box<dyn Drawable>) -> bool {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .modify_coalesce(id, drawable)
    }
    pub fn modify_many_coalesce(&mut self, updates: Vec<(DrawableId, Box<dyn Drawable>)>) -> bool {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .modify_many_coalesce(updates)
    }
    pub fn delete_many(&mut self, ids: &[DrawableId]) -> bool {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .delete_many(ids)
    }
    pub fn reorder_to_top_coalesce(&mut self, id: DrawableId) -> bool {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .reorder_to_top_coalesce(id)
    }
    pub fn drawable_flags(&self, id: DrawableId) -> Option<(bool, bool)> {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .drawable_flags(id)
    }
    pub fn set_drawable_flags(&mut self, id: DrawableId, visible: bool, locked: bool) -> bool {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .set_drawable_flags(id, visible, locked)
    }
    pub fn move_drawable_up(&mut self, id: DrawableId) -> bool {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .move_drawable_up(id)
    }
    pub fn move_drawable_down(&mut self, id: DrawableId) -> bool {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .move_drawable_down(id)
    }
    pub fn move_drawable_to_top(&mut self, id: DrawableId) -> bool {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .move_drawable_to_top(id)
    }
    pub fn move_drawable_to_bottom(&mut self, id: DrawableId) -> bool {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .move_drawable_to_bottom(id)
    }
    pub fn reorder_to(&mut self, new_order: Vec<DrawableId>) -> bool {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .reorder_to(new_order)
    }
    pub fn drawable_custom_name(&self, id: DrawableId) -> Option<String> {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .drawable_custom_name(id)
    }
    pub fn drawable_auto_label_index(&self, id: DrawableId) -> Option<u32> {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .drawable_auto_label_index(id)
    }
    pub fn set_drawable_custom_name(&mut self, id: DrawableId, name: Option<String>) -> bool {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .set_drawable_custom_name(id, name)
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
    pub fn has_visible_overlapper_above(&self, id: DrawableId) -> bool {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .has_visible_overlapper_above(id)
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

    /// Re-fit the background image around the original screenshot
    /// plus the current drawable bounds. Grows the canvas when a
    /// drawable spills past the edge (filling new strips with the
    /// dominant color of the corresponding edge of the existing
    /// image), shrinks it back toward the original when no drawable
    /// still needs the extension. Returns the new `(width, height)`
    /// if a resize happened, `None` otherwise. The `ids_to_exclude`
    /// list names drawables whose just-pushed Add/Modify/Remove
    /// carries pre-translation state — the caller passes the ids it
    /// just touched.
    pub fn auto_resize_for_drawables(
        &mut self,
        ids_to_exclude: &[DrawableId],
    ) -> Option<(f32, f32)> {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .auto_resize_for_drawables(ids_to_exclude)
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

    pub fn resize_image(&mut self, new_w: i32, new_h: i32) -> Option<(f32, f32)> {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .resize_image(new_w, new_h)
    }

    pub fn image_dimensions(&self) -> (i32, i32) {
        self.imp()
            .inner()
            .as_ref()
            .map(|i| i.image_dimensions())
            .unwrap_or((0, 0))
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

    /// The renderer's current image→canvas transform — (effective_scale,
    /// effective_offset). The crop tool reads the scale on activation
    /// and after transform-changing gestures to keep its handle
    /// hit-testing screen-constant.
    pub fn render_transform(&self) -> (f32, Vec2D) {
        self.imp()
            .inner()
            .as_ref()
            .map(|i| i.render_transform())
            .unwrap_or((1.0, Vec2D::zero()))
    }

    /// Synchronously re-run `update_transformation` (via the widget's
    /// resize path) so a subsequent read of `render_transform()`
    /// reflects state changes made just prior. The crop tool's
    /// activation hook uses this so the scale it samples matches the
    /// post-handle_activated view (which may have just flipped a
    /// committed crop back to uncommitted, switching from zoomed-into-
    /// crop to full-image).
    pub fn refresh_transform(&self) {
        self.imp().resize(0, 0);
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

    /// Zoom by `factor` anchored on the last-known cursor position
    /// (tracked by the Motion controller via `set_pointer_offset`).
    /// Used by the canvas wheel-zoom path so the image scales around
    /// whatever the user is hovering over, instead of jumping toward
    /// the canvas center.
    ///
    /// Also runs `resize(0, 0)` to flush the new `zoom_scale` through
    /// `update_transformation` immediately, so `effective_scale` /
    /// `effective_offset` reflect the new zoom before this call
    /// returns rather than on the next render tick.
    pub fn set_zoom_scale_at_cursor(&self, factor: f32) {
        let anchor = self
            .imp()
            .inner()
            .as_ref()
            .expect("Did you call init before using FemtoVgArea?")
            .pointer_offset();
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .set_zoom_scale_at(factor, false, anchor);
        self.imp().resize(0, 0);
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

    pub fn spotlight_darkness(&self) -> f32 {
        self.imp()
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .spotlight_darkness()
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

    fn has_visible_overlapper_above(&self, id: DrawableId) -> bool {
        FemtoVGArea::has_visible_overlapper_above(self, id)
    }

    fn is_drawable_locked(&self, id: DrawableId) -> bool {
        FemtoVGArea::drawable_flags(self, id)
            .map(|(_visible, locked)| locked)
            .unwrap_or(false)
    }
}
