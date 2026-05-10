use std::rc::Rc;

use anyhow::Result;
use femtovg::{Color, FontId, Paint, Path};
use relm4::Sender;

use relm4::gtk::gdk::{Key, ModifierType};

use crate::{
    math::{Rect, Vec2D},
    sketch_board::{KeyEventMsg, MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    style::Style,
};

use super::{
    Drawable, DrawableId, DrawableStore, Handle, HandleId, SELECTION_BLUE, Tool, ToolUpdateResult,
    Tools,
};

pub const HIT_TOLERANCE: f32 = 6.0;
/// Target visible diameter of the blue inner disc, in canvas (post-zoom)
/// pixels. The actual draw radius is divided by the current canvas
/// transform scale so handles look the same size regardless of how much
/// the image is fit-scaled or zoomed.
const HANDLE_INNER_DIAMETER: f32 = 12.0;
/// White ring thickness on each side of the inner disc (so outer
/// diameter = inner + 2 × ring).
const HANDLE_RING: f32 = 2.0;
/// Hit-test radius (image units) for grabbing a handle. Kept in image
/// units because hit-tests run against image-space pointer positions;
/// scaled up a bit so the cursor doesn't have to be pixel-perfect.
pub const HANDLE_HIT_RADIUS: f32 = 12.0;
/// Marquee fill / stroke color (accent blue, faded).
const MARQUEE_FILL: Color = Color {
    r: 0.18,
    g: 0.53,
    b: 0.87,
    a: 0.12,
};
const MARQUEE_STROKE: Color = SELECTION_BLUE;

#[derive(Default)]
pub struct PointerTool {
    input_enabled: bool,
    sender: Option<Sender<SketchBoardInput>>,
    store: Option<Rc<dyn DrawableStore>>,

    /// Multi-selection in stacking order. Single-selection ops use the first
    /// (or only) entry; multi-select ops iterate the whole vec.
    selected: Vec<DrawableId>,
    /// In-flight body or handle drag (single drawable; multi-drag isn't
    /// supported yet).
    drag: Option<DragState>,
    /// In-flight rubber-band selection rectangle. Only created when the
    /// Pointer tool is the active tool (not implicit-mode).
    marquee: Option<MarqueeState>,
    /// True when the Pointer tool is the user-selected active tool, false
    /// when it's only being consulted in implicit-mode for selection. Set
    /// from `handle_activated` / `handle_deactivated`.
    active_as_primary: bool,
    /// Set true when a BeginDrag in implicit mode just deselected because the
    /// user clicked empty space. The follow-up Click event is then suppressed
    /// so e.g. the Marker tool doesn't drop a counter on the same gesture.
    consume_next_click: bool,
}

struct DragState {
    id: DrawableId,
    mode: DragMode,
    original: Box<dyn Drawable>,
    working: Box<dyn Drawable>,
    handle_anchor: Vec2D,
}

#[derive(Debug, Clone, Copy)]
enum DragMode {
    Body,
    Handle(HandleId),
}

struct MarqueeState {
    /// Start point in image coordinates (set on BeginDrag).
    start: Vec2D,
    /// Current corner (start + delta-from-BeginDrag).
    end: Vec2D,
}

impl MarqueeState {
    fn rect(&self) -> Rect {
        Rect::from_corners(self.start, self.end)
    }
}

/// Composite overlay drawn for the current selection: marquee rectangle
/// (during drag-rect) plus manipulation handles for single-selection.
#[derive(Debug)]
struct SelectionOverlay {
    marquee: Option<Rect>,
    handles: Vec<Handle>,
    /// DPR captured at build time, used to size handles in CSS pixels.
    device_pixel_ratio: f32,
}

impl Clone for SelectionOverlay {
    fn clone(&self) -> Self {
        Self {
            marquee: self.marquee,
            handles: self.handles.clone(),
            device_pixel_ratio: self.device_pixel_ratio,
        }
    }
}

impl Drawable for SelectionOverlay {
    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        canvas.save();

        // Marquee rect: faded blue fill + thin stroke.
        if let Some(m) = &self.marquee {
            let mut path = Path::new();
            path.rect(m.pos.x, m.pos.y, m.size.x, m.size.y);
            canvas.fill_path(&path, &Paint::color(MARQUEE_FILL));
            let mut stroke = Paint::color(MARQUEE_STROKE);
            stroke.set_line_width(1.5);
            canvas.stroke_path(&path, &stroke);
        }

        // Handles: white-filled outer disc + blue inner disc. Final goal
        // is HANDLE_INNER_DIAMETER CSS pixels visible on screen. The
        // pipeline:
        //   image_units → (image_to_canvas scale, from canvas.transform)
        //               → physical pixels (canvas is sized in physical px)
        // So to draw N CSS px we want N × DPR physical px, which means
        // (N × DPR) ÷ image_to_canvas in image units.
        let img_to_canvas = canvas.transform().average_scale().max(0.0001);
        let css_to_image = self.device_pixel_ratio / img_to_canvas;
        let inner_r = (HANDLE_INNER_DIAMETER / 2.0) * css_to_image;
        let outer_r = (HANDLE_INNER_DIAMETER / 2.0 + HANDLE_RING) * css_to_image;
        let white_fill = Paint::color(Color::white());
        let blue_fill = Paint::color(SELECTION_BLUE);
        for h in &self.handles {
            let mut outer = Path::new();
            outer.circle(h.pos.x, h.pos.y, outer_r);
            canvas.fill_path(&outer, &white_fill);

            let mut inner = Path::new();
            inner.circle(h.pos.x, h.pos.y, inner_r);
            canvas.fill_path(&inner, &blue_fill);
        }
        canvas.restore();
        Ok(())
    }
}

impl PointerTool {
    /// Hit-test against the handles of the currently-selected drawable —
    /// only valid when there's exactly one selection.
    fn hit_handle(&self, point: Vec2D) -> Option<(DrawableId, Box<dyn Drawable>, Handle)> {
        if self.selected.len() != 1 {
            return None;
        }
        let id = *self.selected.first()?;
        let store = self.store.as_ref()?;
        let drawable = store.clone_drawable(id)?;
        let hit = drawable
            .handles()
            .into_iter()
            .find(|h| h.pos.distance_to(&point) <= h.hit_radius)?;
        Some((id, drawable, hit))
    }
}

impl Tool for PointerTool {
    fn get_tool_type(&self) -> super::Tools {
        Tools::Pointer
    }

    fn get_drawable(&self) -> Option<&dyn super::Drawable> {
        self.drag.as_ref().map(|d| d.working.as_ref())
    }

    fn build_overlay(
        &self,
        selected: Option<&dyn Drawable>,
        device_pixel_ratio: f32,
    ) -> Option<Box<dyn Drawable>> {
        // Marquee rect during drag-rect selection.
        let marquee = self.marquee.as_ref().map(MarqueeState::rect);

        // Handles only for single-selection. Source from the live drag
        // working copy if present, otherwise from the drawable the renderer
        // passed in. We must NOT call back into `self.store` here — the
        // renderer holds a mutable borrow on its inner state across this
        // call, so re-entering would panic with a RefCell conflict.
        let handles: Vec<Handle> = if self.selected.len() == 1 {
            if let Some(d) = &self.drag {
                d.working.handles()
            } else {
                selected.map(|d| d.handles()).unwrap_or_default()
            }
        } else {
            Vec::new()
        };

        if marquee.is_none() && handles.is_empty() {
            return None;
        }
        Some(Box::new(SelectionOverlay {
            marquee,
            handles,
            device_pixel_ratio,
        }))
    }

    fn selected_drawables(&self) -> Vec<DrawableId> {
        self.selected.clone()
    }

    fn dragging_drawable_id(&self) -> Option<DrawableId> {
        self.drag.as_ref().map(|d| d.id)
    }

    fn input_enabled(&self) -> bool {
        self.input_enabled
    }

    fn set_input_enabled(&mut self, value: bool) {
        self.input_enabled = value;
    }

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>) {
        self.sender = Some(sender);
    }

    fn set_drawable_store(&mut self, store: Rc<dyn DrawableStore>) {
        self.store = Some(store);
    }

    fn handle_activated(&mut self) -> ToolUpdateResult {
        self.active_as_primary = true;
        ToolUpdateResult::Unmodified
    }

    fn handle_deactivated(&mut self) -> ToolUpdateResult {
        self.active_as_primary = false;
        // In-flight drag/marquee is dropped on tool switch; selection
        // persists via the implicit-selection mode.
        self.drag = None;
        self.marquee = None;
        ToolUpdateResult::Unmodified
    }

    fn handle_key_event(&mut self, event: KeyEventMsg) -> ToolUpdateResult {
        // Ctrl+A → select all.
        if event.modifier == ModifierType::CONTROL_MASK
            && (event.key == Key::a || event.key == Key::A)
        {
            let Some(store) = self.store.as_ref() else {
                return ToolUpdateResult::Unmodified;
            };
            let ids = store.all_drawable_ids();
            if ids.is_empty() {
                return ToolUpdateResult::Unmodified;
            }
            self.selected = ids;
            self.drag = None;
            self.marquee = None;
            return ToolUpdateResult::RedrawAndStopPropagation;
        }

        if !event.modifier.is_empty() {
            return ToolUpdateResult::Unmodified;
        }
        match event.key {
            Key::Delete | Key::BackSpace => {
                if self.selected.is_empty() {
                    return ToolUpdateResult::Unmodified;
                }
                let ids = std::mem::take(&mut self.selected);
                self.drag = None;
                if ids.len() == 1 {
                    ToolUpdateResult::DeleteDrawable(ids[0])
                } else {
                    ToolUpdateResult::DeleteDrawables(ids)
                }
            }
            Key::Escape => {
                if !self.selected.is_empty() || self.drag.is_some() || self.marquee.is_some() {
                    self.selected.clear();
                    self.drag = None;
                    self.marquee = None;
                    ToolUpdateResult::RedrawAndStopPropagation
                } else {
                    ToolUpdateResult::Unmodified
                }
            }
            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn handle_style_event(&mut self, style: Style) -> ToolUpdateResult {
        if self.selected.is_empty() {
            return ToolUpdateResult::Unmodified;
        }
        let Some(store) = self.store.as_ref() else {
            return ToolUpdateResult::Unmodified;
        };
        // Apply the new style to every selected drawable.
        let mut updates: Vec<(DrawableId, Box<dyn Drawable>)> = Vec::new();
        for &id in &self.selected {
            if let Some(mut d) = store.clone_drawable(id) {
                d.set_style(style);
                updates.push((id, d));
            }
        }
        if updates.is_empty() {
            return ToolUpdateResult::Unmodified;
        }
        if updates.len() == 1 {
            let (id, d) = updates.pop().unwrap();
            ToolUpdateResult::ModifyDrawable(id, d)
        } else {
            ToolUpdateResult::ModifyDrawables(updates)
        }
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        if event.button == MouseButton::Middle {
            return ToolUpdateResult::Unmodified;
        }
        let Some(store) = self.store.clone() else {
            return ToolUpdateResult::Unmodified;
        };

        match event.type_ {
            MouseEventType::BeginDrag => {
                // 1. Handle hit (single-selection only) takes priority.
                if let Some((id, drawable, handle)) = self.hit_handle(event.pos) {
                    self.drag = Some(DragState {
                        id,
                        mode: DragMode::Handle(handle.id),
                        original: drawable.clone_box(),
                        working: drawable,
                        handle_anchor: handle.pos,
                    });
                    return ToolUpdateResult::RedrawAndStopPropagation;
                }

                // 2. Body hit: replace selection with the clicked drawable
                //    (multi-drag isn't supported yet; clicking a member of
                //    a multi-selection collapses to single).
                if let Some(id) = store.hit_test(event.pos, HIT_TOLERANCE)
                    && let Some(drawable) = store.clone_drawable(id)
                {
                    self.selected = vec![id];
                    self.drag = Some(DragState {
                        id,
                        mode: DragMode::Body,
                        original: drawable.clone_box(),
                        working: drawable,
                        handle_anchor: Vec2D::zero(),
                    });
                    return ToolUpdateResult::RedrawAndStopPropagation;
                }

                // 3. Empty space.
                let had_selection = !self.selected.is_empty();
                if self.active_as_primary {
                    // Primary mode: start marquee-rect selection. Clear any
                    // existing selection first (will be replaced on EndDrag).
                    self.selected.clear();
                    self.marquee = Some(MarqueeState {
                        start: event.pos,
                        end: event.pos,
                    });
                    ToolUpdateResult::RedrawAndStopPropagation
                } else if had_selection {
                    // Implicit mode + had a selection: just clear; consume
                    // so drawing tools don't ALSO start drawing on this
                    // gesture. Also flag the follow-up Click so the active
                    // drawing tool (e.g. Marker) doesn't create a new shape
                    // when the user releases without moving.
                    self.selected.clear();
                    self.consume_next_click = true;
                    ToolUpdateResult::RedrawAndStopPropagation
                } else {
                    // Implicit mode + no selection: pass through so the
                    // drawing tool can start a new shape.
                    ToolUpdateResult::Unmodified
                }
            }
            MouseEventType::UpdateDrag => {
                // Marquee takes priority over body/handle drag.
                if let Some(m) = self.marquee.as_mut() {
                    // event.pos is delta from BeginDrag — start was set to
                    // BeginDrag's image-coord pos; new end is start + delta.
                    m.end = m.start + event.pos;
                    return ToolUpdateResult::RedrawAndStopPropagation;
                }
                let Some(drag) = self.drag.as_mut() else {
                    return ToolUpdateResult::Unmodified;
                };
                let mut working = drag.original.clone_box();
                match drag.mode {
                    DragMode::Body => working.translate(event.pos),
                    DragMode::Handle(h_id) => {
                        working.move_handle(h_id, drag.handle_anchor + event.pos)
                    }
                }
                drag.working = working;
                ToolUpdateResult::RedrawAndStopPropagation
            }
            MouseEventType::EndDrag => {
                // Marquee end: finalize selection from the rect.
                if let Some(m) = self.marquee.take() {
                    let rect = m.rect();
                    if rect.size.x.abs() < 1.0 && rect.size.y.abs() < 1.0 {
                        // Zero-area marquee — treat as plain click on empty,
                        // selection already cleared at BeginDrag.
                    } else {
                        self.selected = store.drawables_in_rect(rect);
                    }
                    return ToolUpdateResult::RedrawAndStopPropagation;
                }

                let Some(drag) = self.drag.take() else {
                    return ToolUpdateResult::Unmodified;
                };
                self.selected = vec![drag.id];
                if event.pos.is_zero() {
                    ToolUpdateResult::RedrawAndStopPropagation
                } else {
                    ToolUpdateResult::ModifyDrawable(drag.id, drag.working)
                }
            }
            MouseEventType::Click => {
                if self.consume_next_click {
                    self.consume_next_click = false;
                    return ToolUpdateResult::RedrawAndStopPropagation;
                }
                // Suppress the post-drag Click so drawing tools don't ALSO
                // act on it when the pointer just selected something.
                if store.hit_test(event.pos, HIT_TOLERANCE).is_some() {
                    ToolUpdateResult::RedrawAndStopPropagation
                } else {
                    ToolUpdateResult::Unmodified
                }
            }
            _ => ToolUpdateResult::Unmodified,
        }
    }
}
