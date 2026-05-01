use anyhow::Result;
use femtovg::{Color, FontId, Paint, Path};
use relm4::Sender;

use crate::{
    math::Vec2D,
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
};

use super::{Drawable, Tool, ToolUpdateResult, Tools};

/// Size of each resize handle in image-space pixels.
const HANDLE_SIZE: f32 = 8.0;
const HANDLE_HALF: f32 = HANDLE_SIZE / 2.0;
const SELECTION_BORDER_OUTSET: f32 = 2.0;

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

    /// Compute new (tl, br) after dragging this handle by `delta`.
    pub fn apply_delta(&self, tl: Vec2D, br: Vec2D, delta: Vec2D) -> (Vec2D, Vec2D) {
        let mut new_tl = tl;
        let mut new_br = br;
        match self {
            ResizeHandle::TopLeft => {
                new_tl += delta;
            }
            ResizeHandle::TopCenter => {
                new_tl.y += delta.y;
            }
            ResizeHandle::TopRight => {
                new_tl.y += delta.y;
                new_br.x += delta.x;
            }
            ResizeHandle::MiddleLeft => {
                new_tl.x += delta.x;
            }
            ResizeHandle::MiddleRight => {
                new_br.x += delta.x;
            }
            ResizeHandle::BottomLeft => {
                new_tl.x += delta.x;
                new_br.y += delta.y;
            }
            ResizeHandle::BottomCenter => {
                new_br.y += delta.y;
            }
            ResizeHandle::BottomRight => {
                new_br += delta;
            }
        }
        (new_tl, new_br)
    }
}

/// Returns the handle under `pos`, if any, given bounds `(tl, br)`.
pub fn hit_handle(pos: Vec2D, tl: Vec2D, br: Vec2D) -> Option<ResizeHandle> {
    for h in ResizeHandle::all() {
        let c = h.center(tl, br);
        if (pos.x - c.x).abs() <= HANDLE_HALF && (pos.y - c.y).abs() <= HANDLE_HALF {
            return Some(h);
        }
    }
    None
}

/// Draws a selection rectangle with 8 resize handles on top of the selected drawable.
#[derive(Clone, Debug)]
struct SelectionOverlay {
    tl: Vec2D,
    br: Vec2D,
}

impl Drawable for SelectionOverlay {
    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        canvas.save();

        let w = self.br.x - self.tl.x;
        let h = self.br.y - self.tl.y;

        // Selection rectangle
        let border_tl = Vec2D::new(
            self.tl.x - SELECTION_BORDER_OUTSET,
            self.tl.y - SELECTION_BORDER_OUTSET,
        );
        let border_w = w + 2.0 * SELECTION_BORDER_OUTSET;
        let border_h = h + 2.0 * SELECTION_BORDER_OUTSET;

        let mut rect = Path::new();
        rect.rect(border_tl.x, border_tl.y, border_w, border_h);
        canvas.stroke_path(
            &rect,
            &Paint::color(Color::rgba(70, 130, 180, 220)).with_line_width(1.5),
        );

        // Resize handles
        for handle in ResizeHandle::all() {
            let c = handle.center(self.tl, self.br);
            let mut hpath = Path::new();
            hpath.rect(
                c.x - HANDLE_HALF,
                c.y - HANDLE_HALF,
                HANDLE_SIZE,
                HANDLE_SIZE,
            );
            canvas.fill_path(&hpath, &Paint::color(Color::rgba(255, 255, 255, 255)));
            canvas.stroke_path(
                &hpath,
                &Paint::color(Color::rgba(70, 130, 180, 255)).with_line_width(1.5),
            );
        }

        canvas.restore();
        Ok(())
    }
}

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
    selected_index: Option<usize>,
    selected_bounds: Option<(Vec2D, Vec2D)>,
    drag_state: DragState,
    /// Shown as the active-tool drawable: either a moved/resized preview, or a selection overlay.
    preview: Option<Box<dyn Drawable>>,
    selection_overlay: Option<SelectionOverlay>,
}

impl Default for PointerTool {
    fn default() -> Self {
        Self {
            input_enabled: false,
            sender: None,
            selected_index: None,
            selected_bounds: None,
            drag_state: DragState::None,
            preview: None,
            selection_overlay: None,
        }
    }
}

impl PointerTool {
    pub fn selected_index(&self) -> Option<usize> {
        self.selected_index
    }

    /// Returns the handle under `pos` given the current selection bounds.
    pub fn hit_test_handle(&self, pos: Vec2D) -> Option<ResizeHandle> {
        self.selected_bounds
            .and_then(|(tl, br)| hit_handle(pos, tl, br))
    }

    /// Called by SketchBoard before delivering a BeginDrag event: sets up a move drag.
    pub fn begin_move(
        &mut self,
        index: usize,
        drawable: Box<dyn Drawable>,
        orig_bounds: (Vec2D, Vec2D),
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
    }

    /// Called by SketchBoard before delivering a BeginDrag event: sets up a resize drag.
    pub fn begin_resize(
        &mut self,
        index: usize,
        drawable: Box<dyn Drawable>,
        handle: ResizeHandle,
        orig_bounds: (Vec2D, Vec2D),
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
    }

    /// Select a drawable without starting a drag (e.g. after a commit/replace).
    pub fn set_selection(&mut self, index: usize, bounds: (Vec2D, Vec2D)) {
        self.selected_index = Some(index);
        self.selected_bounds = Some(bounds);
        self.selection_overlay = Some(SelectionOverlay {
            tl: bounds.0,
            br: bounds.1,
        });
        self.drag_state = DragState::None;
        self.preview = None;
    }

    pub fn deselect(&mut self) {
        self.selected_index = None;
        self.selected_bounds = None;
        self.selection_overlay = None;
        self.drag_state = DragState::None;
        self.preview = None;
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
        self.deselect();
        ToolUpdateResult::Redraw
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        if event.button == MouseButton::Middle {
            return ToolUpdateResult::Unmodified;
        }

        // For EndDrag/UpdateDrag, event.pos is the cumulative delta since BeginDrag.
        match event.type_ {
            MouseEventType::UpdateDrag => match &self.drag_state {
                DragState::Moving {
                    original,
                    orig_bounds,
                    ..
                } => {
                    let delta = event.pos;
                    let mut preview = original.clone_box();
                    preview.translate(delta);
                    let (tl, br) = *orig_bounds;
                    self.selected_bounds = Some((tl + delta, br + delta));
                    self.preview = Some(preview);
                    ToolUpdateResult::Redraw
                }
                DragState::Resizing {
                    original,
                    handle,
                    orig_bounds,
                    ..
                } => {
                    let delta = event.pos;
                    let (new_tl, new_br) = handle.apply_delta(orig_bounds.0, orig_bounds.1, delta);
                    let mut preview = original.clone_box();
                    preview.resize_bounds(new_tl, new_br);
                    self.selected_bounds = Some((new_tl, new_br));
                    self.preview = Some(preview);
                    ToolUpdateResult::Redraw
                }
                DragState::None => ToolUpdateResult::Unmodified,
            },

            MouseEventType::EndDrag => {
                match std::mem::replace(&mut self.drag_state, DragState::None) {
                    DragState::Moving {
                        index,
                        original,
                        orig_bounds,
                    } => {
                        let delta = event.pos;
                        if delta.is_zero() {
                            // Click with no movement: just show selection overlay
                            self.selected_bounds = Some(orig_bounds);
                            self.selection_overlay = Some(SelectionOverlay {
                                tl: orig_bounds.0,
                                br: orig_bounds.1,
                            });
                            self.preview = None;
                            ToolUpdateResult::Redraw
                        } else {
                            let mut final_drawable = original;
                            final_drawable.translate(delta);
                            let (tl, br) = orig_bounds;
                            let new_bounds = (tl + delta, br + delta);
                            self.selected_bounds = Some(new_bounds);
                            self.selection_overlay = Some(SelectionOverlay {
                                tl: new_bounds.0,
                                br: new_bounds.1,
                            });
                            self.preview = None;
                            ToolUpdateResult::ReplaceDrawable(index, final_drawable)
                        }
                    }
                    DragState::Resizing {
                        index,
                        original,
                        handle,
                        orig_bounds,
                    } => {
                        let delta = event.pos;
                        if delta.is_zero() {
                            self.selected_bounds = Some(orig_bounds);
                            self.selection_overlay = Some(SelectionOverlay {
                                tl: orig_bounds.0,
                                br: orig_bounds.1,
                            });
                            self.preview = None;
                            ToolUpdateResult::Redraw
                        } else {
                            let (new_tl, new_br) =
                                handle.apply_delta(orig_bounds.0, orig_bounds.1, delta);
                            let mut final_drawable = original;
                            final_drawable.resize_bounds(new_tl, new_br);
                            self.selected_bounds = Some((new_tl, new_br));
                            self.selection_overlay = Some(SelectionOverlay {
                                tl: new_tl,
                                br: new_br,
                            });
                            self.preview = None;
                            ToolUpdateResult::ReplaceDrawable(index, final_drawable)
                        }
                    }
                    DragState::None => ToolUpdateResult::Unmodified,
                }
            }

            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>) {
        self.sender = Some(sender);
    }
}
