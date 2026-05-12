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
        snap_x: impl Fn(f32) -> f32,
        snap_y: impl Fn(f32) -> f32,
    ) {
        let mut tl = state.top_left_start;
        let mut br = state.bottom_right_start;

        // Apply the per-handle transformation, then snap each dragged
        // coordinate through the caller's snap closures. Handles that
        // only move along one axis only snap that axis — e.g. the
        // top edge doesn't try to snap left/right.
        match state.handle {
            CropHandle::TopLeftCorner => {
                tl.x = snap_x(tl.x + direction.x);
                tl.y = snap_y(tl.y + direction.y);
            }
            CropHandle::TopEdge => {
                tl.y = snap_y(tl.y + direction.y);
            }
            CropHandle::TopRightCorner => {
                tl.y = snap_y(tl.y + direction.y);
                br.x = snap_x(br.x + direction.x);
            }
            CropHandle::RightEdge => {
                br.x = snap_x(br.x + direction.x);
            }
            CropHandle::BottomRightCorner => {
                br.x = snap_x(br.x + direction.x);
                br.y = snap_y(br.y + direction.y);
            }
            CropHandle::BottomEdge => {
                br.y = snap_y(br.y + direction.y);
            }
            CropHandle::BottomLeftCorner => {
                tl.x = snap_x(tl.x + direction.x);
                br.y = snap_y(br.y + direction.y);
            }
            CropHandle::LeftEdge => {
                tl.x = snap_x(tl.x + direction.x);
            }
        }

        // convert back and save
        crop.pos = tl;
        crop.size = br - tl;
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
                crop.size = Vec2D::new(ex - crop.pos.x, ey - crop.pos.y);
                self.emit_crop_dimensions_update();
                ToolUpdateResult::Redraw
            }
            CropToolAction::DragHandle(state) => {
                Self::apply_drag_handle_transformation(crop, state, direction, snap_x, snap_y);
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
            Key::Escape => {
                // Esc always exits Crop and returns to the previously
                // selected tool (Pointer if none was recorded). If
                // there's an uncommitted in-progress crop we drop it
                // so the user doesn't come back to a stray dim
                // overlay; a previously-committed crop is preserved
                // (the user only "left edit mode", not "removed the
                // crop").
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
            //FIXME: use if let guards as soon as they're stabilized (1.95)
            Key::Return | Key::KP_Enter if self.crop.is_some() => {
                let crop = self.crop.as_mut().unwrap();
                if crop.active {
                    // Pressing Enter while editing "applies" the crop:
                    // mark it committed so the renderer switches to
                    // zoomed-in view, and deactivate so the dim
                    // overlay + handles are no longer drawn over the
                    // zoomed image. `ever_committed` sticks so future
                    // Esc presses know to restore the committed view.
                    let (pos, size) = crop.get_rectangle();
                    crop.pos = pos;
                    crop.size = size;
                    crop.last_committed = Some((pos, size));
                    crop.committed = true;
                    crop.ever_committed = true;
                    crop.active = false;
                    self.action = None;
                    // Push the new content size out so main.rs can
                    // shrink the window around the cropped region.
                    self.emit_content_size(size.x, size.y);
                    // Hand the user back to the main view — emit a
                    // tool switch so the StyleToolbar reappears and
                    // the crop bottom bar collapses. Pointer is a
                    // neutral default; user can pick whichever tool
                    // they want next via the top toolbar.
                    if let Some(sender) = &self.sender {
                        sender
                            .send(SketchBoardInput::ToolbarEvent(
                                ToolbarEvent::ToolSelected(Tools::Pointer),
                            ))
                            .ok();
                    }
                    ToolUpdateResult::Redraw
                } else {
                    ToolUpdateResult::Unmodified
                }
            }
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
