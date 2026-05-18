use std::rc::Rc;
use std::time::Instant;

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
    Drawable, DrawableId, DrawableStore, Handle, HandleId, SELECTION_BLUE, Text, Tool,
    ToolUpdateResult, Tools, aspect_lock_corner_target, mirror_side_target,
};

/// Step (image-space px) for a plain arrow-key nudge.
const NUDGE_STEP: f32 = 1.0;
/// Step when Shift is held — order-of-magnitude bigger, matching
/// the convention in Figma / Sketch / Photoshop.
const NUDGE_STEP_SHIFT: f32 = 10.0;
/// A nudge that lands within this window of the previous one is
/// treated as part of the same OS auto-repeat burst and folded into
/// the same undo entry. Typical key-repeat is ~30–50 ms apart;
/// deliberate tap-tap is >150 ms apart, so 100 ms cleanly separates
/// the two.
const NUDGE_COALESCE_MS: u128 = 100;

pub const HIT_TOLERANCE: f32 = 6.0;
/// Generous radius (image units) for grabbing a selection handle.
/// Larger than the visible 12 px disc so users don't need pixel-
/// precise aim — matches typical "comfortably grabbable from
/// anywhere within ~20 px of the dot" feel.
pub const HANDLE_HIT_RADIUS: f32 = 20.0;
/// Marquee fill / stroke color (faded accent blue).
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
    /// Which drawing tool is currently active when we're being consulted
    /// in implicit mode. Used to gate body-grab / body-click on existing
    /// drawables: when this is `Some(t)` and the user clicks a drawable
    /// whose `tool_type()` differs from `t`, we fall through so the
    /// active drawing tool can place a fresh annotation on top instead
    /// of stealing the gesture to grab/move the existing one. `None`
    /// when Pointer is itself the active tool (no gating).
    implicit_other_tool: Option<Tools>,
    /// Set true when a BeginDrag in implicit mode just deselected because the
    /// user clicked empty space. The follow-up Click event is then suppressed
    /// so e.g. the Marker tool doesn't drop a counter on the same gesture.
    consume_next_click: bool,
    /// Timestamp of the most recent arrow-key nudge. Used to decide
    /// whether the next nudge is an OS auto-repeat tick (coalesce into
    /// the previous undo entry) or a fresh discrete press (new entry).
    last_nudge_at: Option<Instant>,
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
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

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

        // Handles: white outer disc + blue inner disc, scaled so the
        // on-screen size stays constant across zoom + DPR. The
        // image_units → physical-pixels pipeline lives in `css_to_image`
        // (= DPR ÷ image_to_canvas); `super::render_handles` reads it
        // out at the same scale used by the editing-mode handles in
        // `Text::draw_editing_handles`, so committed-selection and
        // mid-edit handles match pixel-for-pixel.
        let img_to_canvas = canvas.transform().average_scale().max(0.0001);
        let css_to_image = self.device_pixel_ratio / img_to_canvas;
        super::render_handles(canvas, &self.handles, css_to_image);
        canvas.restore();
        Ok(())
    }
}

impl PointerTool {
    /// True when we're being consulted implicitly and the hit drawable's
    /// owning tool doesn't match the active drawing tool — in which case
    /// the active tool wins the gesture.
    fn should_pass_through_body_hit(&self, drawable: &dyn Drawable) -> bool {
        let Some(active) = self.implicit_other_tool else {
            return false;
        };
        drawable.tool_type() != Some(active)
    }

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

    /// Translate every selected drawable by `delta` (image-space px) and
    /// return a Modify result. Used by arrow-key nudge. When `coalesce`
    /// is true, signals the renderer to fold this change into the
    /// previous Modify undo entry instead of pushing a new one — so a
    /// held-down arrow stays one undo step.
    fn nudge_selection(&self, delta: Vec2D, coalesce: bool) -> ToolUpdateResult {
        let Some(store) = self.store.as_ref() else {
            return ToolUpdateResult::Unmodified;
        };
        let mut updates: Vec<(DrawableId, Box<dyn Drawable>)> = Vec::new();
        for &id in &self.selected {
            if let Some(mut d) = store.clone_drawable(id) {
                d.translate(delta);
                updates.push((id, d));
            }
        }
        match (updates.len(), coalesce) {
            (0, _) => ToolUpdateResult::Unmodified,
            (1, false) => {
                let (id, d) = updates.pop().unwrap();
                ToolUpdateResult::ModifyDrawable(id, d)
            }
            (1, true) => {
                let (id, d) = updates.pop().unwrap();
                ToolUpdateResult::ModifyDrawableCoalesce(id, d)
            }
            (_, false) => ToolUpdateResult::ModifyDrawables(updates),
            (_, true) => ToolUpdateResult::ModifyDrawablesCoalesce(updates),
        }
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

        // Handles only for single-selection AND only when no drag is
        // in flight — hiding them mid-move / mid-resize lets the user
        // actually see where the shape's edges / endpoints will land
        // without the handle glyphs blocking the view. They reappear
        // when the drag ends (the drag state is cleared at that
        // point so this branch returns to drawing from `selected`).
        // We must NOT call back into `self.store` here — the
        // renderer holds a mutable borrow on its inner state across
        // this call, so re-entering would panic with a RefCell
        // conflict.
        let handles: Vec<Handle> = if self.selected.len() == 1 && self.drag.is_none() {
            selected.map(|d| d.handles()).unwrap_or_default()
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

    fn is_resizing(&self) -> bool {
        matches!(
            self.drag.as_ref().map(|d| d.mode),
            Some(DragMode::Handle(_))
        )
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

    fn set_implicit_other_tool(&mut self, tool: Option<Tools>) {
        self.implicit_other_tool = tool;
    }

    fn set_selected_drawables(&mut self, ids: Vec<DrawableId>) {
        // Drop any in-flight drag / marquee state — the caller is
        // typically replacing the selection wholesale (e.g. after a
        // Super+D duplicate just put fresh ids on the canvas), and
        // continuing an old drag against the new selection would
        // mutate the new drawables.
        self.selected = ids;
        self.drag = None;
        self.marquee = None;
    }

    fn handle_activated(&mut self) -> ToolUpdateResult {
        self.active_as_primary = true;
        self.implicit_other_tool = None;
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

        // Arrow-key nudge: move the selection by 1 px (10 px with Shift).
        // Accepts no modifier or Shift only; any other modifier falls
        // through so e.g. Alt+arrow (canvas pan) keeps working.
        let blocking_mods =
            ModifierType::CONTROL_MASK | ModifierType::ALT_MASK | ModifierType::SUPER_MASK;
        if !event.modifier.intersects(blocking_mods) && !self.selected.is_empty() {
            let step = if event.modifier.intersects(ModifierType::SHIFT_MASK) {
                NUDGE_STEP_SHIFT
            } else {
                NUDGE_STEP
            };
            let delta = match event.key {
                Key::Left => Some(Vec2D::new(-step, 0.0)),
                Key::Right => Some(Vec2D::new(step, 0.0)),
                Key::Up => Some(Vec2D::new(0.0, -step)),
                Key::Down => Some(Vec2D::new(0.0, step)),
                _ => None,
            };
            if let Some(delta) = delta {
                let now = Instant::now();
                let coalesce = self
                    .last_nudge_at
                    .map(|t| now.duration_since(t).as_millis() < NUDGE_COALESCE_MS)
                    .unwrap_or(false);
                self.last_nudge_at = Some(now);
                return self.nudge_selection(delta, coalesce);
            }
        }

        if !event.modifier.is_empty() {
            return ToolUpdateResult::Unmodified;
        }
        match event.key {
            Key::Delete | Key::BackSpace => {
                if self.selected.is_empty() {
                    return ToolUpdateResult::Unmodified;
                }
                // Locked drawables are spared from deletion. Affects
                // both single-target Delete and Ctrl+A + Delete bulk
                // paths: anything locked stays in the stack AND stays
                // selected so the user has something to act on after
                // unlocking.
                let store = self.store.as_ref();
                let (to_delete, to_keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.selected)
                    .into_iter()
                    .partition(|id| match &store {
                        Some(s) => !s.is_drawable_locked(*id),
                        None => true,
                    });
                self.selected = to_keep;
                if to_delete.is_empty() {
                    return ToolUpdateResult::Unmodified;
                }
                self.drag = None;
                if to_delete.len() == 1 {
                    ToolUpdateResult::DeleteDrawable(to_delete[0])
                } else {
                    ToolUpdateResult::DeleteDrawables(to_delete)
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
                    // Implicit mode + tool-type mismatch: yield so the
                    // active drawing tool places a fresh annotation on
                    // top instead of grabbing this one. (Pointer itself
                    // never sets `implicit_other_tool`, so explicit
                    // selection still works for any drawable.)
                    if self.should_pass_through_body_hit(drawable.as_ref()) {
                        return ToolUpdateResult::Unmodified;
                    }
                    // Auto-raise: if some other visible drawable overlaps
                    // this one from above, the user expects the click to
                    // bring it forward. Hit_test already returned this id
                    // as the topmost-at-pointer, but topmost-at-pointer
                    // ≠ topmost-among-overlappers (other overlappers may
                    // sit above us elsewhere along our bbox). Sketch_board
                    // performs the coalesced reorder in the result match.
                    let should_raise = store.has_visible_overlapper_above(id);

                    // BEFORE setting up a body-drag, check whether the
                    // click is on one of THIS drawable's handles. Lets
                    // the user start a Handle drag on an unselected
                    // drawable in a single click+drag gesture, instead
                    // of needing one click to select and a second to
                    // grab the handle. Without this, dragging a text's
                    // resize handle would either body-drag the text
                    // (wrong) or fall through to TextTool (creates a
                    // ghost text box).
                    if let Some(handle) = drawable
                        .handles()
                        .into_iter()
                        .find(|h| h.pos.distance_to(&event.pos) <= h.hit_radius)
                    {
                        self.selected = vec![id];
                        self.drag = Some(DragState {
                            id,
                            mode: DragMode::Handle(handle.id),
                            original: drawable.clone_box(),
                            working: drawable,
                            handle_anchor: handle.pos,
                        });
                        return if should_raise {
                            ToolUpdateResult::RaiseAndRedrawStop(id)
                        } else {
                            ToolUpdateResult::RedrawAndStopPropagation
                        };
                    }

                    self.selected = vec![id];
                    self.drag = Some(DragState {
                        id,
                        mode: DragMode::Body,
                        original: drawable.clone_box(),
                        working: drawable,
                        handle_anchor: Vec2D::zero(),
                    });
                    return if should_raise {
                        ToolUpdateResult::RaiseAndRedrawStop(id)
                    } else {
                        ToolUpdateResult::RedrawAndStopPropagation
                    };
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
                        let target = drag.handle_anchor + event.pos;
                        let shift = event.modifier.intersects(ModifierType::SHIFT_MASK);
                        let orig_bounds = drag.original.bounds();
                        match (shift, orig_bounds) {
                            (true, Some(orig)) => {
                                // Shift on corner: lock aspect ratio.
                                // Shift on side: grow opposite side symmetrically.
                                let constrained = aspect_lock_corner_target(orig, h_id, target);
                                working.move_handle(h_id, constrained);
                                if let Some((opp, mirrored)) =
                                    mirror_side_target(orig, h_id, target)
                                {
                                    working.move_handle(opp, mirrored);
                                }
                            }
                            _ => working.move_handle(h_id, target),
                        }
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
                // A click on the selected drawable's resize handle is a
                // selection interaction — consume it so it doesn't bubble
                // to a click-to-create tool (e.g. Marker dropping a fresh
                // counter). Handles sit outside the body, so the
                // `hit_test` below would miss and the click would
                // otherwise fall through. Covers both a bare click on a
                // handle and the Click GTK interleaves into a
                // BeginDrag→Click→…→EndDrag handle-resize gesture.
                if self.hit_handle(event.pos).is_some() {
                    return ToolUpdateResult::RedrawAndStopPropagation;
                }
                let hit = store.hit_test(event.pos, HIT_TOLERANCE);
                // Implicit mode + tool-type mismatch: don't select or
                // consume — let the click propagate to the active
                // drawing tool so e.g. a Marker count gets dropped where
                // the user clicked, even if it lands over an existing
                // shape of another type. Skip this gate for double-clicks
                // on Text (the edit-text affordance below stays useful).
                if event.n_pressed != 2
                    && let Some(id) = hit
                    && let Some(drawable) = store.clone_drawable(id)
                    && self.should_pass_through_body_hit(drawable.as_ref())
                {
                    let _ = id;
                    return ToolUpdateResult::Unmodified;
                }

                // Double-click on a Text drawable: switch to TextTool and
                // resume editing. sketch_board catches the variant and
                // wires up the tool transition. Clearing drag/marquee is
                // safe here because we're abandoning this gesture entirely
                // in favor of the tool switch.
                if event.n_pressed == 2
                    && let Some(id) = hit
                    && let Some(d) = store.clone_drawable(id)
                    && d.as_any().is::<Text>()
                {
                    self.selected.clear();
                    self.drag = None;
                    self.marquee = None;
                    return ToolUpdateResult::EditTextDrawable(id);
                }
                // GTK fires events in order: BeginDrag → Click → UpdateDrag
                // → EndDrag. BeginDrag has already set up `self.selected`
                // and `self.drag` if this click hit an existing drawable;
                // touching them here would clobber the body-drag state
                // before UpdateDrag/EndDrag can use it, breaking move
                // entirely. We only need to consume the Click so it doesn't
                // propagate to the active drawing tool. As a safety net,
                // populate `selected` when `drag` is `None` (the rare path
                // where BeginDrag didn't fire — e.g. a release-only event
                // synthesized by GTK in some edge cases) so Delete-after-tap
                // still works.
                if let Some(id) = hit {
                    if self.drag.is_none() && self.marquee.is_none() {
                        self.selected = vec![id];
                    }
                    return ToolUpdateResult::RedrawAndStopPropagation;
                }
                ToolUpdateResult::Unmodified
            }
            _ => ToolUpdateResult::Unmodified,
        }
    }
}
