use std::{
    any::Any,
    borrow::Cow,
    cell::RefCell,
    collections::HashMap,
    fmt::{Debug, Display},
    rc::Rc,
};

use anyhow::Result;
use femtovg::{Canvas, FontId, Paint, Path as FemtoPath, renderer::OpenGl};
use relm4::gtk::gdk_pixbuf::{
    glib::{Variant, VariantTy},
    prelude::{StaticVariantType, ToVariant},
};

use relm4::gtk::glib::variant::FromVariant;
use relm4::{
    Sender,
    gtk::{self, IMMulticontext},
};
use serde_derive::Deserialize;

use crate::{
    math::{Rect, Vec2D},
    sketch_board::{InputEvent, KeyEventMsg, MouseEventMsg, SketchBoardInput, TextEventMsg},
    style::Style,
};

use satty_cli::command_line;

mod arrow;
mod blur;
mod brush;
mod crop;
mod ellipse;
mod highlight;
mod spotlight;
mod line;
mod marker;
mod pointer;
mod rectangle;
mod text;

pub enum ToolEvent {
    Activated,
    Deactivated,
    Input(InputEvent),
    StyleChanged(Style),
}

pub trait Tool {
    fn handle_event(&mut self, event: ToolEvent) -> ToolUpdateResult {
        match event {
            ToolEvent::Activated => self.handle_activated(),
            ToolEvent::Deactivated => self.handle_deactivated(),
            ToolEvent::Input(e) => self.handle_input_event(e),
            ToolEvent::StyleChanged(s) => self.handle_style_event(s),
        }
    }

    fn handle_activated(&mut self) -> ToolUpdateResult {
        ToolUpdateResult::Unmodified
    }

    fn handle_deactivated(&mut self) -> ToolUpdateResult {
        ToolUpdateResult::Unmodified
    }

    fn handle_input_event(&mut self, event: InputEvent) -> ToolUpdateResult {
        match event {
            InputEvent::Mouse(e) => self.handle_mouse_event(e),
            InputEvent::Key(e) => self.handle_key_event(e),
            InputEvent::KeyRelease(e) => self.handle_key_release_event(e),
            InputEvent::Text(e) => self.handle_text_event(e),
        }
    }

    fn handle_text_event(&mut self, event: TextEventMsg) -> ToolUpdateResult {
        let _ = event;
        ToolUpdateResult::Unmodified
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        let _ = event;
        ToolUpdateResult::Unmodified
    }

    fn handle_key_event(&mut self, event: KeyEventMsg) -> ToolUpdateResult {
        let _ = event;
        ToolUpdateResult::Unmodified
    }

    fn handle_key_release_event(&mut self, event: KeyEventMsg) -> ToolUpdateResult {
        let _ = event;
        ToolUpdateResult::Unmodified
    }

    fn handle_style_event(&mut self, style: Style) -> ToolUpdateResult {
        let _ = style;
        ToolUpdateResult::Unmodified
    }

    fn active(&self) -> bool {
        false
    }

    fn input_enabled(&self) -> bool;

    fn set_input_enabled(&mut self, value: bool);

    fn handle_undo(&mut self) -> ToolUpdateResult {
        ToolUpdateResult::Unmodified
    }

    fn handle_redo(&mut self) -> ToolUpdateResult {
        ToolUpdateResult::Unmodified
    }

    fn set_im_context(&mut self, _context: Option<InputContext>) {}

    fn get_drawable(&self) -> Option<&dyn Drawable>;

    /// Optional overlay drawn on top of the in-progress drawable
    /// (e.g. selection handles). Computed fresh each frame so visuals stay in
    /// sync after undo/redo or external mutations.
    ///
    /// `selected` is the live drawable matching `selected_drawable()` from the
    /// renderer's stack, passed in to avoid the tool re-entering the renderer
    /// (which already holds a borrow during the render path).
    ///
    /// `device_pixel_ratio` is the host display's DPR (1 on standard, 2 on
    /// retina). Tools use it to size visuals in CSS pixels while still
    /// looking sharp on HiDPI screens.
    fn build_overlay(
        &self,
        _selected: Option<&dyn Drawable>,
        _device_pixel_ratio: f32,
    ) -> Option<Box<dyn Drawable>> {
        None
    }

    /// If the tool has a current selection, return its id. Used by the renderer
    /// to know which drawable is selected (for selection visuals when not dragging).
    fn selected_drawable(&self) -> Option<DrawableId> {
        self.selected_drawables().first().copied()
    }

    /// All currently-selected drawable ids. Default returns single selection
    /// (or empty); tools that support multi-selection override.
    fn selected_drawables(&self) -> Vec<DrawableId> {
        Vec::new()
    }

    /// If the tool is actively dragging an existing drawable, return its id.
    /// Used by the renderer to skip rendering the original (the tool's
    /// `get_drawable()` returns a moved copy during the drag).
    fn dragging_drawable_id(&self) -> Option<DrawableId> {
        None
    }

    /// True when the tool is currently dragging a resize handle (vs a
    /// body / move drag). Sketch_board hides the cursor during a resize
    /// drag so the user can see exactly where the dragged edge or
    /// corner lands.
    fn is_resizing(&self) -> bool {
        false
    }

    fn get_tool_type(&self) -> Tools;

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>);

    /// Inject a handle the tool can use to query the committed-drawable stack.
    /// Currently only the pointer tool needs this (for hit-testing and pulling
    /// a working copy of a selection).
    fn set_drawable_store(&mut self, _store: Rc<dyn DrawableStore>) {}

    /// Switch the arrow geometry (only meaningful for `ArrowTool`). Default
    /// no-op so the toolbar can broadcast without checking tool type.
    fn set_arrow_style(&mut self, _style: ArrowStyle) {}

    /// Switch the blur algorithm (only meaningful for `BlurTool`).
    /// Default no-op for the same reason as `set_arrow_style`.
    fn set_blur_style(&mut self, _style: BlurStyle) {}

    /// Switch the background style for new text drawables (only
    /// meaningful for `TextTool`). Default no-op so the toolbar can
    /// broadcast without checking tool type.
    fn set_text_background(&mut self, _bg: TextBackground) {}

    /// Resume editing an existing committed text drawable. Only `TextTool`
    /// implements this; the default no-op lets sketch_board dispatch
    /// uniformly. Returns true if the tool accepted the request.
    fn enter_text_edit_mode(
        &mut self,
        _id: DrawableId,
        _drawable: Box<dyn Drawable>,
    ) -> bool {
        false
    }

    /// Handles attached to the tool's in-progress drawable that should
    /// participate in cursor hit-testing (resize cursors on hover).
    /// Used by sketch_board's `update_hover_cursor` so editing-mode
    /// handles light up the same way committed-selection handles do.
    /// Default empty — only `TextTool` currently exposes editing
    /// handles outside the committed `Drawable::handles()` path.
    fn editing_handles(&self) -> Vec<Handle> {
        Vec::new()
    }

    /// Image-space rect covering the tool's in-progress editable body,
    /// used by sketch_board's `update_hover_cursor` to swap to an
    /// i-beam when the pointer is over an actively-editing region
    /// (currently only `TextTool` populates this). `None` means "no
    /// active editing body" and the cursor falls through to the
    /// default tool cursor.
    fn editing_body_rect(&self) -> Option<Rect> {
        None
    }
}

/// Read-only view of the committed-drawable stack, exposed to tools that need
/// to do hit-testing or pull working copies of existing drawables.
pub trait DrawableStore {
    fn hit_test(&self, point: Vec2D, tolerance: f32) -> Option<DrawableId>;
    fn clone_drawable(&self, id: DrawableId) -> Option<Box<dyn Drawable>>;
    /// Drawable ids whose bounds overlap `rect`. Used for marquee / lasso
    /// selection.
    fn drawables_in_rect(&self, rect: Rect) -> Vec<DrawableId>;
    /// All committed drawable ids (back-to-front order). Used for Ctrl+A.
    fn all_drawable_ids(&self) -> Vec<DrawableId>;
}

#[derive(Clone, Debug)]
pub struct InputContext {
    pub im_context: IMMulticontext,
    pub widget: gtk::Widget,
}

// the clone method below has been adapted from: https://stackoverflow.com/questions/30353462/how-to-clone-a-struct-storing-a-boxed-trait-object
// it feels "strange" and especially the fact that drawable has to derive from DrawableClone feels "wrong".
pub trait DrawableClone {
    fn clone_box(&self) -> Box<dyn Drawable>;
}

impl<T> DrawableClone for T
where
    T: 'static + Drawable + Clone,
{
    fn clone_box(&self) -> Box<dyn Drawable> {
        Box::new(self.clone())
    }
}

pub trait Drawable: DrawableClone + Debug {
    fn draw(&self, canvas: &mut Canvas<OpenGl>, font: FontId, bounds: (Vec2D, Vec2D))
    -> Result<()>;
    fn handle_undo(&mut self) {}
    fn handle_redo(&mut self) {}

    /// Marker for spotlight drawables. The renderer skips these in the
    /// main draw pass and applies them as a single inverse-mask overlay
    /// at the end (so multiple spotlight shapes union correctly into one
    /// dark layer, with the global slider value controlling its alpha).
    /// Default false; only `spotlight::SpotlightKind` overrides.
    fn is_spotlight(&self) -> bool {
        false
    }

    /// Add this drawable's silhouette to `path` in image-space units.
    /// Used by the renderer's spotlight pass to build the punch-out mask
    /// — the renderer fills `path` with composite=DestinationOut, so
    /// each spotlight's shape erases the dark overlay where the user
    /// drew it. Default no-op; only spotlight drawables implement it.
    fn append_spotlight_path(&self, _path: &mut FemtoPath) {}

    /// Type-erased downcast hook. Returns `&self` typed as `&dyn Any` so
    /// callers that need concrete-type access (e.g. PointerTool's
    /// double-click-to-edit-text path) can `downcast_ref::<ConcreteType>()`.
    /// Each impl provides the one-line override; the trait itself can't
    /// default this because `&self` is type-erased at the trait-object
    /// boundary.
    fn as_any(&self) -> &dyn Any;

    /// Axis-aligned bounding box in image coordinates. `None` means "not selectable"
    /// (e.g. an in-progress drawable still being drawn).
    fn bounds(&self) -> Option<Rect> {
        None
    }

    /// Whether `point` (image coordinates) hits this drawable.
    /// `tolerance` is extra picking slack in image-space pixels — sketch_board passes
    /// a value scaled to the current zoom.
    /// Default falls through to bounds-containment, which is correct for filled shapes.
    fn hit_test(&self, point: Vec2D, tolerance: f32) -> bool {
        self.bounds()
            .map(|b| b.inflated(tolerance).contains(point))
            .unwrap_or(false)
    }

    /// Translate the drawable by `delta` (image coordinates).
    /// Default is a no-op so non-movable drawables (e.g. crop overlays) don't need
    /// to implement it.
    fn translate(&mut self, _delta: Vec2D) {}

    /// Handles to expose for direct manipulation when this drawable is selected.
    /// Default is empty (move-only).
    fn handles(&self) -> Vec<Handle> {
        Vec::new()
    }

    /// Move a handle to `to` (image coordinates). The drawable updates itself
    /// according to the handle's semantics (e.g. arrow endpoint, rect corner).
    /// Default is a no-op.
    fn move_handle(&mut self, _handle: HandleId, _to: Vec2D) {}

    /// Apply a new style to the drawable (color, size, fill, …). Used when the
    /// user picks a different color/size in the toolbar while a drawable is
    /// selected, so the toolbar acts on the selection rather than only on
    /// future shapes. Default is a no-op for drawables that don't carry a
    /// mutable style.
    fn set_style(&mut self, _style: Style) {}

    /// Current style of the drawable, when it has one. Used by the
    /// sketch board to sync toolbar controls (size slider, color
    /// chip, fill toggle) to whichever shape is currently selected —
    /// so the user sees the *current* shape's size in the slider
    /// rather than the last-typed value. Default `None` for drawables
    /// that don't carry a mutable style.
    fn style(&self) -> Option<Style> {
        None
    }

    /// Apply the text-pill background style (Plain vs Rounded) to a
    /// committed Text drawable. Default no-op — only Text overrides
    /// this so the dropdown in the StyleToolbar can restyle a
    /// selected text after the fact, not just at creation time.
    fn set_text_background(&mut self, _bg: TextBackground) {}

    /// Read the text-pill background style (Plain vs Rounded) off a
    /// Text drawable. `None` for any drawable that doesn't carry a
    /// text background — the toolbar uses this on selection-sync to
    /// flip its dropdown to the just-selected drawable's value so
    /// the on-screen affordance reflects whatever is currently
    /// clicked.
    fn text_background(&self) -> Option<TextBackground> {
        None
    }

    /// Apply / read the arrow-geometry variant on an Arrow drawable.
    /// Symmetric with the text-background pair so popover picks,
    /// double-tap cycles, and selection-driven re-syncs all work
    /// against the *selected* drawable instead of just the tool's
    /// default. Default `None` / no-op for non-arrows.
    fn set_arrow_style_on_drawable(&mut self, _style: ArrowStyle) {}
    fn arrow_style(&self) -> Option<ArrowStyle> {
        None
    }

    /// Apply / read the blur algorithm on a Blur drawable. Same
    /// shape as `set_arrow_style_on_drawable` / `arrow_style`.
    fn set_blur_style_on_drawable(&mut self, _style: BlurStyle) {}
    fn blur_style(&self) -> Option<BlurStyle> {
        None
    }

    /// Which tool created this drawable. Used by sketch_board to
    /// auto-switch the active tool when the user selects an existing
    /// drawable, so the toolbar's tool-specific controls (arrow
    /// style chip, blur algorithm, text-background dropdown, etc.)
    /// match the picked shape. `None` for internal drawables that
    /// shouldn't trigger a switch (selection overlay).
    fn tool_type(&self) -> Option<Tools> {
        None
    }

    /// Render a selection "glow" — a semi-transparent blue trace of the 
    /// shape, drawn under the original. Each shape's impl chooses how 
    /// to map `HALO_PAD` (a CSS-pixel target) into image units using 
    /// `glow_scale_image_units` so the halo appears at constant on-screen 
    /// thickness regardless of zoom or DPR.
    fn render_glow(
        &self,
        canvas: &mut Canvas<OpenGl>,
        _font: FontId,
        _bounds: (Vec2D, Vec2D),
        device_pixel_ratio: f32,
    ) -> Result<()> {
        let Some(b) = self.bounds() else {
            return Ok(());
        };
        canvas.save();
        let halo = halo_in_image_units(canvas, device_pixel_ratio);
        let inflate = halo / 2.0;
        let mut path = FemtoPath::new();
        path.rounded_rect(
            b.pos.x - inflate,
            b.pos.y - inflate,
            b.size.x + inflate * 2.0,
            b.size.y + inflate * 2.0,
            6.0,
        );
        let mut paint = Paint::color(GLOW_COLOR);
        paint.set_line_width(halo);
        canvas.stroke_path(&path, &paint);
        canvas.restore();
        Ok(())
    }
}

/// Convert `HALO_PAD` (CSS pixels) into image units given the canvas's
/// current image→canvas transform and the host display's DPR. Use this
/// inside any `render_glow` impl that wants a halo of constant on-screen
/// thickness.
pub fn halo_in_image_units(
    canvas: &Canvas<OpenGl>,
    device_pixel_ratio: f32,
) -> f32 {
    let img_to_canvas = canvas.transform().average_scale().max(0.0001);
    let css_to_image = device_pixel_ratio / img_to_canvas;
    HALO_PAD * css_to_image
}

/// Selection accent colour (used for handles + glow + hover cursor halo).
pub const SELECTION_BLUE: femtovg::Color = femtovg::Color {
    r: 0.18,
    g: 0.53,
    b: 0.87,
    a: 1.0,
};
/// Semi-transparent variant for the glow trace.
pub const GLOW_COLOR: femtovg::Color = femtovg::Color {
    r: 0.18,
    g: 0.53,
    b: 0.87,
    a: 0.45,
};
/// Visible halo width (in CSS pixels) — the band of GLOW_COLOR shown
/// outside each selected drawable's silhouette. Per-shape `render_glow`
/// impls translate this into stroke widths, fill insets, etc., and
/// scale it via `halo_in_image_units` so the on-screen size is constant
/// regardless of zoom or DPR.
pub const HALO_PAD: f32 = 4.0;

/// Visual shape used by the SelectionOverlay to render a handle.
/// Round is the standard "resize a side/corner" affordance;
/// Square signals a different semantic (e.g. text's bottom-right
/// corner scales font size + width together, not just resize).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HandleKind {
    #[default]
    Round,
    Square,
}

/// A handle exposed by a drawable for direct manipulation.
#[derive(Debug, Clone, Copy)]
pub struct Handle {
    pub id: HandleId,
    pub pos: Vec2D,
    /// Hit-test radius in image units. Defaults to `HANDLE_HIT_RADIUS`;
    /// drawables can opt into a bigger target (e.g. Curved/Double arrows
    /// where the midpoint handle sits on a wide shaft) via `with_hit_radius`.
    pub hit_radius: f32,
    /// Visual style — Round (default) or Square. Per-handle so a single
    /// drawable can mix shapes (e.g. text uses Round side handles + a
    /// Square bottom-right corner).
    pub kind: HandleKind,
}

impl Handle {
    pub fn new(id: HandleId, pos: Vec2D) -> Self {
        Self {
            id,
            pos,
            hit_radius: pointer::HANDLE_HIT_RADIUS,
            kind: HandleKind::Round,
        }
    }

    pub fn with_hit_radius(mut self, r: f32) -> Self {
        self.hit_radius = r;
        self
    }

    pub fn with_kind(mut self, kind: HandleKind) -> Self {
        self.kind = kind;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleId {
    /// Linear-shape endpoints (arrow, line).
    Start,
    End,
    /// Mid-shape control point (curved/double arrow Bezier control).
    Control,
    /// Bounding-box corners.
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    /// Bounding-box edge midpoints.
    Top,
    Right,
    Bottom,
    Left,
}

/// 8 standard bounding-box handles (4 corners, 4 edge midpoints).
/// Shared by rectangle / ellipse / blur / highlight-block.
pub fn bbox_handles(rect: Rect) -> Vec<Handle> {
    let tl = rect.top_left();
    let tr = rect.top_right();
    let bl = rect.bottom_left();
    let br = rect.bottom_right();
    let center = rect.center();
    vec![
        Handle::new(HandleId::TopLeft, tl),
        Handle::new(HandleId::TopRight, tr),
        Handle::new(HandleId::BottomLeft, bl),
        Handle::new(HandleId::BottomRight, br),
        Handle::new(HandleId::Top, Vec2D::new(center.x, tl.y)),
        Handle::new(HandleId::Bottom, Vec2D::new(center.x, br.y)),
        Handle::new(HandleId::Left, Vec2D::new(tl.x, center.y)),
        Handle::new(HandleId::Right, Vec2D::new(br.x, center.y)),
    ]
}

/// Resize a canonical bounding box given a handle being dragged to `to`.
/// Returns a canonicalized rect.
pub fn bbox_resize(rect: Rect, handle: HandleId, to: Vec2D) -> Rect {
    let tl = rect.top_left();
    let br = rect.bottom_right();
    let (new_tl, new_br) = match handle {
        HandleId::TopLeft => (to, br),
        HandleId::TopRight => (Vec2D::new(tl.x, to.y), Vec2D::new(to.x, br.y)),
        HandleId::BottomLeft => (Vec2D::new(to.x, tl.y), Vec2D::new(br.x, to.y)),
        HandleId::BottomRight => (tl, to),
        HandleId::Top => (Vec2D::new(tl.x, to.y), br),
        HandleId::Bottom => (tl, Vec2D::new(br.x, to.y)),
        HandleId::Left => (Vec2D::new(to.x, tl.y), br),
        HandleId::Right => (tl, Vec2D::new(to.x, br.y)),
        _ => return rect,
    };
    Rect::from_corners(new_tl, new_br)
}

/// Constrain a corner-handle target so the resulting bbox preserves the
/// original aspect ratio. Used when Shift is held while dragging a corner.
///
/// The dominant axis (whichever the user has scaled more, measured from
/// the pinned opposite corner) wins; the other axis is snapped to match.
/// Returns `to` unchanged if `handle` is not a corner or `orig` is degenerate.
pub fn aspect_lock_corner_target(orig: Rect, handle: HandleId, to: Vec2D) -> Vec2D {
    let anchor = match handle {
        HandleId::TopLeft => orig.bottom_right(),
        HandleId::TopRight => orig.bottom_left(),
        HandleId::BottomLeft => orig.top_right(),
        HandleId::BottomRight => orig.top_left(),
        _ => return to,
    };
    if orig.size.x <= f32::EPSILON || orig.size.y <= f32::EPSILON {
        return to;
    }
    let dx = to.x - anchor.x;
    let dy = to.y - anchor.y;
    let scale_x = dx.abs() / orig.size.x;
    let scale_y = dy.abs() / orig.size.y;
    let scale = scale_x.max(scale_y);
    let sign_x = if dx >= 0.0 { 1.0 } else { -1.0 };
    let sign_y = if dy >= 0.0 { 1.0 } else { -1.0 };
    Vec2D::new(
        anchor.x + sign_x * scale * orig.size.x,
        anchor.y + sign_y * scale * orig.size.y,
    )
}

/// For a side-handle drag with Shift held, compute the opposite side's
/// handle id and the mirrored target that produces symmetric resize about
/// the original bbox's center on the dragged axis.
///
/// Returns `None` if `handle` is not a side handle. Callers should apply
/// `move_handle(handle, to)` first, then `move_handle(opp, mirrored)` so
/// shapes with vertex-scaling resize (brush, highlight) compose correctly.
pub fn mirror_side_target(
    orig: Rect,
    handle: HandleId,
    to: Vec2D,
) -> Option<(HandleId, Vec2D)> {
    let tl = orig.top_left();
    let br = orig.bottom_right();
    match handle {
        HandleId::Top => {
            let mirrored_y = br.y - (to.y - tl.y);
            Some((HandleId::Bottom, Vec2D::new(br.x, mirrored_y)))
        }
        HandleId::Bottom => {
            let mirrored_y = tl.y - (to.y - br.y);
            Some((HandleId::Top, Vec2D::new(tl.x, mirrored_y)))
        }
        HandleId::Left => {
            let mirrored_x = br.x - (to.x - tl.x);
            Some((HandleId::Right, Vec2D::new(mirrored_x, br.y)))
        }
        HandleId::Right => {
            let mirrored_x = tl.x - (to.x - br.x);
            Some((HandleId::Left, Vec2D::new(mirrored_x, tl.y)))
        }
        _ => None,
    }
}

/// Identifier for a committed drawable on the sketch stack.
/// Stable across moves, edits, and undo/redo cycles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DrawableId(pub u64);

/// A drawable that has been committed to the stack, paired with its stable ID.
#[derive(Debug)]
pub struct Stacked {
    pub id: DrawableId,
    pub drawable: Box<dyn Drawable>,
}

#[derive(Debug)]
pub enum ToolUpdateResult {
    Commit(Box<dyn Drawable>),
    /// Replace the existing drawable identified by `DrawableId` with the new one.
    /// Recorded as a single Modify undo action.
    ModifyDrawable(DrawableId, Box<dyn Drawable>),
    /// Replace many drawables atomically (Batch undo). Used for multi-select
    /// restyle.
    ModifyDrawables(Vec<(DrawableId, Box<dyn Drawable>)>),
    /// Same as `ModifyDrawable` but merge with the previous undo entry
    /// if it's already a Modify for the same id. Used by arrow-key
    /// nudge so a held-down arrow produces one undo step rather than
    /// one per OS auto-repeat tick.
    ModifyDrawableCoalesce(DrawableId, Box<dyn Drawable>),
    /// Multi-select counterpart of `ModifyDrawableCoalesce`.
    ModifyDrawablesCoalesce(Vec<(DrawableId, Box<dyn Drawable>)>),
    /// Remove the drawable from the stack. Recorded as a Remove undo action so
    /// it can be restored.
    DeleteDrawable(DrawableId),
    /// Remove a set of drawables atomically. Recorded as a single Batch undo
    /// action so one Ctrl+Z restores them all.
    DeleteDrawables(Vec<DrawableId>),
    /// Request that sketch_board switch to the Text tool and resume
    /// editing the drawable with this id. Emitted by `PointerTool` on
    /// double-click of a Text drawable. The drawable itself is not
    /// passed — sketch_board fetches it via the renderer.
    EditTextDrawable(DrawableId),
    Redraw,
    Unmodified,
    StopPropagation,
    RedrawAndStopPropagation,
}

/// A reversible change to the drawable stack. Stored on undo/redo stacks.
///
/// Same variant moves between stacks for `Modify`. `Add` and `Remove` are paired:
/// undoing an `Add` produces a `Remove` on the redo stack (and vice versa), since
/// the live drawable storage location differs between the two states.
#[derive(Debug)]
pub enum UndoAction {
    /// A drawable with this id was added; it currently lives in the stack.
    Add(DrawableId),
    /// A drawable was removed; this action holds it until restored.
    Remove {
        id: DrawableId,
        idx: usize,
        drawable: Box<dyn Drawable>,
    },
    /// A drawable was modified. `prev` is the state to restore on the next swap.
    /// (After a swap, `prev` becomes the *new* previous, so the same variant can
    /// move between undo/redo stacks symmetrically.)
    Modify {
        id: DrawableId,
        prev: Box<dyn Drawable>,
    },
    /// Group of actions applied/reversed atomically — single Ctrl+Z undoes
    /// the whole group. Used for multi-select operations like deleting a set
    /// of drawables at once.
    Batch(Vec<UndoAction>),
    /// Canvas was auto-extended to fit a drawable that spilled past the
    /// previous image bounds. Holds the pre-extension Pixbuf, the
    /// `(left, top)` translation applied to the listed drawables when
    /// the strips were prepended, and the ids of drawables that were
    /// translated. The triggering Add/Modify's drawable is excluded
    /// from `translated_ids` because the Add's stored payload or the
    /// Modify's `prev` were captured pre-translation, so the
    /// inverse-of-inverse cycle would translate them twice. Always
    /// paired with the triggering `Add` / `Modify` inside a `Batch`
    /// so one Ctrl+Z reverses both. Batch order: ResizeCanvas first,
    /// triggering action second.
    ResizeCanvas {
        prev_image: gtk::gdk_pixbuf::Pixbuf,
        applied_offset: Vec2D,
        translated_ids: Vec<DrawableId>,
    },
}

pub use arrow::{ArrowStyle, ArrowTool};
pub use blur::{BlurStyle, BlurTool};
pub use crop::{AspectRatio, CropBgColor, CropHit, CropTool};
pub use ellipse::EllipseTool;
pub use highlight::{HighlightTool, Highlighters};
pub use line::LineTool;
pub use rectangle::RectangleTool;
pub use spotlight::SpotlightTool;
pub use text::{Text, TextBackground, TextTool};

use self::{brush::BrushTool, marker::MarkerTool, pointer::PointerTool};

// Re-export pointer-tool tunables that other modules (e.g. sketch_board's
// hover cursor) want to share.
pub use self::pointer::{HANDLE_HIT_RADIUS, HIT_TOLERANCE};

#[derive(
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Clone,
    Copy,
    Hash,
    Deserialize,
    serde::Serialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Tools {
    Pointer = 0,
    Crop = 1,
    Line = 2,
    Arrow = 3,
    Rectangle = 4,
    Ellipse = 5,
    Text = 6,
    Marker = 7,
    Blur = 8,
    Highlighter = 9,
    Brush = 10,
    Spotlight = 11,
}

impl Tools {
    pub fn display_name(&self) -> &'static str {
        match self {
            Tools::Pointer => "Pointer",
            Tools::Crop => "Crop",
            Tools::Brush => "Brush",
            Tools::Line => "Line",
            Tools::Arrow => "Arrow",
            Tools::Rectangle => "Rectangle",
            Tools::Ellipse => "Ellipse",
            Tools::Text => "Text",
            Tools::Marker => "Numbered Marker",
            Tools::Blur => "Blur",
            Tools::Highlighter => "Highlighter",
            Tools::Spotlight => "Spotlight",
        }
    }
}

// used for printing
impl Display for Tools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pointer => write!(f, "pointer"),
            Self::Crop => write!(f, "crop"),
            Self::Line => write!(f, "line"),
            Self::Arrow => write!(f, "arrow"),
            Self::Rectangle => write!(f, "rectangle"),
            Self::Ellipse => write!(f, "ellipse"),
            Self::Text => write!(f, "text"),
            Self::Marker => write!(f, "marker"),
            Self::Blur => write!(f, "blur"),
            Self::Highlighter => write!(f, "highlighter"),
            Self::Brush => write!(f, "brush"),
            Self::Spotlight => write!(f, "spotlight"),
        }
    }
}

pub struct ToolsManager {
    tools: HashMap<Tools, Rc<RefCell<dyn Tool>>>,
    crop_tool: Rc<RefCell<CropTool>>,
}

impl ToolsManager {
    pub fn new() -> Self {
        let mut tools: HashMap<Tools, Rc<RefCell<dyn Tool>>> = HashMap::new();
        //tools.insert(Tools::Crop, Rc::new(RefCell::new(CropTool::default())));
        tools.insert(
            Tools::Pointer,
            Rc::new(RefCell::new(PointerTool::default())),
        );
        tools.insert(Tools::Line, Rc::new(RefCell::new(LineTool::default())));
        tools.insert(Tools::Arrow, Rc::new(RefCell::new(ArrowTool::default())));
        tools.insert(
            Tools::Rectangle,
            Rc::new(RefCell::new(RectangleTool::default())),
        );
        tools.insert(
            Tools::Ellipse,
            Rc::new(RefCell::new(EllipseTool::default())),
        );
        tools.insert(Tools::Text, Rc::new(RefCell::new(TextTool::default())));
        tools.insert(Tools::Blur, Rc::new(RefCell::new(BlurTool::default())));
        tools.insert(
            Tools::Highlighter,
            Rc::new(RefCell::new(HighlightTool::default())),
        );
        tools.insert(Tools::Marker, Rc::new(RefCell::new(MarkerTool::default())));
        tools.insert(Tools::Brush, Rc::new(RefCell::new(BrushTool::default())));
        tools.insert(
            Tools::Spotlight,
            Rc::new(RefCell::new(SpotlightTool::default())),
        );

        let crop_tool = Rc::new(RefCell::new(CropTool::default()));
        Self { tools, crop_tool }
    }

    pub fn get(&self, tool: &Tools) -> Rc<RefCell<dyn Tool>> {
        match tool {
            Tools::Crop => self.crop_tool.clone(),
            _ => self
                .tools
                .get(tool)
                .unwrap_or_else(|| {
                    panic!("Did you add the requested too {tool:#?} to the tools HashMap?")
                })
                .clone(),
        }
    }

    pub fn get_crop_tool(&self) -> Rc<RefCell<CropTool>> {
        self.crop_tool.clone()
    }
}

impl StaticVariantType for Tools {
    fn static_variant_type() -> Cow<'static, VariantTy> {
        Cow::Borrowed(VariantTy::UINT32)
    }
}

impl ToVariant for Tools {
    fn to_variant(&self) -> Variant {
        Variant::from(*self as u32)
    }
}

impl FromVariant for Tools {
    fn from_variant(variant: &Variant) -> Option<Self> {
        variant.get::<u32>().and_then(|v| match v {
            0 => Some(Tools::Pointer),
            1 => Some(Tools::Crop),
            2 => Some(Tools::Line),
            3 => Some(Tools::Arrow),
            4 => Some(Tools::Rectangle),
            5 => Some(Tools::Ellipse),
            6 => Some(Tools::Text),
            7 => Some(Tools::Marker),
            8 => Some(Tools::Blur),
            9 => Some(Tools::Highlighter),
            10 => Some(Tools::Brush),
            11 => Some(Tools::Spotlight),
            _ => None,
        })
    }
}

impl From<command_line::Tools> for Tools {
    fn from(tool: command_line::Tools) -> Self {
        match tool {
            command_line::Tools::Pointer => Self::Pointer,
            command_line::Tools::Crop => Self::Crop,
            command_line::Tools::Line => Self::Line,
            command_line::Tools::Arrow => Self::Arrow,
            command_line::Tools::Rectangle => Self::Rectangle,
            command_line::Tools::Ellipse => Self::Ellipse,
            command_line::Tools::Text => Self::Text,
            command_line::Tools::Marker => Self::Marker,
            command_line::Tools::Blur => Self::Blur,
            command_line::Tools::Highlight => Self::Highlighter,
            command_line::Tools::Brush => Self::Brush,
        }
    }
}

#[cfg(test)]
mod resize_constraint_tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::new(Vec2D::new(x, y), Vec2D::new(w, h))
    }

    fn vec(x: f32, y: f32) -> Vec2D {
        Vec2D::new(x, y)
    }

    fn approx(a: Vec2D, b: Vec2D) {
        assert!(
            (a.x - b.x).abs() < 1e-4 && (a.y - b.y).abs() < 1e-4,
            "expected {:?}, got {:?}",
            b,
            a
        );
    }

    #[test]
    fn aspect_lock_grows_dominant_axis() {
        // 100×100 square, drag BottomRight from (100,100) to (200,110).
        // X dominates → both axes scale to 2.0, target → (200, 200).
        let r = rect(0.0, 0.0, 100.0, 100.0);
        let got = aspect_lock_corner_target(r, HandleId::BottomRight, vec(200.0, 110.0));
        approx(got, vec(200.0, 200.0));
    }

    #[test]
    fn aspect_lock_preserves_non_unit_ratio() {
        // 200×100 (2:1). BottomRight drag to (300, 110): scale_x=0.5,
        // scale_y=0.1 → scale=0.5 → new = (200 + 0.5*200, 100 + 0.5*100) = (300, 150).
        let r = rect(0.0, 0.0, 200.0, 100.0);
        let got = aspect_lock_corner_target(r, HandleId::BottomRight, vec(300.0, 110.0));
        approx(got, vec(300.0, 150.0));
    }

    #[test]
    fn aspect_lock_topleft_handles_negative_signs() {
        // 100×100 at (50, 50). TopLeft drag to (20, 0): anchor=BottomRight=(150,150).
        // dx=-130, dy=-150, scale_x=1.3, scale_y=1.5 → scale=1.5.
        // Result: (150 - 1.5*100, 150 - 1.5*100) = (0, 0).
        let r = rect(50.0, 50.0, 100.0, 100.0);
        let got = aspect_lock_corner_target(r, HandleId::TopLeft, vec(20.0, 0.0));
        approx(got, vec(0.0, 0.0));
    }

    #[test]
    fn aspect_lock_passthrough_for_non_corner() {
        let r = rect(0.0, 0.0, 100.0, 100.0);
        let p = vec(42.0, 17.0);
        assert_eq!(aspect_lock_corner_target(r, HandleId::Top, p), p);
        assert_eq!(aspect_lock_corner_target(r, HandleId::Start, p), p);
    }

    #[test]
    fn aspect_lock_passthrough_for_degenerate_rect() {
        let r = rect(0.0, 0.0, 0.0, 100.0);
        let p = vec(42.0, 17.0);
        assert_eq!(aspect_lock_corner_target(r, HandleId::BottomRight, p), p);
    }

    #[test]
    fn mirror_top_reflects_bottom_across_center() {
        // Bbox (0,0)-(100,100), center.y=50. Drag Top to y=30 (Δ=30) →
        // expect opposite Bottom at y=70 (br.y - Δ).
        let r = rect(0.0, 0.0, 100.0, 100.0);
        let (opp, target) = mirror_side_target(r, HandleId::Top, vec(0.0, 30.0)).unwrap();
        assert_eq!(opp, HandleId::Bottom);
        approx(target, vec(100.0, 70.0));
    }

    #[test]
    fn mirror_bottom_reflects_top_across_center() {
        // Drag Bottom from y=100 to y=80 (Δ=-20) → Top moves from 0 to 20.
        let r = rect(0.0, 0.0, 100.0, 100.0);
        let (opp, target) = mirror_side_target(r, HandleId::Bottom, vec(0.0, 80.0)).unwrap();
        assert_eq!(opp, HandleId::Top);
        approx(target, vec(0.0, 20.0));
    }

    #[test]
    fn mirror_left_reflects_right_across_center() {
        let r = rect(0.0, 0.0, 100.0, 100.0);
        let (opp, target) = mirror_side_target(r, HandleId::Left, vec(15.0, 0.0)).unwrap();
        assert_eq!(opp, HandleId::Right);
        approx(target, vec(85.0, 100.0));
    }

    #[test]
    fn mirror_right_reflects_left_across_center() {
        let r = rect(0.0, 0.0, 100.0, 100.0);
        let (opp, target) = mirror_side_target(r, HandleId::Right, vec(80.0, 0.0)).unwrap();
        assert_eq!(opp, HandleId::Left);
        approx(target, vec(20.0, 0.0));
    }

    #[test]
    fn mirror_returns_none_for_non_side() {
        let r = rect(0.0, 0.0, 100.0, 100.0);
        assert!(mirror_side_target(r, HandleId::TopLeft, vec(0.0, 0.0)).is_none());
        assert!(mirror_side_target(r, HandleId::Start, vec(0.0, 0.0)).is_none());
    }

    /// Sanity check that the combined operation (move dragged side, then
    /// move opposite to mirrored target) leaves the bbox symmetric about
    /// the original center. Mirrors what pointer.rs does for side+shift.
    #[test]
    fn side_pair_keeps_bbox_centered() {
        let orig = rect(0.0, 0.0, 100.0, 100.0);
        let target = vec(0.0, 30.0); // drag Top down by 30
        let after_first = bbox_resize(orig, HandleId::Top, target);
        let (opp, mirrored) = mirror_side_target(orig, HandleId::Top, target).unwrap();
        let after_second = bbox_resize(after_first, opp, mirrored);
        // Final: top=30, bottom=70, height=40 — symmetric about y=50.
        approx(after_second.top_left(), vec(0.0, 30.0));
        approx(after_second.bottom_right(), vec(100.0, 70.0));
    }
}
