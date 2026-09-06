use anyhow::Result;
use femtovg::{Color, FontId, Paint, Path};
use relm4::{
    Sender,
    gtk::{self, gdk::ModifierType, prelude::WidgetExt},
};
use std::cell::Cell;

use crate::{
    configuration::APP_CONFIG,
    math::{Vec2D, ensure_bounding_box},
    sketch_board::{KeyEventMsg, MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    tools::RenderingMode,
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
        let mut delta = event.pos;

        if event.modifier & ModifierType::SHIFT_MASK != ModifierType::empty() {
            (delta.x, delta.y) = match self {
                ResizeHandle::TopRight => {
                    let x = delta.x.max(-delta.y);
                    (x, -x)
                }
                ResizeHandle::BottomRight => {
                    let x = delta.x.max(delta.y);
                    (x, x)
                }
                ResizeHandle::BottomLeft => {
                    let x = delta.x.min(-delta.y);
                    (x, -x)
                }
                ResizeHandle::TopLeft => {
                    let x = delta.x.min(delta.y);
                    (x, x)
                }
                _ => (delta.x, delta.y),
            };
        }

        let is_centered = event.modifier & ModifierType::ALT_MASK != ModifierType::empty();
        if is_centered {
            delta = delta / 2.0;
        }

        let mut new_tl = tl;
        let mut new_br = br;

        match self {
            ResizeHandle::TopRight => {
                new_tl.y += delta.y;
                new_br.x += delta.x;
                if is_centered {
                    new_tl.x -= delta.x;
                    new_br.y -= delta.y;
                }
            }
            ResizeHandle::MiddleRight => {
                new_br.x += delta.x;
                if is_centered {
                    new_tl.x -= delta.x;
                }
            }
            ResizeHandle::BottomRight => {
                new_br += delta;
                if is_centered {
                    new_tl -= delta;
                }
            }
            ResizeHandle::BottomCenter => {
                new_br.y += delta.y;
                if is_centered {
                    new_tl.y -= delta.y;
                }
            }
            ResizeHandle::BottomLeft => {
                new_tl.x += delta.x;
                new_br.y += delta.y;
                if is_centered {
                    new_tl.y -= delta.y;
                    new_br.x -= delta.x;
                }
            }
            ResizeHandle::MiddleLeft => {
                new_tl.x += delta.x;
                if is_centered {
                    new_br.x -= delta.x;
                }
            }
            ResizeHandle::TopCenter => {
                new_tl.y += delta.y;
                if is_centered {
                    new_br.y -= delta.y;
                }
            }
            ResizeHandle::TopLeft => {
                new_tl += delta;
                if is_centered {
                    new_br -= delta;
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
    scaled_handle_size: Cell<f32>,
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
        canvas.save();

        // draw handles in inverse zoom scale so the visual size stays constant on screen.
        let scale = canvas.transform().average_scale().max(f32::EPSILON);

        // Selection rectangle
        let stroke_width = 1.5 / scale;
        let mut rect = Path::new();
        rect.rect(
            self.tl.x,
            self.tl.y,
            self.br.x - self.tl.x,
            self.br.y - self.tl.y,
        );
        canvas.stroke_path(
            &rect,
            &Paint::color(Color::rgba(70, 130, 180, 220)).with_line_width(stroke_width),
        );

        // Resize handles
        let handle_half = HANDLE_HALF / scale;
        let handle_size = HANDLE_SIZE / scale;
        self.scaled_handle_size.set(handle_size);
        for handle in ResizeHandle::all() {
            let c = handle.center(self.tl, self.br);
            let mut hpath = Path::new();
            hpath.rect(
                c.x - handle_half,
                c.y - handle_half,
                handle_size,
                handle_size,
            );
            canvas.fill_path(&hpath, &Paint::color(Color::rgba(255, 255, 255, 255)));
            canvas.stroke_path(
                &hpath,
                &Paint::color(Color::rgba(70, 130, 180, 255)).with_line_width(stroke_width),
            );
        }

        canvas.restore();
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
        let scaled_handle_size = overlay.scaled_handle_size.get();
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

    // The crop readout has to track anything that moves or resizes the crop,
    // since all of it changes what a save would produce. Sourced from the
    // drawable's own bounds, which is the rect render_native_resolution clips.
    fn emit_crop_dimensions_update(&self, crop: &dyn Drawable) {
        if crop.get_rendering_mode() == RenderingMode::Crop
            && let Some(sender) = &self.sender
            && let Some((tl, br)) = crop.bounds()
        {
            sender
                .send(SketchBoardInput::CropDimensionsUpdate((tl, br - tl)))
                .ok();
        }
    }

    fn update_selection_bounds(&mut self, tl: Vec2D, br: Vec2D) {
        let (tl, br) = ensure_bounding_box(tl, br);
        self.selected_bounds = Some((tl, br));

        let handle_size = if let Some(overlay) = &self.selection_overlay {
            overlay.scaled_handle_size.get()
        } else {
            HANDLE_SIZE
        };

        // Add extra outset to selection overlay if the drawable is small to reduce handle overlapping
        let w = br.x - tl.x + SELECTION_BORDER_OUTSET * 2.0;
        let h = br.y - tl.y + SELECTION_BORDER_OUTSET * 2.0;
        let border_outset_x = if w < 3.0 * handle_size {
            HANDLE_SIZE
        } else {
            SELECTION_BORDER_OUTSET
        };
        let border_outset_y = if h < 3.0 * handle_size {
            HANDLE_SIZE
        } else {
            SELECTION_BORDER_OUTSET
        };

        self.selection_overlay = Some(SelectionOverlay {
            tl: tl - Vec2D::new(border_outset_x, border_outset_y),
            br: br + Vec2D::new(border_outset_x, border_outset_y),
            // is updated in draw() to maintain constant on-screen size regardless of zoom level
            scaled_handle_size: Cell::new(HANDLE_SIZE),
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
                    self.emit_crop_dimensions_update(preview.as_ref());
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
                    preview.resize_bounds(new_tl, new_br);
                    self.emit_crop_dimensions_update(preview.as_ref());

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
                            final_drawable.resize_bounds(new_tl, new_br);
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
