use anyhow::Result;
use glow::HasContext;
use std::{
    cell::{RefCell, RefMut},
    collections::HashSet,
    num::NonZeroU32,
    path::PathBuf,
    rc::Rc,
};

use femtovg::{
    Canvas, FontId, ImageFlags, ImageId, ImageSource, Paint, Path, PixelFormat, Transform2D,
    imgref::{Img, ImgVec},
    renderer,
    rgb::{RGB, RGBA, RGBA8},
};
use fontconfig::Fontconfig;
use gtk::{glib, prelude::*, subclass::prelude::*};
use relm4::gtk::gdk_pixbuf::Pixbuf;
use relm4::{Sender, gtk};
use resource::resource;

use crate::{
    APP_CONFIG,
    configuration::Action,
    math::{Vec2D, rect_ensure_in_bounds, rect_round},
    sketch_board::SketchBoardInput,
    tools::{CropTool, Drawable, DrawableId, Stacked, Tool, UndoAction},
};

use super::{font_stack, set_font_stack};

const TRANSPARENCY_SQUARE_SIZE: usize = 64;

#[derive(Default)]
pub struct FemtoVGArea {
    canvas: RefCell<Option<femtovg::Canvas<femtovg::renderer::OpenGl>>>,
    font: RefCell<Option<FontId>>,
    inner: RefCell<Option<FemtoVgAreaMut>>,
    request_render: RefCell<Option<Vec<Action>>>,
    sender: RefCell<Option<Sender<SketchBoardInput>>>,
}

pub struct FemtoVgAreaMut {
    background_image: Pixbuf,
    background_image_id: Option<femtovg::ImageId>,
    transparent_background_id: Option<femtovg::ImageId>,
    active_tool: Rc<RefCell<dyn Tool>>,
    /// The pointer tool is consulted alongside the active tool so implicit
    /// selection (clicking a shape while a drawing tool is active) renders
    /// handles, glow, and live drag visuals.
    pointer_tool: Rc<RefCell<dyn Tool>>,
    crop_tool: Rc<RefCell<CropTool>>,
    scale_factor: f32,
    offset: Vec2D,
    drawables: Vec<Stacked>,
    undo_stack: Vec<UndoAction>,
    redo_stack: Vec<UndoAction>,
    next_drawable_id: u64,
    zoom_scale: f32,
    last_scale: f32,
    pointer_offset: Vec2D,
    last_offset: Vec2D,
    drag_offset: Vec2D,
    is_drag: bool,
    is_reset: bool,
    /// Device pixel ratio of the host display (1 on standard DPI, 2 on
    /// retina). Updated on `resize`. Used so per-frame UI elements
    /// (selection handles) can render at constant CSS-pixel size while
    /// still looking sharp on HiDPI screens.
    device_pixel_ratio: f32,
}

#[glib::object_subclass]
impl ObjectSubclass for FemtoVGArea {
    const NAME: &'static str = "FemtoVGArea";
    type Type = super::FemtoVGArea;
    type ParentType = gtk::GLArea;
}

impl ObjectImpl for FemtoVGArea {
    fn constructed(&self) {
        self.parent_constructed();
        let area = self.obj();
        area.set_has_stencil_buffer(true);
        area.queue_render();
    }
}

impl WidgetImpl for FemtoVGArea {
    fn realize(&self) {
        self.parent_realize();
    }
    fn unrealize(&self) {
        self.obj().make_current();
        self.canvas.borrow_mut().take();
        self.parent_unrealize();
    }
}

impl GLAreaImpl for FemtoVGArea {
    fn resize(&self, width: i32, height: i32) {
        self.ensure_canvas();

        let mut bc = self.canvas.borrow_mut();
        let canvas = bc.as_mut().unwrap(); // this unwrap is safe as long as we call "ensure_canvas" before

        let w = canvas.width();
        let h = canvas.height();

        let dpr = self.obj().scale_factor() as f32;
        canvas.set_size(
            if width == 0 { w } else { width as u32 },
            if height == 0 { h } else { height as u32 },
            dpr,
        );

        // update scale factor
        let mut inner_ref = self.inner();
        let inner = inner_ref
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?");
        inner.device_pixel_ratio = dpr;
        inner.update_transformation(canvas);
    }
    fn render(&self, _context: &gtk::gdk::GLContext) -> glib::Propagation {
        self.ensure_canvas();

        let mut bc = self.canvas.borrow_mut();
        let canvas = bc.as_mut().unwrap(); // this unwrap is safe as long as we call "ensure_canvas" before
        let font = self.font.borrow().unwrap(); // this unwrap is safe as long as we call "ensure_canvas" before
        let mut actions = self.request_render.borrow_mut();

        // if we got requested to render a frame
        if let Some(a) = actions.take() {
            // render image
            let image = match self
                .inner()
                .as_mut()
                .expect("Did you call init before using FemtoVgArea?")
                .render_native_resolution(canvas, font)
            {
                Ok(t) => t,
                Err(e) => {
                    println!("Error while rendering image: {e}");
                    return glib::Propagation::Stop;
                }
            };

            // send result
            self.sender
                .borrow()
                .as_ref()
                .expect("Did you call init before using FemtoVgArea?")
                .emit(SketchBoardInput::RenderResult(image, a));

            // reset request
            *actions = None;
        }
        if let Err(e) = self
            .inner()
            .as_mut()
            .expect("Did you call init before using FemtoVgArea?")
            .render_framebuffer(canvas, font)
        {
            println!("Error rendering to framebuffer: {e}");
        }
        glib::Propagation::Stop
    }
}
impl FemtoVGArea {
    pub fn init(
        &self,
        sender: Sender<SketchBoardInput>,
        crop_tool: Rc<RefCell<CropTool>>,
        active_tool: Rc<RefCell<dyn Tool>>,
        pointer_tool: Rc<RefCell<dyn Tool>>,
        background_image: Pixbuf,
    ) {
        self.inner().replace(FemtoVgAreaMut {
            background_image,
            background_image_id: None,
            transparent_background_id: None,
            active_tool,
            pointer_tool,
            crop_tool,
            scale_factor: 1.0,
            offset: Vec2D::zero(),
            drawables: Vec::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            next_drawable_id: 0,
            zoom_scale: 0.0,
            pointer_offset: Vec2D::zero(),
            last_offset: Vec2D::zero(),
            drag_offset: Vec2D::zero(),
            last_scale: 0.0,
            is_drag: false,
            is_reset: false,
            device_pixel_ratio: 1.0,
        });
        self.sender.borrow_mut().replace(sender);
    }
    fn ensure_canvas(&self) {
        if self.canvas.borrow().is_none() {
            let c = self
                .setup_canvas()
                .expect("Cannot setup renderer and canvas");
            self.canvas.borrow_mut().replace(c);
        }

        if self.font.borrow().is_none()
            && let Some(first) = font_stack().first()
        {
            self.font.borrow_mut().replace(*first);
        }
    }

    fn build_text_context(&self) -> Result<(femtovg::TextContext, Vec<FontId>)> {
        let text_context = femtovg::TextContext::default();
        let mut loaded_fonts = Vec::new();
        let mut loaded_paths = HashSet::<(PathBuf, u32)>::new();

        let app_config = APP_CONFIG.read();
        let fontconfig = Fontconfig::new();

        let mut load_font = |family: &str, style: Option<&str>| -> Result<FontId> {
            let font = fontconfig
                .as_ref()
                .and_then(|fc| fc.find(family, style))
                .ok_or_else(|| anyhow::anyhow!("Font family '{}' not found", family))?;

            let face_index = font.index.unwrap_or(0).max(0) as u32;

            if !loaded_paths.insert((font.path.clone(), face_index)) {
                return Err(anyhow::anyhow!("Font '{}' already loaded", family));
            }
            let data = std::fs::read(&font.path)
                .map_err(|e| anyhow::anyhow!("Failed to read font file: {}", e))?;

            text_context
                .add_shared_font_with_index(data, face_index)
                .map_err(|e| anyhow::anyhow!("Failed to load font: {}", e))
        };

        match load_font(
            app_config.font().family().unwrap_or(""),
            app_config.font().style(),
        ) {
            Ok(id) => {
                loaded_fonts.push(id);
            }
            Err(e) => {
                eprintln!("Primary font: {}", e);
            }
        }

        if loaded_fonts.is_empty() {
            let fallback = text_context
                .add_font_mem(&resource!("src/assets/Roboto-Regular.ttf"))
                .expect("Cannot add font");
            loaded_fonts.push(fallback);
        }

        for family in app_config.font().fallback() {
            match load_font(family, None) {
                Ok(id) => {
                    loaded_fonts.push(id);
                }
                Err(e) => {
                    eprintln!("Fallback font: {}", e);
                }
            }
        }

        Ok((text_context, loaded_fonts))
    }

    fn setup_canvas(&self) -> Result<femtovg::Canvas<femtovg::renderer::OpenGl>> {
        let widget = self.obj();
        widget.attach_buffers();

        static LOAD_FN: fn(&str) -> *const std::ffi::c_void =
            |s| epoxy::get_proc_addr(s) as *const _;
        // SAFETY: Need to get the framebuffer id that gtk expects us to draw into, so
        // femtovg knows which framebuffer to bind. This is safe as long as we
        // call attach_buffers beforehand. Also unbind it here just in case,
        // since this can be called outside render.
        let (mut renderer, fbo) = unsafe {
            let renderer =
                renderer::OpenGl::new_from_function(LOAD_FN).expect("Cannot create renderer");
            let ctx = glow::Context::from_loader_function(LOAD_FN);
            let id = NonZeroU32::new(ctx.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING) as u32)
                .expect("No GTK provided framebuffer binding");
            ctx.bind_framebuffer(glow::FRAMEBUFFER, None);
            (renderer, glow::NativeFramebuffer(id))
        };
        renderer.set_screen_target(Some(fbo));

        let (text_context, loaded_fonts) = self.build_text_context()?;
        let canvas = Canvas::new_with_text_context(renderer, text_context)?;

        set_font_stack(loaded_fonts.clone());
        if let Some(first) = loaded_fonts.first() {
            self.font.borrow_mut().replace(*first);
        }

        Ok(canvas)
    }

    pub fn inner(&self) -> RefMut<'_, Option<FemtoVgAreaMut>> {
        self.inner.borrow_mut()
    }
    pub fn request_render(&self, actions: &[Action]) {
        self.request_render.borrow_mut().replace(actions.into());
        self.obj().queue_render();
    }
    pub fn set_parent_sender(&self, sender: Sender<SketchBoardInput>) {
        self.sender.borrow_mut().replace(sender);
    }
}

impl FemtoVgAreaMut {
    pub fn commit(&mut self, drawable: Box<dyn Drawable>) -> DrawableId {
        let id = DrawableId(self.next_drawable_id);
        self.next_drawable_id += 1;
        self.drawables.push(Stacked { id, drawable });
        self.undo_stack.push(UndoAction::Add(id));
        self.redo_stack.clear();
        id
    }

    /// Replace the drawable with `id` in-place. Records a Modify undo action.
    /// Returns true if the id was found.
    pub fn modify(&mut self, id: DrawableId, new: Box<dyn Drawable>) -> bool {
        let Some(pos) = self.drawables.iter().position(|s| s.id == id) else {
            return false;
        };
        let prev = std::mem::replace(&mut self.drawables[pos].drawable, new);
        self.undo_stack.push(UndoAction::Modify { id, prev });
        self.redo_stack.clear();
        true
    }

    /// Remove the drawable with `id` from the stack. Records a Remove undo
    /// action so the deletion can be undone.
    pub fn delete(&mut self, id: DrawableId) -> bool {
        let Some(pos) = self.drawables.iter().position(|s| s.id == id) else {
            return false;
        };
        let stacked = self.drawables.remove(pos);
        self.undo_stack.push(UndoAction::Remove {
            id: stacked.id,
            idx: pos,
            drawable: stacked.drawable,
        });
        self.redo_stack.clear();
        true
    }

    /// Replace many drawables atomically (single Batch undo).
    pub fn modify_many(&mut self, updates: Vec<(DrawableId, Box<dyn Drawable>)>) -> bool {
        let mut actions = Vec::new();
        for (id, new) in updates {
            if let Some(pos) = self.drawables.iter().position(|s| s.id == id) {
                let prev = std::mem::replace(&mut self.drawables[pos].drawable, new);
                actions.push(UndoAction::Modify { id, prev });
            }
        }
        if actions.is_empty() {
            return false;
        }
        self.undo_stack.push(UndoAction::Batch(actions));
        self.redo_stack.clear();
        true
    }

    /// Remove a set of drawables atomically. Records a single Batch undo
    /// action so one Ctrl+Z brings them all back.
    pub fn delete_many(&mut self, ids: &[DrawableId]) -> bool {
        let mut actions = Vec::new();
        // Sort by position descending so removing earlier ids doesn't shift
        // later ones.
        let mut positions: Vec<(usize, DrawableId)> = ids
            .iter()
            .filter_map(|&id| {
                self.drawables
                    .iter()
                    .position(|s| s.id == id)
                    .map(|pos| (pos, id))
            })
            .collect();
        positions.sort_by_key(|p| std::cmp::Reverse(p.0));
        for (pos, id) in positions {
            let stacked = self.drawables.remove(pos);
            actions.push(UndoAction::Remove {
                id,
                idx: pos,
                drawable: stacked.drawable,
            });
        }
        if actions.is_empty() {
            return false;
        }
        // Apply order matters for the undo (Insert): the original order was
        // back-to-front, so reverse the per-removal actions to insert in the
        // right order on undo.
        actions.reverse();
        self.undo_stack.push(UndoAction::Batch(actions));
        self.redo_stack.clear();
        true
    }

    /// Drawable ids whose AABB bounds overlap `rect` (image coords). Used
    /// for marquee / drag-rect selection.
    pub fn drawables_in_rect(&self, rect: crate::math::Rect) -> Vec<DrawableId> {
        self.drawables
            .iter()
            .filter(|s| {
                s.drawable
                    .bounds()
                    .map(|b| b.intersects(rect))
                    .unwrap_or(false)
            })
            .map(|s| s.id)
            .collect()
    }

    /// All drawable ids in stacking order (back-to-front).
    pub fn all_drawable_ids(&self) -> Vec<DrawableId> {
        self.drawables.iter().map(|s| s.id).collect()
    }

    pub fn undo(&mut self) -> bool {
        let Some(action) = self.undo_stack.pop() else {
            return false;
        };
        let inverse = self.apply_inverse(action);
        self.redo_stack.push(inverse);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(action) = self.redo_stack.pop() else {
            return false;
        };
        let inverse = self.apply_inverse(action);
        self.undo_stack.push(inverse);
        true
    }

    /// Apply the inverse of `action`, returning the action that should be pushed
    /// on the opposite stack. Shared between undo() and redo().
    fn apply_inverse(&mut self, action: UndoAction) -> UndoAction {
        match action {
            UndoAction::Add(id) => {
                let pos = self
                    .drawables
                    .iter()
                    .position(|s| s.id == id)
                    .expect("Add references missing drawable");
                let mut stacked = self.drawables.remove(pos);
                stacked.drawable.handle_undo();
                UndoAction::Remove {
                    id,
                    idx: pos,
                    drawable: stacked.drawable,
                }
            }
            UndoAction::Remove {
                id,
                idx,
                mut drawable,
            } => {
                drawable.handle_redo();
                let insert_at = idx.min(self.drawables.len());
                self.drawables.insert(insert_at, Stacked { id, drawable });
                UndoAction::Add(id)
            }
            UndoAction::Modify { id, prev } => {
                let pos = self
                    .drawables
                    .iter()
                    .position(|s| s.id == id)
                    .expect("Modify references missing drawable");
                let cur = std::mem::replace(&mut self.drawables[pos].drawable, prev);
                UndoAction::Modify { id, prev: cur }
            }
            UndoAction::Batch(actions) => {
                // Reverse order while inverting so insert/remove indices stay
                // consistent. The result is also a Batch; pushing it onto the
                // opposite stack lets one Ctrl+Z/Y restore the whole group.
                let mut inverses: Vec<UndoAction> = actions
                    .into_iter()
                    .rev()
                    .map(|a| self.apply_inverse(a))
                    .collect();
                inverses.reverse();
                UndoAction::Batch(inverses)
            }
        }
    }

    pub fn reset(&mut self) -> bool {
        let mut any = false;
        while !self.drawables.is_empty() && self.undo() {
            any = true;
        }
        any
    }

    /// Topmost drawable hit by `point` (image coords). Iterates back-to-front so
    /// the most recently drawn (visually on top) wins.
    pub fn hit_test(&self, point: Vec2D, tolerance: f32) -> Option<DrawableId> {
        for s in self.drawables.iter().rev() {
            if s.drawable.hit_test(point, tolerance) {
                return Some(s.id);
            }
        }
        None
    }

    /// Borrow the live drawable for a given id, if it exists in the stack.
    pub fn drawable(&self, id: DrawableId) -> Option<&dyn Drawable> {
        self.drawables
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.drawable.as_ref())
    }

    pub fn set_active_tool(&mut self, active_tool: Rc<RefCell<dyn Tool>>) {
        self.active_tool = active_tool;
    }

    pub fn render_native_resolution(
        &mut self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        font: FontId,
    ) -> anyhow::Result<ImgVec<RGBA8>> {
        let bounds = (
            Vec2D::zero(),
            Vec2D::new(
                self.background_image.width() as f32,
                self.background_image.height() as f32,
            ),
        );
        // get offset and size of the area in question
        let (pos, size) = self
            .crop_tool
            .borrow()
            .get_crop()
            .map(|c| c.get_rectangle())
            .map(|rect| rect_ensure_in_bounds(rect, bounds))
            .map(rect_round)
            .filter(|(_, size)| !size.is_zero())
            .unwrap_or(bounds);

        // create render-target
        let image_id = canvas.create_image_empty(
            size.x as usize,
            size.y as usize,
            PixelFormat::Rgba8,
            ImageFlags::empty(),
        )?;
        canvas.set_render_target(femtovg::RenderTarget::Image(image_id));

        // apply offset
        let mut transform = Transform2D::identity();
        transform.translate(-pos.x, -pos.y);
        canvas.reset_transform();
        canvas.set_transform(&transform);

        self.render(
            canvas,
            font,
            false,
            femtovg::Color::rgbaf(0.0, 0.0, 0.0, 0.0),
            false,
        )?;

        // return screenshot
        let result = canvas.screenshot();

        // clean up
        canvas.set_render_target(femtovg::RenderTarget::Screen);
        canvas.delete_image(image_id);

        Ok(result?)
    }

    pub fn render_framebuffer(
        &mut self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        font: FontId,
    ) -> Result<()> {
        canvas.set_render_target(femtovg::RenderTarget::Screen);

        // setup transform to image coordinates
        let mut transform = Transform2D::identity();
        transform.scale(self.scale_factor, self.scale_factor);
        transform.translate(self.offset.x, self.offset.y);

        canvas.reset_transform();
        canvas.set_transform(&transform);

        //TODO: make background color configurable
        self.render(canvas, font, true, femtovg::Color::black(), true)?;

        Ok(())
    }

    fn render(
        &mut self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        font: FontId,
        render_crop: bool,
        outside_bg_color: femtovg::Color,
        onscreen: bool,
    ) -> Result<()> {
        // clear canvas

        canvas.clear_rect(0, 0, canvas.width(), canvas.height(), outside_bg_color);

        // render background
        self.render_background_image(canvas, onscreen)?;

        let bounds = (
            Vec2D::zero(),
            Vec2D::new(
                self.background_image.width() as f32,
                self.background_image.height() as f32,
            ),
        );
        // Skip rendering of any drawable currently being dragged by either
        // tool — the tool will render the moved/transformed copy below.
        let dragging_active = self.active_tool.borrow().dragging_drawable_id();
        let dragging_pointer = self.pointer_tool.borrow().dragging_drawable_id();
        let selected_ids = self.pointer_tool.borrow().selected_drawables();

        for s in &mut self.drawables {
            if dragging_active == Some(s.id) || dragging_pointer == Some(s.id) {
                continue;
            }
            // Render the selection glow underneath each selected drawable so
            // the wide blue trace is half-clipped by the drawable on top —
            // leaving only an outer halo.
            if selected_ids.contains(&s.id) {
                s.drawable
                    .render_glow(canvas, font, bounds, self.device_pixel_ratio)?;
            }
            s.drawable.draw(canvas, font, bounds)?;
        }

        let pointer_is_active = Rc::ptr_eq(&self.active_tool, &self.pointer_tool);

        // In-progress drawable from the active tool (e.g. the shape currently
        // being drawn). When the pointer tool is the active tool *and* it's
        // mid-drag, this is the selection's working copy — render the glow
        // beneath it so the halo follows the drag in real time.
        {
            let at = self.active_tool.borrow();
            if let Some(d) = at.get_drawable() {
                if pointer_is_active && at.dragging_drawable_id().is_some() {
                    d.render_glow(canvas, font, bounds, self.device_pixel_ratio)?;
                }
                d.draw(canvas, font, bounds)?;
            }
        }

        // The pointer tool's working copy during an implicit-mode drag (active
        // tool is something else, like Arrow).
        if !pointer_is_active
            && let Some(d) = self.pointer_tool.borrow().get_drawable()
        {
            d.render_glow(canvas, font, bounds, self.device_pixel_ratio)?;
            d.draw(canvas, font, bounds)?;
        }

        // Selection overlay (marquee + handles for single selection).
        let single_selected_drawable = if selected_ids.len() == 1 {
            self.drawables
                .iter()
                .find(|s| s.id == selected_ids[0])
                .map(|s| s.drawable.as_ref())
        } else {
            None
        };
        if let Some(o) = self
            .pointer_tool
            .borrow()
            .build_overlay(single_selected_drawable, self.device_pixel_ratio)
        {
            o.draw(canvas, font, bounds)?;
        }

        // render crop tool
        if render_crop && let Some(c) = self.crop_tool.borrow().get_crop() {
            c.draw(canvas, font, bounds)?;
        }

        canvas.flush();
        Ok(())
    }

    fn render_background_image(
        &mut self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        onscreen: bool,
    ) -> Result<()> {
        let background_image_id = match self.background_image_id {
            Some(id) => id,
            None => {
                let id = Self::upload_background_image(canvas, &self.background_image)?;
                self.background_image_id.replace(id);
                id
            }
        };

        let transparency_bg_id = match self.transparent_background_id {
            Some(id) if onscreen => Some(id),
            None => {
                if let Some(id) = Self::create_transparency_bg(canvas) {
                    self.transparent_background_id.replace(id);
                    Some(id)
                } else {
                    None
                }
            }
            _ => None,
        };

        // render the image
        let mut path = Path::new();

        let w = self.background_image.width() as f32;
        let h = self.background_image.height() as f32;

        path.rect(0.0, 0.0, w, h);

        if let Some(id) = transparency_bg_id {
            canvas.fill_path(
                &path,
                &Paint::image(
                    id,
                    0f32,
                    0f32,
                    TRANSPARENCY_SQUARE_SIZE as f32,
                    TRANSPARENCY_SQUARE_SIZE as f32,
                    0f32,
                    1f32,
                ),
            );
        }

        canvas.fill_path(
            &path,
            &Paint::image(background_image_id, 0f32, 0f32, w, h, 0f32, 1f32),
        );

        Ok(())
    }

    fn upload_background_image(
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        image: &Pixbuf,
    ) -> Result<ImageId> {
        let format = if image.has_alpha() {
            PixelFormat::Rgba8
        } else {
            PixelFormat::Rgb8
        };

        let background_image_id = canvas.create_image_empty(
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

                canvas.update_image(background_image_id, ImageSource::Rgba(img.as_ref()), 0, 0)?;
            } else {
                let img = Img::new_stride(
                    dst_buffer.align_to::<RGB<u8>>().1.to_owned(),
                    width,
                    height,
                    width,
                );

                canvas.update_image(background_image_id, ImageSource::Rgb(img.as_ref()), 0, 0)?;
            }
        }

        Ok(background_image_id)
    }

    fn create_transparency_bg(
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
    ) -> Option<femtovg::ImageId> {
        let tile: usize = TRANSPARENCY_SQUARE_SIZE * 2;
        let mut pixels = vec![RGBA8::new(204, 204, 204, 255); tile * tile];

        for y in 0..tile {
            for x in 0..tile {
                if (x / TRANSPARENCY_SQUARE_SIZE + y / TRANSPARENCY_SQUARE_SIZE) % 2 == 1 {
                    pixels[y * tile + x] = RGBA8::new(153, 153, 153, 255);
                }
            }
        }
        let img = Img::new(pixels, tile, tile);

        match canvas.create_image(
            ImageSource::Rgba(img.as_ref()),
            ImageFlags::REPEAT_X | ImageFlags::REPEAT_Y,
        ) {
            Ok(id) => Some(id),
            Err(_) => {
                eprintln!("Could not create transparency background image");
                None
            }
        }
    }

    pub fn update_transformation(
        &mut self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
    ) {
        let image_width = self.background_image.width() as f32;
        let image_height = self.background_image.height() as f32;
        let aspect_ratio = image_width / image_height;

        let canvas_width = canvas.width() as f32;
        let canvas_height = canvas.height() as f32;

        let prev_scale = self.scale_factor;
        let mut center_offset = Vec2D::zero();

        // update scale_factor
        if self.zoom_scale != 0.0 {
            if self.zoom_scale != self.last_scale {
                self.last_scale = self.zoom_scale;
                self.scale_factor = self.zoom_scale;

                if !self.is_reset {
                    // calculate offset from pointer
                    let pointer_offset = self.pointer_offset;
                    let zoom_offset = Vec2D::new(
                        (pointer_offset.x - self.offset.x) / prev_scale,
                        (pointer_offset.y - self.offset.y) / prev_scale,
                    );

                    let calculated_offset = pointer_offset - zoom_offset * self.scale_factor;

                    // update drag_offset
                    center_offset = Vec2D::new(
                        (canvas_width - image_width * self.scale_factor) / 2.0,
                        (canvas_height - image_height * self.scale_factor) / 2.0,
                    );

                    self.drag_offset = calculated_offset - center_offset;
                    self.store_last_offset();
                }
            } else {
                self.scale_factor = self.zoom_scale;
            }
        } else {
            self.scale_factor = if canvas_width / aspect_ratio <= canvas_height {
                canvas_width / aspect_ratio / image_height
            } else {
                canvas_height * aspect_ratio / image_width
            };
        }

        // final offset
        if center_offset.is_zero() {
            center_offset = Vec2D::new(
                (canvas_width - image_width * self.scale_factor) / 2.0,
                (canvas_height - image_height * self.scale_factor) / 2.0,
            );
        }

        if self.is_reset {
            //centered
            self.is_reset = false;
            self.offset = center_offset;
        } else {
            //dragged
            self.offset = center_offset + self.drag_offset;
        }
    }

    pub fn abs_canvas_to_image_coordinates(&self, input: Vec2D, dpi_scale_factor: f32) -> Vec2D {
        Vec2D::new(
            (input.x * dpi_scale_factor - self.offset.x) / self.scale_factor,
            (input.y * dpi_scale_factor - self.offset.y) / self.scale_factor,
        )
    }
    pub fn rel_canvas_to_image_coordinates(&self, input: Vec2D, dpi_scale_factor: f32) -> Vec2D {
        Vec2D::new(
            input.x * dpi_scale_factor / self.scale_factor,
            input.y * dpi_scale_factor / self.scale_factor,
        )
    }

    pub fn set_zoom_scale(&mut self, factor: f32, abs: bool) {
        if self.is_drag {
            return;
        }

        if abs {
            self.zoom_scale = factor;
        } else {
            if self.zoom_scale == 0.0 {
                self.zoom_scale = self.scale_factor;
            }

            self.zoom_scale *= factor;
            self.zoom_scale = self.zoom_scale.max(0.);
        }
    }

    pub fn set_pointer_offset(&mut self, offset: Vec2D) {
        self.pointer_offset = offset;
    }

    pub fn set_drag_offset(&mut self, offset: Vec2D) {
        self.drag_offset = self.last_offset + offset;
    }

    pub fn reset_drag_offset(&mut self) {
        self.drag_offset = Vec2D::zero();
        self.store_last_offset();
        self.is_reset = true;
    }

    pub fn store_last_offset(&mut self) {
        self.last_offset = self.drag_offset;
    }

    pub fn set_is_drag(&mut self, is_drag: bool) {
        self.is_drag = is_drag;
    }
}
