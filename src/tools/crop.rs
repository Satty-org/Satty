use crate::{
    math::{self, Vec2D},
    sketch_board::{
        KeyEventMsg, MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput,
        SketchBoardOutput,
    },
    ui::toolbars::ToolbarEvent,
};
use anyhow::Result;
use femtovg::{Color, Paint, Path};
use relm4::{
    Sender,
    gtk::gdk::{Key, ModifierType},
};

use super::{Drawable, Tool, ToolUpdateResult, Tools};

#[derive(Debug, Clone)]
pub struct Crop {
    pos: Vec2D,
    size: Vec2D,
    /// True while the crop tool is the current editing focus
    /// — handles + grid + dim overlay are visible.
    active: bool,
    /// True after the user has pressed Enter to "apply" the crop.
    /// In this state the canvas zooms in to fit only the cropped
    /// region; switching back to the crop tool sets this back to
    /// false so the user can adjust against the full original
    /// image.
    committed: bool,
    /// Sticky — once Enter has been pressed at least once, this
    /// stays true even after re-entering edit mode. Lets Esc do
    /// the right thing: a fresh first-edit Esc deletes the crop
    /// (cancel), but Esc on an adjustment-of-already-committed
    /// crop restores the committed view (.
    ever_committed: bool,
    /// `(pos, size)` snapshot captured on each Enter-press. Read
    /// back in `handle_deactivated` to roll an un-committed re-entry
    /// edit back to the prior committed frame when the user leaves
    /// crop without re-pressing Enter (tool switch OR Esc both flow
    /// through deactivation). `None` until the first commit.
    last_committed: Option<(Vec2D, Vec2D)>,
}

pub struct CropTool {
    crop: Option<Crop>,
    action: Option<CropToolAction>,
    input_enabled: bool,
    sender: Option<Sender<SketchBoardInput>>,
    /// Snap crop edges to image edges during drag. Toggled from the
    /// bottom-left checkbox , persisted via
    /// `state::save_snap_to_edges`. Defaults to true. Holding Ctrl
    /// during a drag temporarily bypasses snap regardless of this
    /// flag — matching the standard "Hold ⌘ to disable snapping".
    snap_to_edges: bool,
    /// Image dimensions in image-space pixels. Set once at app
    /// startup; snap targets are derived from this (image edges +
    /// the four "edge" lines bounding it). `None` while the tool
    /// hasn't been told the dimensions yet — snap is a no-op then.
    image_bounds: Option<Vec2D>,
    /// Constrain the crop rectangle's width-to-height ratio while
    /// the user is dragging handles. `Freeform` lets the rectangle
    /// take any shape (legacy behavior); the other variants project
    /// each drag onto the nearest rectangle matching the configured
    /// ratio. Switching the ratio also snaps the *current* rect
    /// (inscribed, centered) so the visible overlay always matches
    /// the selected ratio.
    aspect_ratio: AspectRatio,
}

/// Aspect-ratio constraint applied to crop drags. The variants
/// mirror typical dropdown options minus the user-typed
/// "Custom Ratio" entry (that one would need a sub-dialog and is
/// left for a follow-up).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AspectRatio {
    /// No constraint — drag freely (default).
    #[default]
    Freeform,
    /// Match the source image's W:H — useful when you want a
    /// scaled-down copy of the full screenshot.
    Original,
    /// 1 : 1 (square).
    Square,
    /// 5 : 4 (matches "10 : 8").
    FiveFour,
    /// 7 : 5.
    SevenFive,
    /// 4 : 3.
    FourThree,
    /// 3 : 2 (matches "6 : 4").
    ThreeTwo,
    /// 16 : 9.
    SixteenNine,
}

impl AspectRatio {
    /// Returns `Some((w, h))` as a pair of components defining the
    /// constraint, or `None` for `Freeform`. For `Original`, the
    /// caller supplies the image bounds — since `Self` is `Copy`
    /// and can't carry the bounds with it, the lookup happens at
    /// enforcement time.
    pub fn ratio_components(self, image_bounds: Option<Vec2D>) -> Option<(f32, f32)> {
        match self {
            AspectRatio::Freeform => None,
            AspectRatio::Original => image_bounds.map(|b| (b.x.abs(), b.y.abs())),
            AspectRatio::Square => Some((1.0, 1.0)),
            AspectRatio::FiveFour => Some((5.0, 4.0)),
            AspectRatio::SevenFive => Some((7.0, 5.0)),
            AspectRatio::FourThree => Some((4.0, 3.0)),
            AspectRatio::ThreeTwo => Some((3.0, 2.0)),
            AspectRatio::SixteenNine => Some((16.0, 9.0)),
        }
    }

    /// Mapping to / from the dropdown index used in the top toolbar.
    /// Keep this in sync with the labels array in
    /// `ui::toolbars` (the dropdown is built from
    /// `ALL_LABELS`). Layout: Freeform first so a fresh launch
    /// keeps the legacy "any shape" behavior.
    pub const ALL: &'static [AspectRatio] = &[
        AspectRatio::Freeform,
        AspectRatio::Original,
        AspectRatio::Square,
        AspectRatio::FiveFour,
        AspectRatio::SevenFive,
        AspectRatio::FourThree,
        AspectRatio::ThreeTwo,
        AspectRatio::SixteenNine,
    ];

    pub const ALL_LABELS: &'static [&'static str] = &[
        "Freeform",
        "Original Ratio",
        "1 : 1 (Square)",
        "5 : 4 (10 : 8)",
        "7 : 5",
        "4 : 3",
        "3 : 2 (6 : 4)",
        "16 : 9",
    ];

    pub fn from_index(i: usize) -> Self {
        Self::ALL.get(i).copied().unwrap_or_default()
    }

    pub fn to_index(self) -> usize {
        Self::ALL.iter().position(|r| *r == self).unwrap_or(0)
    }
}

impl Default for CropTool {
    fn default() -> Self {
        Self {
            crop: None,
            action: None,
            input_enabled: false,
            sender: None,
            snap_to_edges: true,
            image_bounds: None,
            aspect_ratio: AspectRatio::Freeform,
        }
    }
}

impl Crop {
    /// Visual size of corner L-brackets and edge handle marks, in CSS
    /// pixels — divided by the canvas-to-image scale at draw time so
    /// the on-screen size stays constant regardless of zoom.
    const BRACKET_LENGTH: f32 = 28.0;
    /// Edge handles are drawn as fat parallel segments overlapping the
    /// edge line itself, so they need to be longer than the old
    /// perpendicular ticks to read as a "drag bar". A short third of
    /// the crop's edge length is the natural read, but at a fixed
    /// CSS-pixel size for predictability across zoom levels.
    const EDGE_HANDLE_LENGTH: f32 = 36.0;
    /// Stroke thickness for both the corner L-brackets and the edge
    /// bars. The edge bars overlay the 2px dark crop border, so they
    /// need a few extra pixels of white on each side to read as a
    /// solid bar (instead of a thin halo around the border). Bumping
    /// the corners by the same amount keeps the two handle styles
    /// visually matched.
    const HANDLE_STROKE_WIDTH: f32 = 5.0;
    /// Grid lines separating the crop area into thirds (rule-of-thirds).
    const GRID_STROKE_WIDTH: f32 = 1.0;
    /// Hit-test radius around each corner / edge-midpoint anchor, in
    /// CSS pixels. Big enough that grabbing anywhere along a bracket
    /// arm or near an edge handle lands the right handle without
    /// pixel-precise aim. Scales with the visual size bump above.
    const HANDLE_HIT_RADIUS: f32 = 20.0;

    fn new(pos: Vec2D) -> Self {
        Self {
            pos,
            size: Vec2D::zero(),
            active: true,
            committed: false,
            ever_committed: false,
            last_committed: None,
        }
    }

    pub fn is_committed(&self) -> bool {
        self.committed
    }

    fn handle_paint(scale: f32) -> Paint {
        // White strokes, slightly hot, with a subtle dark drop is
        // overkill — femtovg doesn't do shadows cheaply. The white
        // stroke alone reads cleanly against the dark overlay used
        // outside the crop area.
        Paint::color(Color::rgbf(1.0, 1.0, 1.0))
            .with_line_width(Self::HANDLE_STROKE_WIDTH / scale)
            .with_line_cap(femtovg::LineCap::Square)
            .with_line_join(femtovg::LineJoin::Miter)
    }

    /// Draw the L-bracket at one corner. `dx`/`dy` are ±1 indicating
    /// which direction the bracket arms extend from the corner point.
    fn draw_corner_bracket(
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        corner: Vec2D,
        dx: f32,
        dy: f32,
        scale: f32,
        paint: &Paint,
    ) {
        let len = Self::BRACKET_LENGTH / scale;
        let mut path = Path::new();
        path.move_to(corner.x + dx * len, corner.y);
        path.line_to(corner.x, corner.y);
        path.line_to(corner.x, corner.y + dy * len);
        canvas.stroke_path(&path, paint);
    }

    /// Draw the edge-midpoint handle as a fat segment lying ALONG the
    /// edge (parallel to it, centered on the midpoint). The thicker
    /// stroke + parallel orientation visually overlay the crop border
    /// line, signaling "grab this and drag the edge." Replaces the
    /// older perpendicular-tick design which read as a divider mark
    /// instead of a draggable bar.
    ///
    /// `edge_dir` is a unit vector pointing along the edge — the
    /// segment is drawn from `midpoint - edge_dir * half` to
    /// `midpoint + edge_dir * half`.
    fn draw_edge_handle(
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        midpoint: Vec2D,
        edge_dir: Vec2D,
        scale: f32,
        paint: &Paint,
    ) {
        let half = (Self::EDGE_HANDLE_LENGTH / 2.0) / scale;
        let mut path = Path::new();
        path.move_to(
            midpoint.x - edge_dir.x * half,
            midpoint.y - edge_dir.y * half,
        );
        path.line_to(
            midpoint.x + edge_dir.x * half,
            midpoint.y + edge_dir.y * half,
        );
        canvas.stroke_path(&path, paint);
    }

    /// Draw the rule-of-thirds grid lines inside the crop rect.
    /// Subtle white at low opacity so the grid hints at composition
    /// without dominating the framed content.
    fn draw_thirds_grid(
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        pos: Vec2D,
        size: Vec2D,
        scale: f32,
    ) {
        let paint = Paint::color(Color::rgbaf(1.0, 1.0, 1.0, 0.35))
            .with_line_width(Self::GRID_STROKE_WIDTH / scale);
        let mut path = Path::new();
        let third_x = size.x / 3.0;
        let third_y = size.y / 3.0;
        // Two vertical lines
        path.move_to(pos.x + third_x, pos.y);
        path.line_to(pos.x + third_x, pos.y + size.y);
        path.move_to(pos.x + 2.0 * third_x, pos.y);
        path.line_to(pos.x + 2.0 * third_x, pos.y + size.y);
        // Two horizontal lines
        path.move_to(pos.x, pos.y + third_y);
        path.line_to(pos.x + size.x, pos.y + third_y);
        path.move_to(pos.x, pos.y + 2.0 * third_y);
        path.line_to(pos.x + size.x, pos.y + 2.0 * third_y);
        canvas.stroke_path(&path, &paint);
    }

    pub fn get_rectangle(&self) -> (Vec2D, Vec2D) {
        math::rect_ensure_positive_size(self.pos, self.size)
    }

    fn get_handle_pos(crop_pos: Vec2D, crop_size: Vec2D, handle: CropHandle) -> Vec2D {
        match handle {
            CropHandle::TopLeftCorner => crop_pos,
            CropHandle::TopEdge => crop_pos + Vec2D::new(crop_size.x / 2.0, 0.0),
            CropHandle::TopRightCorner => crop_pos + Vec2D::new(crop_size.x, 0.0),
            CropHandle::RightEdge => crop_pos + Vec2D::new(crop_size.x, crop_size.y / 2.0),
            CropHandle::BottomRightCorner => crop_pos + Vec2D::new(crop_size.x, crop_size.y),
            CropHandle::BottomEdge => crop_pos + Vec2D::new(crop_size.x / 2.0, crop_size.y),
            CropHandle::BottomLeftCorner => crop_pos + Vec2D::new(0.0, crop_size.y),
            CropHandle::LeftEdge => crop_pos + Vec2D::new(0.0, crop_size.y / 2.0),
        }
    }
    fn get_closest_handle(&self, mouse_pos: Vec2D) -> (CropHandle, f32) {
        let mut min_distance_squared = f32::MAX;
        let mut closest_handle = CropHandle::TopLeftCorner;
        for h in CropHandle::all() {
            let handle_pos = Self::get_handle_pos(self.pos, self.size, h);
            let distance_squared = (handle_pos - mouse_pos).norm2();
            if distance_squared < min_distance_squared {
                min_distance_squared = distance_squared;
                closest_handle = h;
            }
        }
        (closest_handle, min_distance_squared)
    }
    fn test_handle_hit(&self, mouse_pos: Vec2D, margin2: f32) -> Option<CropHandle> {
        const HANDLE_HIT2: f32 = Crop::HANDLE_HIT_RADIUS * Crop::HANDLE_HIT_RADIUS;
        let allowed_distance2 = HANDLE_HIT2 + margin2;

        let (handle, distance2) = self.get_closest_handle(mouse_pos);
        if distance2 < allowed_distance2 {
            Some(handle)
        } else {
            None
        }
    }

    /// Hit-test classification used by the hover-cursor logic. Reports
    /// `Handle` when the pointer is over any of the 8 corner / edge
    /// anchors, `Body` when inside the crop rectangle, and `None` for
    /// the surrounding dim region. Returns `None` while the crop
    /// hasn't been drawn yet (zero size) so an unset crop doesn't
    /// flip the cursor under the user.
    ///
    /// `image_to_canvas_scale` is the renderer's image→canvas multiplier;
    /// we use it to keep the handle hit area at a constant CSS-pixel
    /// size on screen instead of a constant image-pixel radius (the
    /// latter shrinks visibly when an over-sized screenshot gets
    /// auto-fit-scaled down, leaving the visible bracket but no hit
    /// zone). Pass 1.0 if you don't have a useful scale yet.
    pub fn hit_kind(&self, point: Vec2D, image_to_canvas_scale: f32) -> Option<CropHit> {
        if self.size.x.abs() < 1.0 || self.size.y.abs() < 1.0 {
            return None;
        }
        // HANDLE_HIT_RADIUS is in CSS pixels; divide by scale to get
        // an equivalent radius in image-space units (where `point` and
        // handle positions are expressed).
        let scale = image_to_canvas_scale.max(0.0001);
        let radius_image = Self::HANDLE_HIT_RADIUS / scale;
        let radius2 = radius_image * radius_image;
        let (handle, distance2) = self.get_closest_handle(point);
        if distance2 < radius2 {
            return Some(CropHit::Handle(handle));
        }
        let (pos, size) = self.get_rectangle();
        if point.x >= pos.x
            && point.x <= pos.x + size.x
            && point.y >= pos.y
            && point.y <= pos.y + size.y
        {
            return Some(CropHit::Body);
        }
        None
    }
}

/// Where on the crop overlay the pointer currently is. The `Handle`
/// variant carries WHICH handle is under the cursor so sketch_board's
/// hover-cursor logic can show the matching directional resize cursor
/// instead of a generic pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CropHit {
    Handle(CropHandle),
    Body,
}

impl Drawable for Crop {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: femtovg::FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        // Skip drawing the overlay unless the crop tool is the
        // current focus. Two paths get us here:
        //   - committed: renderer zooms into the crop rect; our dim
        //     would overlay the zoomed image or hang off-canvas.
        //   - !active: another tool is current. The dim/border are
        //     part of the crop tool's UI and shouldn't linger on the
        //     canvas while the user is doing something else (e.g.
        //     a first-time draft that hasn't been committed yet —
        //     state is preserved for re-entry, just hidden).
        if self.committed || !self.active {
            return Ok(());
        }

        let size = self.size;
        let saved_transform = canvas.transform();
        let scale = saved_transform.average_scale();

        // Crop rect in CANVAS-PIXEL space. Earlier this was drawn in
        // image-space as `(0,0)→(canvas_w/scale, canvas_h/scale)`,
        // which doesn't account for the renderer's centering offset:
        // the dim rect slid right/down by that offset, leaving the
        // top/left of the canvas uncovered and bleeding past the
        // bottom/right edges. Computing the crop rect in canvas pixels
        // and resetting the transform for the dim fill lets us anchor
        // the outer dim to the literal canvas (0,0)→(canvas_w,canvas_h)
        // rectangle, which is what the user can actually see.
        let crop_canvas_x = saved_transform[0] * self.pos.x
            + saved_transform[2] * self.pos.y
            + saved_transform[4];
        let crop_canvas_y = saved_transform[1] * self.pos.x
            + saved_transform[3] * self.pos.y
            + saved_transform[5];
        let crop_canvas_w = size.x * scale;
        let crop_canvas_h = size.y * scale;

        let shadow_paint = Paint::color(Color::rgbaf(0.0, 0.0, 0.0, 0.5))
            .with_fill_rule(femtovg::FillRule::EvenOdd);
        let mut shadow_path = Path::new();
        shadow_path.rect(0.0, 0.0, canvas.width() as f32, canvas.height() as f32);
        shadow_path.rect(crop_canvas_x, crop_canvas_y, crop_canvas_w, crop_canvas_h);

        canvas.save();
        canvas.reset_transform();
        canvas.fill_path(&shadow_path, &shadow_paint);
        canvas.reset_transform();
        canvas.set_transform(&saved_transform);

        let border_paint = Paint::color(Color::rgbf(0.1, 0.1, 0.1)).with_line_width(2.0);
        let mut border_path = Path::new();
        border_path.rect(self.pos.x, self.pos.y, size.x, size.y);

        canvas.stroke_path(&border_path, &border_paint);

        // Rule-of-thirds grid sits below the brackets so the
        // stronger white outlines stay on top.
        Self::draw_thirds_grid(canvas, self.pos, size, scale);

        let paint = Self::handle_paint(scale);
        // Corners — L-brackets pointing inward from each corner.
        // For each corner, dx/dy are ±1 indicating which axis
        // the arms extend along (always toward the rect interior).
        Self::draw_corner_bracket(canvas, self.pos, 1.0, 1.0, scale, &paint);
        Self::draw_corner_bracket(
            canvas,
            self.pos + Vec2D::new(size.x, 0.0),
            -1.0,
            1.0,
            scale,
            &paint,
        );
        Self::draw_corner_bracket(
            canvas,
            self.pos + Vec2D::new(0.0, size.y),
            1.0,
            -1.0,
            scale,
            &paint,
        );
        Self::draw_corner_bracket(
            canvas,
            self.pos + size,
            -1.0,
            -1.0,
            scale,
            &paint,
        );

        // Edge midpoints — fat segments lying ALONG each edge so
        // they overlay the border line and read as a draggable
        // bar. Top + bottom edges run horizontally, so the handle
        // direction is (1,0); left + right edges run vertically,
        // so the handle direction is (0,1).
        Self::draw_edge_handle(
            canvas,
            self.pos + Vec2D::new(size.x / 2.0, 0.0),
            Vec2D::new(1.0, 0.0),
            scale,
            &paint,
        );
        Self::draw_edge_handle(
            canvas,
            self.pos + Vec2D::new(size.x / 2.0, size.y),
            Vec2D::new(1.0, 0.0),
            scale,
            &paint,
        );
        Self::draw_edge_handle(
            canvas,
            self.pos + Vec2D::new(0.0, size.y / 2.0),
            Vec2D::new(0.0, 1.0),
            scale,
            &paint,
        );
        Self::draw_edge_handle(
            canvas,
            self.pos + Vec2D::new(size.x, size.y / 2.0),
            Vec2D::new(0.0, 1.0),
            scale,
            &paint,
        );

        canvas.restore();
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CropHandle {
    TopLeftCorner,
    TopEdge,
    TopRightCorner,
    RightEdge,
    BottomRightCorner,
    BottomEdge,
    BottomLeftCorner,
    LeftEdge,
}

impl CropHandle {
    /// CSS cursor name for hovering this handle — corner handles get
    /// the diagonal double-arrow, edges get the cardinal one.
    pub fn resize_cursor(self) -> &'static str {
        use CropHandle::*;
        match self {
            TopLeftCorner | BottomRightCorner => "nwse-resize",
            TopRightCorner | BottomLeftCorner => "nesw-resize",
            TopEdge | BottomEdge => "ns-resize",
            LeftEdge | RightEdge => "ew-resize",
        }
    }
}

enum CropToolAction {
    NewCrop,
    DragHandle(DragHandleState),
    Move(MoveState),
}

struct DragHandleState {
    handle: CropHandle,
    top_left_start: Vec2D,
    bottom_right_start: Vec2D,
}

struct MoveState {
    start: Vec2D,
}

impl CropTool {
    pub fn get_crop(&self) -> Option<&Crop> {
        match &self.crop {
            Some(c) => Some(c),
            None => None,
        }
    }

    /// Bounds of the committed crop region in image coordinates,
    /// canonicalized to a positive-size rectangle. Returns `None`
    /// when there's no crop or when the crop isn't committed (i.e.,
    /// the user is still editing it). The renderer reads this to
    /// decide whether to apply zoom-fit transformation.
    pub fn get_committed_rect(&self) -> Option<(Vec2D, Vec2D)> {
        let crop = self.crop.as_ref()?;
        if !crop.committed {
            return None;
        }
        let (pos, size) = crop.get_rectangle();
        if size.x <= 0.0 || size.y <= 0.0 {
            return None;
        }
        Some((pos, size))
    }

    /// Drop the crop entirely — used by the toolbar's "Revert to
    /// Original" button when the user ISN'T currently in the Crop
    /// tool. After this, the renderer renders the full image at
    /// normal scale and saving exports the entire image again.
    pub fn revert(&mut self) {
        self.crop = None;
        self.action = None;
        if let Some(sender) = &self.sender {
            sender
                .send(SketchBoardInput::Output(
                    SketchBoardOutput::DimensionsUpdate(None),
                ))
                .ok();
        }
        self.emit_crop_presence(false);
        // Reverting returns the canvas to the full image — let main.rs
        // resize the window back to fit it.
        if let Some(bounds) = self.image_bounds {
            self.emit_content_size(bounds.x, bounds.y);
        }
    }

    /// "Revert to Original" while still inside the Crop tool: instead
    /// of dropping the crop and stranding the user with a bare image,
    /// reset to the fresh-entry seed (full-image bracket with handles
    /// ready to drag inward). Same visual state as `handle_activated`'s
    /// first-time seed path. Falls back to `revert()` if we somehow
    /// don't know the image dimensions yet.
    pub fn revert_to_seed(&mut self) {
        let Some(bounds) = self.image_bounds else {
            self.revert();
            return;
        };
        self.crop = Some(Crop {
            pos: Vec2D::zero(),
            size: bounds,
            active: true,
            committed: false,
            ever_committed: false,
            last_committed: None,
        });
        self.action = None;
        if let Some(sender) = &self.sender {
            sender
                .send(SketchBoardInput::Output(
                    SketchBoardOutput::DimensionsUpdate(None),
                ))
                .ok();
        }
        // Crop is still present (just reset to the seed) — keep the
        // Revert button visible. The window is already at full-image
        // size while in Crop, but emit anyway so a degenerate
        // out-of-sync state recovers.
        self.emit_crop_presence(true);
        self.emit_content_size(bounds.x, bounds.y);
    }

    /// Toolbar "Cancel" button (or Esc): exit Crop without applying
    /// any pending changes. First-time drafts are dropped entirely;
    /// re-entered crops with a prior commit are restored to that
    /// commit by `handle_deactivated` once we exit the tool.
    pub fn cancel(&mut self) -> ToolUpdateResult {
        let mut cleared = false;
        if let Some(crop) = &mut self.crop
            && !crop.ever_committed
        {
            crop.active = false;
            self.crop = None;
            cleared = true;
        }
        self.action = None;
        if cleared {
            self.emit_crop_presence(false);
        }
        if let Some(sender) = &self.sender {
            sender.send(SketchBoardInput::ExitCropToPreviousTool).ok();
        }
        ToolUpdateResult::Redraw
    }

    /// Toolbar "Crop" button (or Enter): apply the in-progress crop
    /// and exit the tool. No-op if there's no active crop edit.
    pub fn commit(&mut self) -> ToolUpdateResult {
        let Some(crop) = self.crop.as_mut() else {
            return ToolUpdateResult::Unmodified;
        };
        if !crop.active {
            return ToolUpdateResult::Unmodified;
        }
        // Canonicalize, snapshot, mark committed so the renderer
        // switches to zoomed-in view. `ever_committed` sticks so a
        // future re-entry's exit-without-Enter reverts to this rect.
        let (pos, size) = crop.get_rectangle();
        crop.pos = pos;
        crop.size = size;
        crop.last_committed = Some((pos, size));
        crop.committed = true;
        crop.ever_committed = true;
        crop.active = false;
        self.action = None;
        self.emit_content_size(size.x, size.y);
        // Hand the user back to the main view — emit a tool switch
        // so the StyleToolbar reappears and the crop bottom bar
        // collapses. Pointer is a neutral default; the user can
        // pick whichever tool next via the top toolbar.
        if let Some(sender) = &self.sender {
            sender
                .send(SketchBoardInput::ToolbarEvent(
                    ToolbarEvent::ToolSelected(Tools::Pointer),
                ))
                .ok();
        }
        ToolUpdateResult::Redraw
    }

    /// Toggle whether crop edges snap to image edges during drag.
    /// Wired from the toolbar checkbox; persists via state.
    pub fn set_snap_to_edges(&mut self, value: bool) {
        self.snap_to_edges = value;
    }

    pub fn snap_to_edges(&self) -> bool {
        self.snap_to_edges
    }

    /// Provide the image dimensions so snap-to-edges has targets to
    /// snap to. Called once from `sketch_board::init` with the
    /// loaded screenshot's pixel dimensions.
    pub fn set_image_bounds(&mut self, bounds: Vec2D) {
        self.image_bounds = Some(bounds);
    }

    /// Threshold within which an edge "sticks" to the image boundary,
    /// in image-space pixels. Stays in image units because all snap
    /// math is in image-space; we don't try to compensate for zoom
    /// (a tighter pixel threshold at high zoom is acceptable since
    /// the user is also more precise then).
    const SNAP_PIXELS: f32 = 8.0;

    fn snap_active(&self, modifier: ModifierType) -> bool {
        // Mirror typical "snap on, hold the modifier to defeat"
        // semantic. Ctrl is the natural Linux equivalent of macOS Cmd
        // (and our other tools already treat Shift as "snap-to-angle"
        // so reusing Shift here would be a conflict).
        self.snap_to_edges && !modifier.contains(ModifierType::CONTROL_MASK)
    }
}

impl CropHandle {
    fn all() -> [CropHandle; 8] {
        [
            CropHandle::TopLeftCorner,
            CropHandle::TopEdge,
            CropHandle::TopRightCorner,
            CropHandle::RightEdge,
            CropHandle::BottomRightCorner,
            CropHandle::BottomEdge,
            CropHandle::BottomLeftCorner,
            CropHandle::LeftEdge,
        ]
    }
}

impl CropTool {
    const HANDLE_MARGIN_IN_2: f32 = 15.0 * 15.0;
    const HANDLE_MARGIN_OUT: f32 = 40.0;

    fn test_inside_crop(&self, mouse_pos: Vec2D, margin: f32) -> bool {
        let crop = match &self.crop {
            Some(c) => c,
            None => return false,
        };

        let (mut min_x, mut max_x) = (crop.pos.x, crop.pos.x + crop.size.x);
        if min_x > max_x {
            (min_x, max_x) = (max_x, min_x);
        }
        min_x -= margin;
        max_x += margin;

        let (mut min_y, mut max_y) = (crop.pos.y, crop.pos.y + crop.size.y);
        if min_y > max_y {
            (min_y, max_y) = (max_y, min_y);
        }
        min_y -= margin;
        max_y += margin;

        min_x < mouse_pos.x && mouse_pos.x < max_x && min_y < mouse_pos.y && mouse_pos.y < max_y
    }

    fn apply_drag_handle_transformation(
        crop: &mut Crop,
        state: &DragHandleState,
        direction: Vec2D,
        aspect: Option<(f32, f32)>,
        snap_x: impl Fn(f32) -> f32,
        snap_y: impl Fn(f32) -> f32,
    ) {
        let tl0 = state.top_left_start;
        let br0 = state.bottom_right_start;
        let mut tl = tl0;
        let mut br = br0;

        // Apply the per-handle transformation, then snap each dragged
        // coordinate through the caller's snap closures. Handles that
        // only move along one axis only snap that axis — e.g. the
        // top edge doesn't try to snap left/right.
        match state.handle {
            CropHandle::TopLeftCorner => {
                tl.x = snap_x(tl0.x + direction.x);
                tl.y = snap_y(tl0.y + direction.y);
            }
            CropHandle::TopEdge => {
                tl.y = snap_y(tl0.y + direction.y);
            }
            CropHandle::TopRightCorner => {
                tl.y = snap_y(tl0.y + direction.y);
                br.x = snap_x(br0.x + direction.x);
            }
            CropHandle::RightEdge => {
                br.x = snap_x(br0.x + direction.x);
            }
            CropHandle::BottomRightCorner => {
                br.x = snap_x(br0.x + direction.x);
                br.y = snap_y(br0.y + direction.y);
            }
            CropHandle::BottomEdge => {
                br.y = snap_y(br0.y + direction.y);
            }
            CropHandle::BottomLeftCorner => {
                tl.x = snap_x(tl0.x + direction.x);
                br.y = snap_y(br0.y + direction.y);
            }
            CropHandle::LeftEdge => {
                tl.x = snap_x(tl0.x + direction.x);
            }
        }

        // Aspect-ratio enforcement: project the (possibly snapped) rect
        // onto the constrained-ratio shape, anchored to the corner /
        // edge midpoint opposite the one the user is dragging. Edges
        // grow the perpendicular dimension symmetrically (centered on
        // the anchor's midpoint); corners use the dominant drag axis
        // (whichever produces the bigger box) and recompute the other.
        if let Some((rw, rh)) = aspect
            && rh > 0.0
        {
            let r = rw / rh; // target width / height

            // The anchor is the point that DIDN'T move. For corners,
            // it's the opposite corner; for edges, the midpoint of
            // the opposite edge.
            let anchor = match state.handle {
                CropHandle::TopLeftCorner => br0,
                CropHandle::TopRightCorner => Vec2D::new(tl0.x, br0.y),
                CropHandle::BottomLeftCorner => Vec2D::new(br0.x, tl0.y),
                CropHandle::BottomRightCorner => tl0,
                CropHandle::TopEdge => Vec2D::new((tl0.x + br0.x) / 2.0, br0.y),
                CropHandle::BottomEdge => Vec2D::new((tl0.x + br0.x) / 2.0, tl0.y),
                CropHandle::LeftEdge => Vec2D::new(br0.x, (tl0.y + br0.y) / 2.0),
                CropHandle::RightEdge => Vec2D::new(tl0.x, (tl0.y + br0.y) / 2.0),
            };

            let cur_w = (br.x - tl.x).abs();
            let cur_h = (br.y - tl.y).abs();

            let (final_w, final_h) = match state.handle {
                CropHandle::TopLeftCorner
                | CropHandle::TopRightCorner
                | CropHandle::BottomLeftCorner
                | CropHandle::BottomRightCorner => {
                    // Corner: pick the dimension that produces the
                    // larger ratio-matched rect so the dragged corner
                    // tracks the user's pointer along its dominant
                    // axis. Result: dragging "out" never shrinks the
                    // rect, dragging "in" never grows it.
                    if cur_w / r >= cur_h {
                        (cur_w, cur_w / r)
                    } else {
                        (cur_h * r, cur_h)
                    }
                }
                CropHandle::TopEdge | CropHandle::BottomEdge => {
                    // Edge: height changed; width follows from ratio,
                    // centered horizontally on the anchor.
                    (cur_h * r, cur_h)
                }
                CropHandle::LeftEdge | CropHandle::RightEdge => {
                    // Edge: width changed; height follows from ratio,
                    // centered vertically on the anchor.
                    (cur_w, cur_w / r)
                }
            };

            // Place the rectangle relative to the anchor — `sign_*`
            // says which side of the anchor the rect extends along
            // each axis. 0 means "centered on anchor" (edge drags
            // where the parallel axis is symmetric).
            let sign_x = match state.handle {
                CropHandle::TopLeftCorner
                | CropHandle::BottomLeftCorner
                | CropHandle::LeftEdge => -1.0,
                CropHandle::TopRightCorner
                | CropHandle::BottomRightCorner
                | CropHandle::RightEdge => 1.0,
                CropHandle::TopEdge | CropHandle::BottomEdge => 0.0,
            };
            let sign_y = match state.handle {
                CropHandle::TopLeftCorner
                | CropHandle::TopRightCorner
                | CropHandle::TopEdge => -1.0,
                CropHandle::BottomLeftCorner
                | CropHandle::BottomRightCorner
                | CropHandle::BottomEdge => 1.0,
                CropHandle::LeftEdge | CropHandle::RightEdge => 0.0,
            };

            if sign_x > 0.0 {
                tl.x = anchor.x;
                br.x = anchor.x + final_w;
            } else if sign_x < 0.0 {
                br.x = anchor.x;
                tl.x = anchor.x - final_w;
            } else {
                tl.x = anchor.x - final_w / 2.0;
                br.x = anchor.x + final_w / 2.0;
            }
            if sign_y > 0.0 {
                tl.y = anchor.y;
                br.y = anchor.y + final_h;
            } else if sign_y < 0.0 {
                br.y = anchor.y;
                tl.y = anchor.y - final_h;
            } else {
                tl.y = anchor.y - final_h / 2.0;
                br.y = anchor.y + final_h / 2.0;
            }
        }

        // convert back and save
        crop.pos = tl;
        crop.size = br - tl;
    }

    /// Set the active aspect-ratio constraint and snap the existing
    /// crop rect (if any) to that ratio — inscribed in the current
    /// rect, centered. Future drags apply the constraint via
    /// `apply_drag_handle_transformation`'s aspect branch.
    pub fn set_aspect_ratio(&mut self, ratio: AspectRatio) {
        self.aspect_ratio = ratio;
        let Some(crop) = self.crop.as_mut() else {
            return;
        };
        let Some((rw, rh)) = ratio.ratio_components(self.image_bounds) else {
            return; // Freeform — no snap.
        };
        let r = rw / rh;
        let (cur_pos, cur_size) = crop.get_rectangle();
        if cur_size.x <= 0.0 || cur_size.y <= 0.0 || r <= 0.0 {
            return;
        }
        // Inscribe: shrink whichever dimension is too big so the
        // rect fits the ratio inside the current bounds, centered on
        // the current rect's midpoint.
        let center_x = cur_pos.x + cur_size.x / 2.0;
        let center_y = cur_pos.y + cur_size.y / 2.0;
        let (new_w, new_h) = if cur_size.x / r > cur_size.y {
            (cur_size.y * r, cur_size.y)
        } else {
            (cur_size.x, cur_size.x / r)
        };
        crop.pos = Vec2D::new(center_x - new_w / 2.0, center_y - new_h / 2.0);
        crop.size = Vec2D::new(new_w, new_h);
        self.emit_crop_dimensions_update();
    }

    pub fn aspect_ratio(&self) -> AspectRatio {
        self.aspect_ratio
    }

    fn emit_crop_dimensions_update(&self) {
        if let (Some(crop), Some(sender)) = (&self.crop, &self.sender) {
            let (_pos, size) = crop.get_rectangle();
            let width = size.x.round() as i32;
            let height = size.y.round() as i32;
            sender
                .send(SketchBoardInput::Output(
                    SketchBoardOutput::DimensionsUpdate(Some((width, height))),
                ))
                .ok();
        }
    }

    /// Push the current crop-presence state out so the bottom toolbar
    /// shows/hides the "Revert to Original" button. Crop state is
    /// "present" when there's any crop at all (edit OR committed) —
    /// the user gets one button regardless of mode.
    fn emit_crop_presence(&self, present: bool) {
        if let Some(sender) = &self.sender {
            sender
                .send(SketchBoardInput::Output(
                    SketchBoardOutput::CropPresenceChanged(present),
                ))
                .ok();
        }
    }

    /// Notify the rest of the app that the size of whatever's
    /// rendered on the canvas just changed — used by main.rs to
    /// resize the window around the new content (committed crop,
    /// full image after re-enter, or full image after revert).
    fn emit_content_size(&self, width: f32, height: f32) {
        if let Some(sender) = &self.sender {
            sender
                .send(SketchBoardInput::Output(
                    SketchBoardOutput::ContentSizeChanged { width, height },
                ))
                .ok();
        }
    }

    fn begin_drag(&mut self, pos: Vec2D, _modifier: ModifierType) -> ToolUpdateResult {
        let mut activate = false;
        let was_present = self.crop.is_some();
        match &self.crop {
            None => {
                // No crop exists, create a new one
                self.crop = Some(Crop::new(pos));
                self.action = Some(CropToolAction::NewCrop);
            }
            Some(c) => {
                if !c.active {
                    activate = true;
                }
                if let Some(handle) = c.test_handle_hit(pos, CropTool::HANDLE_MARGIN_IN_2) {
                    // Crop exists and we are near a handle, drag it
                    self.action = Some(CropToolAction::DragHandle(DragHandleState {
                        handle,
                        top_left_start: c.pos,
                        bottom_right_start: c.pos + c.size,
                    }));
                } else if self.test_inside_crop(pos, 0.0) {
                    // Crop exists and we are inside it, move it
                    self.action = Some(CropToolAction::Move(MoveState { start: c.pos }));
                } else if self.test_inside_crop(pos, CropTool::HANDLE_MARGIN_OUT) {
                    // Crop exists and we are near the edge, drag from the closest handle
                    let (handle, _) = c.get_closest_handle(pos);
                    self.action = Some(CropToolAction::DragHandle(DragHandleState {
                        handle,
                        top_left_start: c.pos,
                        bottom_right_start: c.pos + c.size,
                    }));
                } else {
                    // Crop exists, but we far outside from it, create a new one
                    self.crop = Some(Crop::new(pos));
                    self.action = Some(CropToolAction::NewCrop);
                }
            }
        }
        if activate && let Some(c) = &mut self.crop {
            c.active = true;
        }
        // First-time crop creation needs to surface to the toolbar so
        // the Revert button appears immediately. Subsequent drags on
        // an existing crop don't change presence and skip the emit.
        if !was_present && self.crop.is_some() {
            self.emit_crop_presence(true);
        }
        ToolUpdateResult::Redraw
    }

    fn update_drag(&mut self, direction: Vec2D, modifier: ModifierType) -> ToolUpdateResult {
        // Build cheap snap closures once and pass them down. Capturing
        // `snap_active` and `bounds` by value avoids `&mut self` /
        // `&self.action` overlap during the inner match — the closures
        // are plain `Fn(f32) -> f32` and don't touch any field after
        // construction.
        let snap_active = self.snap_active(modifier);
        let bounds = self.image_bounds;
        let snap_x = move |v: f32| -> f32 {
            if !snap_active {
                return v;
            }
            let Some(b) = bounds else {
                return v;
            };
            for t in [0.0, b.x] {
                if (v - t).abs() <= Self::SNAP_PIXELS {
                    return t;
                }
            }
            v
        };
        let snap_y = move |v: f32| -> f32 {
            if !snap_active {
                return v;
            }
            let Some(b) = bounds else {
                return v;
            };
            for t in [0.0, b.y] {
                if (v - t).abs() <= Self::SNAP_PIXELS {
                    return t;
                }
            }
            v
        };

        // Materialize the aspect-ratio constraint once for this drag
        // tick. `None` is the freeform path; otherwise both NewCrop
        // and DragHandle project their results onto the constraint.
        let aspect = self.aspect_ratio.ratio_components(self.image_bounds);

        let crop = match &mut self.crop {
            Some(c) => c,
            None => return ToolUpdateResult::Unmodified,
        };

        let action = match &self.action {
            Some(a) => a,
            None => return ToolUpdateResult::Unmodified,
        };

        match action {
            CropToolAction::NewCrop => {
                // Drag-to-create: snap the dragged corner (start + dir)
                // to image edges if applicable. The starting corner
                // (`crop.pos`) was captured at BeginDrag and isn't
                // re-snapped here — feels more predictable than having
                // both ends jump.
                let ex = snap_x(crop.pos.x + direction.x);
                let ey = snap_y(crop.pos.y + direction.y);
                let mut sx = ex - crop.pos.x;
                let mut sy = ey - crop.pos.y;
                if let Some((rw, rh)) = aspect
                    && rh > 0.0
                {
                    let r = rw / rh;
                    let abs_w = sx.abs();
                    let abs_h = sy.abs();
                    // Pure-horizontal / pure-vertical drags get
                    // signed: default down/right when the user
                    // hasn't moved perpendicular yet so the rect
                    // grows in a predictable direction.
                    let sign_x = if sx < 0.0 { -1.0 } else { 1.0 };
                    let sign_y = if sy < 0.0 { -1.0 } else { 1.0 };
                    if abs_w / r >= abs_h {
                        sx = sign_x * abs_w;
                        sy = sign_y * (abs_w / r);
                    } else {
                        sx = sign_x * (abs_h * r);
                        sy = sign_y * abs_h;
                    }
                }
                crop.size = Vec2D::new(sx, sy);
                self.emit_crop_dimensions_update();
                ToolUpdateResult::Redraw
            }
            CropToolAction::DragHandle(state) => {
                Self::apply_drag_handle_transformation(
                    crop, state, direction, aspect, snap_x, snap_y,
                );
                self.emit_crop_dimensions_update();
                ToolUpdateResult::Redraw
            }
            CropToolAction::Move(state) => {
                // Move: snap whichever edge of the crop is closest to
                // an image edge. Try the leading (top/left) edge first;
                // if neither it nor the trailing edge wants to snap,
                // the crop moves freely. Keeps the user's chosen size
                // intact (we only translate, never resize on Move).
                let new_pos = state.start + direction;
                let final_x = {
                    let left = snap_x(new_pos.x);
                    if left != new_pos.x {
                        left
                    } else {
                        let right = snap_x(new_pos.x + crop.size.x);
                        if right != new_pos.x + crop.size.x {
                            right - crop.size.x
                        } else {
                            new_pos.x
                        }
                    }
                };
                let final_y = {
                    let top = snap_y(new_pos.y);
                    if top != new_pos.y {
                        top
                    } else {
                        let bottom = snap_y(new_pos.y + crop.size.y);
                        if bottom != new_pos.y + crop.size.y {
                            bottom - crop.size.y
                        } else {
                            new_pos.y
                        }
                    }
                };
                crop.pos = Vec2D::new(final_x, final_y);
                ToolUpdateResult::Redraw
            }
        }
    }

    fn end_drag(&mut self, direction: Vec2D, modifier: ModifierType) -> ToolUpdateResult {
        // EndDrag finalizes whatever UpdateDrag was producing, so the
        // snap-aware transform runs once more here and `action` is
        // cleared. Reusing `update_drag` keeps both code paths
        // identical and ensures the visible-during-drag position
        // matches the committed-on-release position (any divergence
        // would feel like a "jump" when the user releases).
        let result = self.update_drag(direction, modifier);
        self.action = None;
        match result {
            ToolUpdateResult::Unmodified => ToolUpdateResult::Unmodified,
            _ => ToolUpdateResult::Redraw,
        }
    }

    fn handle_deactivate_and_reset(&mut self) -> ToolUpdateResult {
        self.crop = None;
        self.action = None;
        self.emit_crop_presence(false);

        if let Some(sender) = &self.sender {
            sender
                .send(SketchBoardInput::Output(
                    SketchBoardOutput::DimensionsUpdate(None),
                ))
                .ok();
        }
        ToolUpdateResult::RedrawAndStopPropagation
    }
}

impl Tool for CropTool {
    fn input_enabled(&self) -> bool {
        self.input_enabled
    }

    fn set_input_enabled(&mut self, value: bool) {
        self.input_enabled = value;
    }

    fn get_tool_type(&self) -> super::Tools {
        Tools::Crop
    }

    fn handle_key_event(&mut self, event: KeyEventMsg) -> ToolUpdateResult {
        match event.key {
            Key::Escape => self.cancel(),
            Key::Return | Key::KP_Enter if self.crop.is_some() => self.commit(),
            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        // Once a crop has been committed (Enter pressed and the user is
        // looking at the zoomed-in view), Primary mouse events are
        // locked out entirely. Without this guard, BeginDrag→Move would
        // shift `crop.pos` and the fit-to-canvas transform would render
        // a different region of the underlying image — the user
        // perceives it as panning, even though the original is staying
        // put and what's really happening is the crop is dragging out
        // from under them. To re-edit, switch tools and switch back —
        // `handle_activated` flips `active` on and `committed` off.
        if let Some(crop) = &self.crop
            && crop.is_committed()
            && event.button == MouseButton::Primary
        {
            return ToolUpdateResult::Unmodified;
        }
        match event.type_ {
            MouseEventType::Click if event.button == MouseButton::Secondary => {
                if let Some(crop) = &self.crop
                    && crop.active
                {
                    self.handle_deactivate_and_reset()
                } else {
                    ToolUpdateResult::Unmodified
                }
            }
            MouseEventType::BeginDrag if event.button == MouseButton::Primary => {
                self.begin_drag(event.pos, event.modifier)
            }
            MouseEventType::EndDrag if event.button == MouseButton::Primary => {
                self.end_drag(event.pos, event.modifier)
            }
            MouseEventType::UpdateDrag if event.button == MouseButton::Primary => {
                self.update_drag(event.pos, event.modifier)
            }
            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn handle_activated(&mut self) -> ToolUpdateResult {
        if let Some(c) = &mut self.crop {
            // Re-entering the crop tool drops the user back into edit
            // mode against the original image — we suppress the
            // committed/zoomed view so the crop tool can render its
            // overlay against the full bounds. The crop region itself
            // is preserved so the user adjusts what they had.
            let was_committed = c.committed;
            c.active = true;
            c.committed = false;
            // If the previous frame was showing a committed crop, the
            // canvas is now displaying the full image again — bump the
            // window back up to fit it.
            if was_committed && let Some(bounds) = self.image_bounds {
                self.emit_content_size(bounds.x, bounds.y);
            }
            // The Revert button is gated on `has_crop` in the bottom
            // toolbar; re-asserting presence on every tool entry keeps
            // it in sync even if a prior path forgot to emit.
            self.emit_crop_presence(true);
            return ToolUpdateResult::Redraw;
        }
        // First time entering Crop with no prior crop on file — seed a
        // box covering the whole image so the user has corner/edge
        // handles to drag inward immediately, rather than landing on a
        // bare canvas and having to draw a rectangle from scratch.
        if let Some(bounds) = self.image_bounds {
            self.crop = Some(Crop {
                pos: Vec2D::zero(),
                size: bounds,
                active: true,
                committed: false,
                ever_committed: false,
                last_committed: None,
            });
            // Seeded crop counts as "crop present" — surface Revert
            // immediately so the user can bail out of crop mode
            // without first dragging.
            self.emit_crop_presence(true);
            return ToolUpdateResult::Redraw;
        }
        ToolUpdateResult::Unmodified
    }

    fn handle_deactivated(&mut self) -> ToolUpdateResult {
        if let Some(c) = &mut self.crop {
            c.active = false;
            // Re-entry edit that's leaving without re-pressing Enter:
            // roll pos/size back to the last committed snapshot and
            // re-commit so the renderer snaps the view back to the
            // prior cropped frame. Pending adjustments are discarded 
            // unless explicitly committed. `ever_committed=false` 
            // skips this entirely (first-time draft) so accidentally 
            // clicking another tool while shaping a brand-new crop 
            // keeps the in-progress region around for re-entry.
            if c.ever_committed
                && !c.committed
                && let Some((p, s)) = c.last_committed
            {
                c.pos = p;
                c.size = s;
                c.committed = true;
                self.emit_content_size(s.x, s.y);
            }
        }
        self.action = None;
        ToolUpdateResult::Redraw
    }

    fn get_drawable(&self) -> Option<&dyn Drawable> {
        // the reason we always return None is because we dont want this tool
        // to show up with the standard rendering mechanism. Instead it will always
        // be drawn separately by using `get_crop(&self)`
        None
    }

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>) {
        self.sender = Some(sender);
    }
}
