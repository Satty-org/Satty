use anyhow::Result;
use femtovg::FontId;
use relm4::{
    Sender,
    gtk::{self, gdk::ModifierType, prelude::WidgetExt},
};
use std::cell::Cell;

use crate::{
    configuration::APP_CONFIG,
    math::{Vec2D, ensure_bounding_box, get_closest_aspect_ratio},
    sketch_board::{KeyEventMsg, MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    tools::{
        RenderingMode,
        drag_box::{draw_center_marker, draw_rect_marker},
    },
};

use super::{Drawable, InputContext, Tool, ToolUpdateResult, Tools};

// Desired on-screen size (in device pixels) for each resize handle.
const HANDLE_SIZE: f32 = 11.0;
const HANDLE_HALF: f32 = HANDLE_SIZE / 2.0;
const SELECTION_BORDER_OUTSET: f32 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ResizeHandle {
    TopLeft,
    TopCenter,
    TopRight,
    MiddleLeft,
    MiddleRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

impl ResizeHandle {
    pub fn all() -> [ResizeHandle; 8] {
        [
            ResizeHandle::TopLeft,
            ResizeHandle::TopCenter,
            ResizeHandle::TopRight,
            ResizeHandle::MiddleLeft,
            ResizeHandle::MiddleRight,
            ResizeHandle::BottomLeft,
            ResizeHandle::BottomCenter,
            ResizeHandle::BottomRight,
        ]
    }

    pub fn center(&self, tl: Vec2D, br: Vec2D) -> Vec2D {
        let mx = (tl.x + br.x) / 2.0;
        let my = (tl.y + br.y) / 2.0;
        match self {
            ResizeHandle::TopLeft => tl,
            ResizeHandle::TopCenter => Vec2D::new(mx, tl.y),
            ResizeHandle::TopRight => Vec2D::new(br.x, tl.y),
            ResizeHandle::MiddleLeft => Vec2D::new(tl.x, my),
            ResizeHandle::MiddleRight => Vec2D::new(br.x, my),
            ResizeHandle::BottomLeft => Vec2D::new(tl.x, br.y),
            ResizeHandle::BottomCenter => Vec2D::new(mx, br.y),
            ResizeHandle::BottomRight => br,
        }
    }

    // Compute new (tl, br) after dragging this handle by `delta`.
    //
    // This intentionally preserves axis inversion (tl may become > br) so tools like
    // line/arrow can keep endpoint intent when crossing over an axis.
    pub fn resize(&self, event: MouseEventMsg, tl: Vec2D, br: Vec2D) -> (Vec2D, Vec2D) {
        let mut new_tl = tl;
        let mut new_br = br;
        let mut delta = event.pos;

        let aspect = event.modifier.intersects(ModifierType::SHIFT_MASK);
        let keep_aspect = event.modifier.intersects(ModifierType::CONTROL_MASK);
        let centered = event.modifier.intersects(ModifierType::ALT_MASK);

        type RH = ResizeHandle;

        // keep aspect ratio if Ctrl is held down on corners
        if keep_aspect && (br.y - tl.y) > f32::EPSILON {
            let size = br - tl;
            let aspect_ratio = size.x.abs() / size.y.abs();
            let center_scale = if centered { 1.0 } else { 2.0 };

            // Adjusts a (width-delta, height-delta) pair, in the "both attached to
            // br" sign convention, so it maintains aspect_ratio. Which axis is
            // dominant is picked by whichever the mouse actually moved further
            // in order to keep the mouse pointer on the edge.
            // When centered, wh is applied to *both* opposite corners, so the
            // real total size change is 2*wh, not wh - the signum heuristic below
            // needs that same total to pick the correct axis near a size inversion.
            let total_scale = 2.0 / center_scale;
            let fix_aspect = |mut wh: Vec2D| -> Vec2D {
                let size_delta = size + wh * total_scale;
                let sdxs = size_delta.x.signum();
                let sdys = size_delta.y.signum();
                if sdxs * wh.x >= sdys * wh.y * aspect_ratio {
                    wh.y = wh.x / aspect_ratio;
                } else {
                    wh.x = wh.y * aspect_ratio;
                }
                wh
            };

            match self {
                RH::TopLeft => {
                    // w and h are both attached to tl (-dx/-dy grow them)
                    let wh = fix_aspect(delta * -1.0);
                    delta = wh * -1.0;
                }
                RH::BottomRight => {
                    // w and h are both attached to br (dx/dy grow them)
                    delta = fix_aspect(delta);
                }
                RH::TopRight => {
                    // w is attached to br (dx grows it), h to tl (-dy grows it).
                    let wh = fix_aspect(Vec2D::new(delta.x, -delta.y));
                    delta = Vec2D::new(wh.x, -wh.y);
                }
                RH::BottomLeft => {
                    // w is attached to tl (-dx grows it), h to br (dy grows it).
                    let wh = fix_aspect(Vec2D::new(-delta.x, delta.y));
                    delta = Vec2D::new(-wh.x, wh.y);
                }
                RH::TopCenter => {
                    // h is attached to tl (-dy grows it); w follows via aspect ratio.
                    delta.x = -delta.y * aspect_ratio;
                    new_tl.x -= delta.x / center_scale;
                    new_br.x += delta.x / center_scale;
                }
                RH::BottomCenter => {
                    // h is attached to br (dy grows it); w follows via aspect ratio.
                    delta.x = delta.y * aspect_ratio;
                    new_tl.x -= delta.x / center_scale;
                    new_br.x += delta.x / center_scale;
                }
                RH::MiddleLeft => {
                    // w is attached to tl (-dx grows it); h follows via aspect ratio.
                    delta.y = -delta.x / aspect_ratio;
                    new_tl.y -= delta.y / center_scale;
                    new_br.y += delta.y / center_scale;
                }
                RH::MiddleRight => {
                    // w is attached to br (dx grows it); h follows via aspect ratio.
                    delta.y = delta.x / aspect_ratio;
                    new_tl.y -= delta.y / center_scale;
                    new_br.y += delta.y / center_scale;
                }
            }
        }

        // keep center fixed if Alt is held down
        match self {
            RH::TopLeft => {
                new_tl += delta;
                if centered {
                    new_br -= delta;
                }
            }
            RH::BottomRight => {
                new_br += delta;
                if centered {
                    new_tl -= delta;
                }
            }
            RH::TopRight => {
                new_tl.y += delta.y;
                new_br.x += delta.x;
                if centered {
                    new_tl.x -= delta.x;
                    new_br.y -= delta.y;
                }
            }
            RH::BottomLeft => {
                new_tl.x += delta.x;
                new_br.y += delta.y;
                if centered {
                    new_tl.y -= delta.y;
                    new_br.x -= delta.x;
                }
            }
            RH::TopCenter => {
                new_tl.y += delta.y;
                if centered {
                    new_br.y -= delta.y;
                }
            }
            RH::BottomCenter => {
                new_br.y += delta.y;
                if centered {
                    new_tl.y -= delta.y;
                }
            }
            RH::MiddleLeft => {
                new_tl.x += delta.x;
                if centered {
                    new_br.x -= delta.x;
                }
            }
            RH::MiddleRight => {
                new_br.x += delta.x;
                if centered {
                    new_tl.x -= delta.x;
                }
            }
        }

        // keep to predefined aspect ratio if Shift is held down
        if aspect {
            let w = new_br.x - new_tl.x;
            let h = new_br.y - new_tl.y;
            let config = APP_CONFIG.read();

            let closest_aspect_ratio = get_closest_aspect_ratio(w / h, config.aspect_ratios());
            let aspect_ratio = closest_aspect_ratio.0 / closest_aspect_ratio.1;
            let size = if h.abs() <= f32::EPSILON {
                Vec2D::new(w, h) // fallback
            } else {
                match self {
                    RH::TopCenter | RH::BottomCenter => Vec2D::new(h * aspect_ratio, h),
                    RH::MiddleLeft | RH::MiddleRight => Vec2D::new(w, w / aspect_ratio),
                    _ => {
                        if w.abs() >= h.abs() * aspect_ratio {
                            Vec2D::new(w, w / aspect_ratio)
                        } else {
                            Vec2D::new(h * aspect_ratio, h)
                        }
                    }
                }
            };

            let size_half = size / 2.0;
            let center = (new_tl + new_br) / 2.0;

            if centered {
                new_tl = center - size_half;
                new_br = center + size_half;
            } else {
                match self {
                    RH::TopLeft => {
                        new_tl = new_br - size;
                    }
                    RH::TopRight => {
                        new_br.x = new_tl.x + size.x;
                        new_tl.y = new_br.y - size.y;
                    }
                    RH::BottomRight => {
                        new_br = new_tl + size;
                    }
                    RH::BottomLeft => {
                        new_tl.x = new_br.x - size.x;
                        new_br.y = new_tl.y + size.y;
                    }
                    RH::TopCenter | RH::BottomCenter => {
                        new_tl.x = center.x - size_half.x;
                        new_br.x = center.x + size_half.x;
                    }
                    RH::MiddleLeft | RH::MiddleRight => {
                        new_tl.y = center.y - size_half.y;
                        new_br.y = center.y + size_half.y;
                    }
                }
            }
        }

        (new_tl, new_br)
    }
}

// Returns the handle under `pos`, if any, given bounds `(tl, br)`.
pub fn hit_handle(
    scaled_handle_size: f32,
    pos: Vec2D,
    tl: Vec2D,
    br: Vec2D,
) -> Option<ResizeHandle> {
    for h in ResizeHandle::all() {
        let handle_half = scaled_handle_size / 2.0;

        let c = h.center(tl, br);
        if (pos.x - c.x).abs() <= handle_half && (pos.y - c.y).abs() <= handle_half {
            return Some(h);
        }
    }
    None
}

// Draws a selection rectangle with 8 resize handles on top of the selected drawable.
#[derive(Clone, Debug)]
struct SelectionOverlay {
    tl: Vec2D,
    br: Vec2D,
    scale: Cell<f32>,
    centered: bool,
    editing: bool,
}

impl Drawable for SelectionOverlay {
    fn get_rendering_mode(&self) -> RenderingMode {
        RenderingMode::SelectionOverlay
    }

    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        let scale = canvas.transform().average_scale().max(f32::EPSILON);
        // to be used at other places where the scale is not available
        self.scale.set(scale);

        // Selection rectangle
        let out_set = SELECTION_BORDER_OUTSET / scale;
        let tl = self.tl - out_set;
        let size = (self.br - self.tl) + out_set * 2.0;
        draw_rect_marker(canvas, tl, size, false);

        // Resize handles
        // draw handles in inverse zoom scale so the visual size stays constant on screen.
        let handle_half = HANDLE_HALF / scale;
        let handle_size = HANDLE_SIZE / scale;
        for handle in ResizeHandle::all() {
            let tl = handle.center(self.tl, self.br) - handle_half;
            draw_rect_marker(canvas, tl, Vec2D::new(handle_size, handle_size), true);
        }

        if self.editing && self.centered {
            draw_center_marker(canvas, self.tl + (self.br - self.tl) * 0.5);
        }

        Ok(())
    }
}

#[derive(Debug)]
enum DragState {
    None,
    Moving {
        index: usize,
        original: Box<dyn Drawable>,
        orig_bounds: (Vec2D, Vec2D),
    },
    Resizing {
        index: usize,
        original: Box<dyn Drawable>,
        handle: ResizeHandle,
        orig_bounds: (Vec2D, Vec2D),
    },
}

pub struct PointerTool {
    input_enabled: bool,
    sender: Option<Sender<SketchBoardInput>>,
    cursor_widget: Option<gtk::Widget>,
    selected_index: Option<usize>,
    selected_bounds: Option<(Vec2D, Vec2D)>,
    drag_state: DragState,
    last_drag_state: bool,
    // Shown as the active-tool drawable: either a moved/resized preview, or a selection overlay.
    preview: Option<Box<dyn Drawable>>,
    selection_overlay: Option<SelectionOverlay>,
    // For cycling through overlapping objects: last click position
    last_click_pos: Option<Vec2D>,
    // For cycling through overlapping objects: all hit objects at last click position
    hit_objects_at_pos: Vec<usize>,
    // For cycling through overlapping objects: current index in hit_objects list
    current_cycle_index: usize,
    // Absolute pointer position (image coordinates) at drag start.
    drag_start_pos: Option<Vec2D>,
}

impl Default for PointerTool {
    fn default() -> Self {
        Self {
            input_enabled: false,
            sender: None,
            cursor_widget: None,
            selected_index: None,
            selected_bounds: None,
            drag_state: DragState::None,
            last_drag_state: false,
            preview: None,
            selection_overlay: None,
            last_click_pos: None,
            hit_objects_at_pos: Vec::new(),
            current_cycle_index: 0,
            drag_start_pos: None,
        }
    }
}

impl PointerTool {
    pub fn get_cursor(&self, name: &str) -> Option<gtk::gdk::Cursor> {
        let cursor_candidates = match name {
            "grabbing" => Some(&["grabbing", "all-resize"]),
            "grab" => Some(&["grab", "all-scroll"]),
            "nwse-resize" => Some(&["nwse-resize", "top-left-corner"]),
            "nesw-resize" => Some(&["nesw-resize", "top-right-corner"]),
            "ns-resize" => Some(&["ns-resize", "top-center"]),
            "ew-resize" => Some(&["ew-resize", "middle-left"]),
            "not-allowed" => Some(&["not-allowed", "no-drop"]),
            _ => None,
        };
        cursor_candidates.and_then(|candidates| {
            candidates
                .iter()
                .find_map(|candidate| gtk::gdk::Cursor::from_name(candidate, None))
        })
    }

    fn resize_cursor_name(handle: ResizeHandle) -> &'static str {
        type RH = ResizeHandle;
        match handle {
            RH::TopLeft | RH::BottomRight => "nwse-resize",
            RH::TopRight | RH::BottomLeft => "nesw-resize",
            RH::TopCenter | RH::BottomCenter => "ns-resize",
            RH::MiddleLeft | RH::MiddleRight => "ew-resize",
        }
    }

    fn set_hover_cursor(&mut self, pos: Vec2D) {
        let Some(widget) = &self.cursor_widget else {
            return;
        };

        let not_renderable = self.preview.as_ref().is_some_and(|p| !p.is_renderable());

        let cursor = if let DragState::Moving { .. } = self.drag_state {
            self.last_drag_state = true;
            if not_renderable {
                self.get_cursor("not-allowed")
            } else {
                self.get_cursor("grabbing")
            }
        } else if let DragState::Resizing { handle, .. } = self.drag_state {
            if not_renderable {
                self.get_cursor("not-allowed")
            } else {
                self.get_cursor(Self::resize_cursor_name(handle))
            }
        } else if matches!(self.drag_state, DragState::None) && self.last_drag_state {
            if let Some(sender) = &self.sender {
                sender.emit(SketchBoardInput::RefreshMouseCursor(pos));
            }
            self.last_drag_state = false;
            None
        } else if let Some(handle) = self.hit_test_handles(pos) {
            self.get_cursor(Self::resize_cursor_name(handle))
        } else {
            None
        };

        if cursor.is_some() {
            widget.set_cursor(cursor.as_ref());
        }
    }

    fn clear_hover_cursor(&self) {
        if let Some(widget) = &self.cursor_widget {
            widget.set_cursor(None);
        }
    }

    pub fn selected_index(&self) -> Option<usize> {
        self.selected_index
    }

    pub fn selected_bounds(&self) -> Option<(Vec2D, Vec2D)> {
        self.selected_bounds
    }

    // Returns the handle under `pos` given the current selection bounds.
    pub fn hit_test_handles(&self, pos: Vec2D) -> Option<ResizeHandle> {
        let overlay = self.selection_overlay.as_ref()?;
        let scaled_handle_size = HANDLE_SIZE / overlay.scale.get();
        hit_handle(scaled_handle_size, pos, overlay.tl, overlay.br)
    }

    // Called by SketchBoard before delivering a BeginDrag event: sets up a move drag.
    pub fn begin_move(
        &mut self,
        index: usize,
        drawable: Box<dyn Drawable>,
        orig_bounds: (Vec2D, Vec2D),
        start_pos: Vec2D,
    ) {
        self.selected_index = Some(index);
        self.selected_bounds = Some(orig_bounds);
        self.selection_overlay = None;
        self.preview = Some(drawable.clone_box());
        self.drag_state = DragState::Moving {
            index,
            original: drawable,
            orig_bounds,
        };
        self.drag_start_pos = Some(start_pos);
        self.set_hover_cursor(orig_bounds.0);
    }

    // Called by SketchBoard before delivering a BeginDrag event: sets up a resize drag.
    pub fn begin_resize(
        &mut self,
        index: usize,
        drawable: Box<dyn Drawable>,
        handle: ResizeHandle,
        orig_bounds: (Vec2D, Vec2D),
        start_pos: Vec2D,
    ) {
        self.selected_index = Some(index);
        self.selected_bounds = Some(orig_bounds);
        self.selection_overlay = None;
        self.preview = Some(drawable.clone_box());
        self.drag_state = DragState::Resizing {
            index,
            original: drawable,
            handle,
            orig_bounds,
        };
        self.drag_start_pos = Some(start_pos);
    }

    fn current_drag_pos(&self, delta: Vec2D) -> Vec2D {
        self.drag_start_pos.map_or(delta, |start| start + delta)
    }

    fn emit_dimensions_update(&self, drawable: &dyn Drawable) {
        if let Some(sender) = &self.sender
            && let Some((tl, br)) = drawable.bounds()
        {
            // get it from drawable as it represents the most up-to-date bounds
            sender
                .send(SketchBoardInput::ShapeDimensionsUpdate(br - tl))
                .ok();
        }
    }

    fn update_selection_bounds(&mut self, tl: Vec2D, br: Vec2D) {
        let (tl, br) = ensure_bounding_box(tl, br);
        self.selected_bounds = Some((tl, br));

        self.selection_overlay = Some(SelectionOverlay {
            tl,
            br,
            // is updated in draw() to maintain constant on-screen size regardless of zoom level
            scale: Cell::new(1.0),
            centered: false,
            editing: true,
        });
    }

    // Select a drawable without starting a drag (e.g. after a commit/replace).
    pub fn set_selection(&mut self, index: usize, bounds: (Vec2D, Vec2D)) {
        self.selected_index = Some(index);
        self.update_selection_bounds(bounds.0, bounds.1);
        self.drag_state = DragState::None;
        self.preview = None;
    }

    pub fn deselect(&mut self) {
        self.selected_index = None;
        self.selected_bounds = None;
        self.selection_overlay = None;
        self.drag_state = DragState::None;
        self.preview = None;
        // Reset cycling state when deselecting
        self.last_click_pos = None;
        self.hit_objects_at_pos.clear();
        self.current_cycle_index = 0;
        self.drag_start_pos = None;
    }

    // Cycle through overlapping objects at the same position.
    // When Alt+Click is used, this method determines which object to select next.
    // Returns the next object index to cycle through, or None if no objects are at the position.
    pub fn cycle_to_next_object(
        &mut self,
        click_pos: Vec2D,
        hit_indices: Vec<usize>,
    ) -> Option<usize> {
        if hit_indices.is_empty() {
            return None;
        }

        // Check if this is the same position as last click
        if let Some(last_pos) = self.last_click_pos {
            if (last_pos.x - click_pos.x).abs() < 0.1 && (last_pos.y - click_pos.y).abs() < 0.1 {
                // Same position: advance to next object in cycle
                self.current_cycle_index = (self.current_cycle_index + 1) % hit_indices.len();
            } else {
                // Different position: reset cycle
                self.current_cycle_index = 0;
            }
        } else {
            // First time: reset cycle
            self.current_cycle_index = 0;
        }

        // Store position for next cycle check
        self.last_click_pos = Some(click_pos);

        // Return the object at current cycle index
        hit_indices.get(self.current_cycle_index).copied()
    }
}

impl Tool for PointerTool {
    fn get_tool_type(&self) -> Tools {
        Tools::Pointer
    }

    fn get_drawable(&self) -> Option<&dyn Drawable> {
        if let Some(p) = &self.preview {
            Some(p.as_ref())
        } else if let Some(s) = &self.selection_overlay {
            Some(s)
        } else {
            None
        }
    }

    fn input_enabled(&self) -> bool {
        self.input_enabled
    }

    fn set_input_enabled(&mut self, value: bool) {
        self.input_enabled = value;
    }

    fn handle_deactivated(&mut self) -> ToolUpdateResult {
        self.clear_hover_cursor();
        self.deselect();
        ToolUpdateResult::Redraw
    }

    fn handle_key_event(&mut self, event: KeyEventMsg) -> ToolUpdateResult {
        if self.selected_index.is_none()
            || event
                .modifier
                .intersects(ModifierType::CONTROL_MASK | ModifierType::ALT_MASK)
        {
            return ToolUpdateResult::Unmodified;
        }

        let step = if event.modifier.contains(ModifierType::SHIFT_MASK) {
            APP_CONFIG.read().text_move_length()
        } else {
            1.0
        };

        let delta = match event.key {
            relm4::gtk::gdk::Key::Left => Vec2D::new(-step, 0.0),
            relm4::gtk::gdk::Key::Right => Vec2D::new(step, 0.0),
            relm4::gtk::gdk::Key::Up => Vec2D::new(0.0, -step),
            relm4::gtk::gdk::Key::Down => Vec2D::new(0.0, step),
            _ => return ToolUpdateResult::Unmodified,
        };

        if let Some(sender) = &self.sender {
            sender.emit(SketchBoardInput::NudgeSelection(delta));
            ToolUpdateResult::StopPropagation
        } else {
            ToolUpdateResult::Unmodified
        }
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        if event.button == MouseButton::Middle {
            return ToolUpdateResult::Unmodified;
        }

        // For EndDrag/UpdateDrag, event.pos is the cumulative delta since BeginDrag.
        match event.type_ {
            MouseEventType::PointerPos | MouseEventType::Release => {
                self.set_hover_cursor(event.pos);
                ToolUpdateResult::Unmodified
            }

            MouseEventType::UpdateDrag => match &self.drag_state {
                DragState::Moving {
                    original,
                    orig_bounds,
                    ..
                } => {
                    let delta = event.pos;
                    let mut preview = original.clone_box();
                    preview.translate(delta);
                    self.emit_dimensions_update(preview.as_ref());
                    let (tl, br) = *orig_bounds;
                    self.update_selection_bounds(tl + delta, br + delta);
                    self.preview = Some(preview);
                    ToolUpdateResult::Redraw
                }
                DragState::Resizing {
                    original,
                    handle,
                    orig_bounds,
                    ..
                } => {
                    let (new_tl, new_br) = handle.resize(event, orig_bounds.0, orig_bounds.1);
                    let mut preview = original.clone_box();
                    let keep_aspect = event
                        .modifier
                        .intersects(ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK);
                    preview.resize_bounds(new_tl, new_br, event.pos, keep_aspect);
                    preview.set_centered(event.modifier.intersects(ModifierType::ALT_MASK));
                    preview.set_editing(true);
                    self.emit_dimensions_update(preview.as_ref());
                    self.update_selection_bounds(new_tl, new_br);
                    self.preview = Some(preview);
                    ToolUpdateResult::Redraw
                }
                DragState::None => ToolUpdateResult::Unmodified,
            },

            MouseEventType::EndDrag => {
                let current_pos = self.current_drag_pos(event.pos);
                match std::mem::replace(&mut self.drag_state, DragState::None) {
                    DragState::Moving {
                        index,
                        original,
                        orig_bounds,
                    } => {
                        let delta = event.pos;
                        let not_renderable =
                            self.preview.as_ref().is_some_and(|p| !p.is_renderable());
                        let result = if delta.is_zero() || not_renderable {
                            // Click with no movement: just show selection overlay
                            self.update_selection_bounds(orig_bounds.0, orig_bounds.1);
                            self.preview = None;
                            ToolUpdateResult::Redraw
                        } else {
                            let mut final_drawable = original;
                            final_drawable.translate(delta);
                            let (tl, br) = orig_bounds;
                            let new_bounds = (tl + delta, br + delta);
                            self.update_selection_bounds(new_bounds.0, new_bounds.1);
                            self.preview = None;
                            ToolUpdateResult::ReplaceDrawable(index, final_drawable)
                        };
                        self.drag_start_pos = None;
                        self.set_hover_cursor(current_pos);
                        result
                    }
                    DragState::Resizing {
                        index,
                        original,
                        handle,
                        orig_bounds,
                    } => {
                        let delta = event.pos;
                        let not_renderable =
                            self.preview.as_ref().is_some_and(|p| !p.is_renderable());
                        let result = if delta.is_zero() || not_renderable {
                            self.update_selection_bounds(orig_bounds.0, orig_bounds.1);
                            self.preview = None;
                            ToolUpdateResult::Redraw
                        } else {
                            let (new_tl, new_br) =
                                handle.resize(event, orig_bounds.0, orig_bounds.1);
                            let mut final_drawable = original;
                            let keep_aspect = event
                                .modifier
                                .intersects(ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK);
                            final_drawable.resize_bounds(new_tl, new_br, event.pos, keep_aspect);
                            final_drawable.set_centered(false);
                            final_drawable.set_editing(false);
                            self.update_selection_bounds(new_tl, new_br);
                            self.preview = None;
                            ToolUpdateResult::ReplaceDrawable(index, final_drawable)
                        };
                        self.drag_start_pos = None;
                        self.set_hover_cursor(current_pos);
                        result
                    }
                    DragState::None => {
                        self.drag_start_pos = None;
                        ToolUpdateResult::Unmodified
                    }
                }
            }

            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>) {
        self.sender = Some(sender);
    }

    fn set_im_context(&mut self, context: Option<InputContext>) {
        self.cursor_widget = context.map(|ctx| ctx.widget);
        self.clear_hover_cursor();
    }
}
