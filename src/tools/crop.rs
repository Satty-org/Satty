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
    /// Position of the crop frame in CANVAS (CSS-pixel) coords,
    /// derived from the image-coord `pos`/`size` via the renderer's
    /// transform whenever the tool refreshes (`refresh_canvas_from_image`).
    /// During inside-out edit (CROP_INSIDE_OUT_PLAN.md) the canvas-
    /// coord twin is what the user sees fixed on screen; the wheel +
    /// pan phases re-derive image coords FROM this pair to keep the
    /// canvas frame anchored while the image scales / scrolls
    /// beneath it. Outside edit mode the value is stale bookkeeping.
    canvas_pos: Vec2D,
    canvas_size: Vec2D,
    /// Color of the matte rendered OUTSIDE the crop rectangle. Set
    /// from the top toolbar's background-color dropdown via
    /// `CropTool::set_bg_color`.
    bg_color: CropBgColor,
}

pub struct CropTool {
    crop: Option<Crop>,
    action: Option<CropToolAction>,
    input_enabled: bool,
    sender: Option<Sender<SketchBoardInput>>,
    /// Snap crop edges to image edges during drag. Toggled from the
    /// bottom-left checkbox, persisted via
    /// `state::save_snap_to_edges`. Defaults to true. Holding Alt
    /// during a drag temporarily bypasses snap regardless of this
    /// flag — Ctrl is reserved for the inside-out pan gesture
    /// (CROP_INSIDE_OUT_PLAN.md, Phase 5), so the temporary-defeat
    /// modifier moved off Ctrl onto Alt.
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
    /// Currently-selected background-color preset for the matte
    /// outside the crop rect. Mirrored on each `Crop` instance the
    /// tool builds (seed, re-seed, etc.) so the drawable sees it
    /// without a back-reference.
    bg_color: CropBgColor,
    /// Latest renderer transform snapshot (`effective_scale`,
    /// `effective_offset`, device-pixel-ratio). Pushed by sketch_board
    /// on tool activation and after every transform-changing event
    /// (wheel zoom, pan, render-tick re-derive). The crop tool's
    /// inside-out edit workflow uses it to round-trip its canvas-
    /// fixed frame against the image-coord rect that rendering +
    /// hit-tests still consume. `None` before the first push.
    render_transform: Option<RenderTransformSnapshot>,
    /// Bitmask of which arrow keys are currently held — bit 0 Up,
    /// 1 Down, 2 Left, 3 Right. Combined with the per-event
    /// modifier into a single delta per arrow-key event so
    /// holding (Up, Right) moves diagonally instead of stepping
    /// along just one axis. KeyRelease for an arrow clears the
    /// corresponding bit; window unfocus + tool deactivate also
    /// reset the whole mask so a stuck-held key doesn't leak
    /// state across sessions.
    held_arrows: u8,
}

const ARROW_UP: u8 = 1 << 0;
const ARROW_DOWN: u8 = 1 << 1;
const ARROW_LEFT: u8 = 1 << 2;
const ARROW_RIGHT: u8 = 1 << 3;

fn arrow_bit(key: Key) -> Option<u8> {
    match key {
        Key::Up | Key::uparrow => Some(ARROW_UP),
        Key::Down | Key::downarrow => Some(ARROW_DOWN),
        Key::Left | Key::leftarrow => Some(ARROW_LEFT),
        Key::Right | Key::rightarrow => Some(ARROW_RIGHT),
        _ => None,
    }
}

/// Combined translation delta in image coords (+y is down) for
/// the held arrows at `step` per axis. Diagonal holds get one
/// axis-step of motion on each axis rather than zero.
fn move_delta(held: u8, step: f32) -> (f32, f32) {
    let dx = if held & ARROW_RIGHT != 0 { step } else { 0.0 }
        - if held & ARROW_LEFT != 0 { step } else { 0.0 };
    let dy = if held & ARROW_DOWN != 0 { step } else { 0.0 }
        - if held & ARROW_UP != 0 { step } else { 0.0 };
    (dx, dy)
}

/// Combined symmetric-resize delta (dw, dh) for the held arrows
/// at `step` per axis. Up/Right grow, Down/Left shrink.
fn resize_delta(held: u8, step: f32) -> (f32, f32) {
    let dw = if held & ARROW_RIGHT != 0 { step } else { 0.0 }
        - if held & ARROW_LEFT != 0 { step } else { 0.0 };
    let dh = if held & ARROW_UP != 0 { step } else { 0.0 }
        - if held & ARROW_DOWN != 0 { step } else { 0.0 };
    (dw, dh)
}

/// Color shown OUTSIDE the crop rectangle while the tool is active —
/// the dimmed / opaque "matte" surrounding the framed region. Stored
/// on each `Crop` so `Drawable::draw` can read it without a back-
/// reference to `CropTool`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum CropBgColor {
    /// Default — semi-transparent black, lets the image bleed through
    /// dimmed.
    #[default]
    Auto,
    /// No matte at all — the area outside the crop renders untouched.
    /// The crop border / handles still draw so the user can shape it.
    Transparent,
    /// Opaque white matte.
    White,
    /// Opaque medium gray matte.
    Gray,
    /// Opaque black matte (vs. `Auto`'s semi-transparent black).
    Black,
    /// User-picked color via the bottom "Custom Color…" entry.
    /// Stored as `(r, g, b)` floats in 0..1 — alpha is hard-coded to
    /// fully opaque so the user's pick reads as a solid matte.
    Custom(f32, f32, f32),
}

impl CropBgColor {
    /// Resolve to a femtovg `Color` ready for `Paint::color(...)`.
    /// `Transparent` reports alpha=0 so the caller can skip drawing
    /// the matte rect entirely.
    pub fn paint_color(self) -> Color {
        match self {
            // 0.5 alpha black gives the dim that lets the screenshot 
            // show through. Other "named" colors are fully opaque so 
            // they read as a solid frame.
            CropBgColor::Auto => Color::rgbaf(0.0, 0.0, 0.0, 0.5),
            CropBgColor::Transparent => Color::rgbaf(0.0, 0.0, 0.0, 0.0),
            CropBgColor::White => Color::rgbaf(1.0, 1.0, 1.0, 1.0),
            CropBgColor::Gray => Color::rgbaf(0.5, 0.5, 0.5, 1.0),
            CropBgColor::Black => Color::rgbaf(0.0, 0.0, 0.0, 1.0),
            CropBgColor::Custom(r, g, b) => Color::rgbaf(r, g, b, 1.0),
        }
    }

    /// Dropdown index used by the top toolbar's bg-color picker.
    /// Order matches `ALL_LABELS`; `Custom(_)` collapses to the last
    /// "Custom Color…" entry regardless of the stored RGB triple.
    pub const ALL_LABELS: &'static [&'static str] = &[
        "Auto",
        "Transparent",
        "White",
        "Gray",
        "Black",
        "Custom Color…",
    ];

    pub fn from_index(i: usize) -> Self {
        match i {
            0 => CropBgColor::Auto,
            1 => CropBgColor::Transparent,
            2 => CropBgColor::White,
            3 => CropBgColor::Gray,
            4 => CropBgColor::Black,
            // Custom rounds back to a neutral mid-gray on selection;
            // a "Custom Color…" picker dialog is left for a follow-up.
            5 => CropBgColor::Custom(0.5, 0.5, 0.5),
            _ => CropBgColor::default(),
        }
    }

    pub fn to_index(self) -> usize {
        match self {
            CropBgColor::Auto => 0,
            CropBgColor::Transparent => 1,
            CropBgColor::White => 2,
            CropBgColor::Gray => 3,
            CropBgColor::Black => 4,
            CropBgColor::Custom(..) => 5,
        }
    }
}

/// Aspect-ratio constraint applied to crop drags. Common photo /
/// display ratios; a user-typed "Custom Ratio" entry would need a
/// sub-dialog and is left for a follow-up.
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
            bg_color: CropBgColor::Auto,
            render_transform: None,
            held_arrows: 0,
        }
    }
}

/// Snapshot of the renderer's image↔canvas transform, captured by
/// sketch_board whenever a transform-changing event lands (tool
/// activation, wheel zoom, pan). The crop tool's inside-out edit
/// workflow uses it to derive its canvas-fixed frame's image-coord
/// twin (and vice versa for the upcoming wheel + pan phases) without
/// a back-reference to the renderer.
///
/// Mirrors the `effective_scale` / `effective_offset` pair the
/// renderer caches in `update_transformation`; `dpi` is the widget's
/// device-pixel-ratio so we can round-trip CSS-pixel canvas coords
/// against the physical-pixel offset the renderer stores.
#[derive(Debug, Clone, Copy)]
struct RenderTransformSnapshot {
    effective_scale: f32,
    effective_offset: Vec2D,
    dpi: f32,
}

impl RenderTransformSnapshot {
    fn is_valid(&self) -> bool {
        self.effective_scale > 0.0 && self.dpi > 0.0
    }

    fn image_to_canvas_rect(
        &self,
        image_pos: Vec2D,
        image_size: Vec2D,
    ) -> (Vec2D, Vec2D) {
        let cp = Vec2D::new(
            (image_pos.x * self.effective_scale + self.effective_offset.x) / self.dpi,
            (image_pos.y * self.effective_scale + self.effective_offset.y) / self.dpi,
        );
        let cs = Vec2D::new(
            image_size.x * self.effective_scale / self.dpi,
            image_size.y * self.effective_scale / self.dpi,
        );
        (cp, cs)
    }

    fn canvas_to_image_rect(
        &self,
        canvas_pos: Vec2D,
        canvas_size: Vec2D,
    ) -> (Vec2D, Vec2D) {
        let ip = Vec2D::new(
            (canvas_pos.x * self.dpi - self.effective_offset.x) / self.effective_scale,
            (canvas_pos.y * self.dpi - self.effective_offset.y) / self.effective_scale,
        );
        let is = Vec2D::new(
            canvas_size.x * self.dpi / self.effective_scale,
            canvas_size.y * self.dpi / self.effective_scale,
        );
        (ip, is)
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

    fn new(pos: Vec2D, bg_color: CropBgColor) -> Self {
        Self {
            pos,
            size: Vec2D::zero(),
            active: true,
            committed: false,
            ever_committed: false,
            last_committed: None,
            canvas_pos: Vec2D::zero(),
            canvas_size: Vec2D::zero(),
            bg_color,
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
    /// Edge-aware handle hit test. Corner handles match within
    /// `tolerance` of the corner point; edge handles match anywhere
    /// along the edge SEGMENT (within `tolerance` perpendicular
    /// distance from the edge line, and between the two corners on
    /// the parallel axis). Corners take precedence in the diagonal
    /// zone so a hit near a corner resolves to the corner handle
    /// rather than one of its two edges.
    ///
    /// `tolerance` is in the same coordinate space as `point`
    /// (callers in image-space pass an image-pixel tolerance;
    /// callers compensating for canvas scale pre-divide).
    ///
    /// The visible edge "bar" at the midpoint is purely a hint —
    /// the user can grab anywhere along the edge.
    fn hit_handle(&self, point: Vec2D, tolerance: f32) -> Option<CropHandle> {
        let (pos, size) = self.get_rectangle();
        if size.x <= 0.0 || size.y <= 0.0 {
            return None;
        }
        let right = pos.x + size.x;
        let bottom = pos.y + size.y;
        let tol2 = tolerance * tolerance;

        for (handle, corner) in [
            (CropHandle::TopLeftCorner, Vec2D::new(pos.x, pos.y)),
            (CropHandle::TopRightCorner, Vec2D::new(right, pos.y)),
            (CropHandle::BottomRightCorner, Vec2D::new(right, bottom)),
            (CropHandle::BottomLeftCorner, Vec2D::new(pos.x, bottom)),
        ] {
            if (corner - point).norm2() <= tol2 {
                return Some(handle);
            }
        }

        let in_horizontal_span = point.x >= pos.x && point.x <= right;
        let in_vertical_span = point.y >= pos.y && point.y <= bottom;

        if in_horizontal_span && (point.y - pos.y).abs() <= tolerance {
            return Some(CropHandle::TopEdge);
        }
        if in_horizontal_span && (point.y - bottom).abs() <= tolerance {
            return Some(CropHandle::BottomEdge);
        }
        if in_vertical_span && (point.x - pos.x).abs() <= tolerance {
            return Some(CropHandle::LeftEdge);
        }
        if in_vertical_span && (point.x - right).abs() <= tolerance {
            return Some(CropHandle::RightEdge);
        }
        None
    }

    /// Hit-test classification used by the hover-cursor logic. Reports
    /// `Handle` when the pointer is over any corner or anywhere along
    /// an edge, `Body` when inside the crop rectangle, and `None` for
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
        // an equivalent tolerance in image-space units (where `point`
        // and the rect coordinates are expressed).
        let scale = image_to_canvas_scale.max(0.0001);
        let tolerance = Self::HANDLE_HIT_RADIUS / scale;
        if let Some(handle) = self.hit_handle(point, tolerance) {
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
    fn kind_label(&self) -> &'static str {
        "Crop"
    }
    fn icon_name(&self) -> &'static str {
        "crop-filled"
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

        // Outside-crop matte. `bg_color` picks the color preset; we
        // skip the fill entirely when the user picked Transparent
        // (alpha == 0) so the canvas behind shows through unblended.
        let matte = self.bg_color.paint_color();
        canvas.save();
        canvas.reset_transform();
        if matte.a > 0.0 {
            let shadow_paint =
                Paint::color(matte).with_fill_rule(femtovg::FillRule::EvenOdd);
            let mut shadow_path = Path::new();
            shadow_path.rect(0.0, 0.0, canvas.width() as f32, canvas.height() as f32);
            shadow_path.rect(crop_canvas_x, crop_canvas_y, crop_canvas_w, crop_canvas_h);
            canvas.fill_path(&shadow_path, &shadow_paint);
        }
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
    ///
    /// `emit_resize` controls whether to broadcast a
    /// `ContentSizeChanged` with the new image bounds. Default
    /// callers (revert button) want this so the window re-fits the
    /// uncropped image; the rotate path passes `false` because it
    /// emits its own zoom-scaled resize and the bounds emit here
    /// would otherwise win the race and blow the window up to
    /// native pixel size.
    pub fn revert_to_seed(&mut self, emit_resize: bool) {
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
            canvas_pos: Vec2D::zero(),
            canvas_size: Vec2D::zero(),
            bg_color: self.bg_color,
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
        if emit_resize {
            self.emit_content_size(bounds.x, bounds.y);
        }
        self.refresh_canvas_from_image();
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
        let (mut pos, mut size) = crop.get_rectangle();
        // Inside-out edit can push the canvas frame past the image
        // edges (zoom hits the 10 %/500 % clamp, pan slides image
        // away). Clamp the committed image rect back inside the
        // bounds so the output is always a valid subregion — the
        // saved screenshot can't reference image pixels that don't
        // exist. Skipped if we don't have bounds yet (shouldn't
        // happen after init, defensive).
        if let Some(b) = self.image_bounds {
            let left = pos.x.clamp(0.0, b.x);
            let top = pos.y.clamp(0.0, b.y);
            let right = (pos.x + size.x).clamp(0.0, b.x);
            let bottom = (pos.y + size.y).clamp(0.0, b.y);
            pos = Vec2D::new(left, top);
            size = Vec2D::new((right - left).max(0.0), (bottom - top).max(0.0));
        }
        crop.pos = pos;
        crop.size = size;
        crop.last_committed = Some((pos, size));
        crop.committed = true;
        crop.ever_committed = true;
        crop.active = false;
        self.action = None;
        self.emit_content_size(size.x, size.y);
        // The OUTPUT dimensions just became the cropped rect —
        // push to the bottom-right readout so the user sees the
        // new effective image size after commit. (Drags don't
        // touch this readout; only commit / revert do.)
        self.emit_output_dimensions(Some((size.x.round() as i32, size.y.round() as i32)));
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

    /// True when the inside-out edit workflow's canvas-fixed frame is
    /// the user's current focus: a crop exists, is flagged active, and
    /// is not committed (the post-Enter zoomed-in view doesn't count).
    /// Sketch_board reads this to decide whether a wheel-zoom or pan
    /// gesture should re-derive the image-coord rect from the canvas-
    /// coord twin (canvas-fixed frame, Phase 4/5) or fall through to
    /// the normal full-canvas zoom + pan.
    pub fn is_active_edit(&self) -> bool {
        self.crop
            .as_ref()
            .is_some_and(|c| c.active && !c.committed)
    }

    /// Push the renderer's current image↔canvas transform into the
    /// tool. Sketch_board calls this on tool activation and after any
    /// transform-changing event (wheel zoom, pan) so the drag handlers
    /// + render-tick re-derive paths can convert between canvas and
    /// image coords without a back-reference to the renderer.
    pub fn set_render_transform(
        &mut self,
        effective_scale: f32,
        effective_offset: Vec2D,
        dpi: f32,
    ) {
        self.render_transform = Some(RenderTransformSnapshot {
            effective_scale,
            effective_offset,
            dpi,
        });
    }

    /// Refresh the live crop's canvas-coord frame from its image-coord
    /// rect via the cached render transform. Called after every drag
    /// tick so the canvas-coord twin stays in sync with the image-coord
    /// source of truth that snap + aspect + clamp math operates on.
    /// No-op when there's no crop or no transform snapshot.
    pub fn refresh_canvas_from_image(&mut self) {
        let Some(t) = self.render_transform.filter(|t| t.is_valid()) else {
            return;
        };
        if let Some(c) = self.crop.as_mut() {
            let (cp, cs) = t.image_to_canvas_rect(c.pos, c.size);
            c.canvas_pos = cp;
            c.canvas_size = cs;
        }
    }

    /// Mirror of `refresh_canvas_from_image` — derive image-coord rect
    /// from the canvas-coord twin via the cached transform. Reserved
    /// for the wheel + pan phases where the renderer's transform
    /// changes while the canvas frame must stay put: canvas is the
    /// authority and image is re-derived to track the new transform.
    pub fn refresh_image_from_canvas(&mut self) {
        let Some(t) = self.render_transform.filter(|t| t.is_valid()) else {
            return;
        };
        if let Some(c) = self.crop.as_mut() {
            let (ip, is) = t.canvas_to_image_rect(c.canvas_pos, c.canvas_size);
            c.pos = ip;
            c.size = is;
        }
    }

    /// Clamp the live crop's image-space rect to the image bounds and
    /// re-sync the canvas-coord twin so the on-screen frame visibly
    /// snaps back inside the image edges. Called after each wheel-
    /// zoom / Ctrl+drag-pan tick so the inside-out edit can't let
    /// the frame run off the image — once the frame represents pixels
    /// that don't exist, a commit produces an invalid crop. Drops the
    /// crop entirely if the clamp would collapse it to zero area
    /// (frame entirely outside the image).
    pub fn clamp_image_to_bounds(&mut self) {
        let Some(bounds) = self.image_bounds else {
            return;
        };
        if let Some(c) = self.crop.as_mut() {
            let left = c.pos.x.clamp(0.0, bounds.x);
            let top = c.pos.y.clamp(0.0, bounds.y);
            let right = (c.pos.x + c.size.x).clamp(0.0, bounds.x);
            let bottom = (c.pos.y + c.size.y).clamp(0.0, bounds.y);
            c.pos = Vec2D::new(left, top);
            c.size = Vec2D::new((right - left).max(0.0), (bottom - top).max(0.0));
        }
        // Push the clamped image-coords back through the transform so
        // the canvas-coord frame visibly tracks the clamp.
        self.refresh_canvas_from_image();
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

    /// Nudge the crop frame by (dx, dy) image-space pixels and
    /// clamp the result so the whole crop stays inside the image.
    /// Returns true if anything actually moved — the caller should
    /// only request a redraw on that signal so a no-op key (crop
    /// already pinned against the edge, or as big as the image)
    /// doesn't spam idle redraws. Used by the keyboard-arrow
    /// shortcut in Crop mode.
    pub fn nudge(&mut self, dx: f32, dy: f32) -> bool {
        let Some(bounds) = self.image_bounds else {
            return false;
        };
        let Some(c) = self.crop.as_mut() else {
            return false;
        };
        // No room to move if the crop already fills the image on
        // both axes.
        if c.size.x >= bounds.x && c.size.y >= bounds.y {
            return false;
        }
        let max_x = (bounds.x - c.size.x).max(0.0);
        let max_y = (bounds.y - c.size.y).max(0.0);
        let new_x = (c.pos.x + dx).clamp(0.0, max_x);
        let new_y = (c.pos.y + dy).clamp(0.0, max_y);
        if (new_x - c.pos.x).abs() < f32::EPSILON
            && (new_y - c.pos.y).abs() < f32::EPSILON
        {
            return false;
        }
        c.pos = Vec2D::new(new_x, new_y);
        self.refresh_canvas_from_image();
        self.emit_crop_edit_dimensions();
        true
    }

    /// Resize the crop symmetrically by (dw, dh) image-space
    /// pixels — half on each side, so the geometric center stays
    /// put when there's room. Returns true if anything changed.
    /// Clamps to image bounds: when a side would push past the
    /// image edge, that side pins flush and the opposite side
    /// absorbs the remaining delta (so a Ctrl+Right against the
    /// right edge instead pushes the left edge leftward, keeping
    /// the requested width growth). Minimum crop side is 1 px so
    /// a relentless contract doesn't collapse the crop to zero
    /// area. Used by the Ctrl+arrow keyboard resize shortcut.
    pub fn resize_symmetric(&mut self, dw: f32, dh: f32) -> bool {
        let Some(bounds) = self.image_bounds else {
            return false;
        };
        let Some(c) = self.crop.as_mut() else {
            return false;
        };
        const MIN_SIDE: f32 = 1.0;
        let mut changed = false;
        if dw != 0.0 {
            let new_w = (c.size.x + dw).clamp(MIN_SIDE, bounds.x);
            if (new_w - c.size.x).abs() > f32::EPSILON {
                // Pin center, then clamp the position so the
                // wider/narrower box stays inside the image. If
                // either edge would push past, slide the position
                // so the box rests against that edge.
                let center = c.pos.x + c.size.x * 0.5;
                let mut new_x = center - new_w * 0.5;
                new_x = new_x.clamp(0.0, (bounds.x - new_w).max(0.0));
                c.pos.x = new_x;
                c.size.x = new_w;
                changed = true;
            }
        }
        if dh != 0.0 {
            let new_h = (c.size.y + dh).clamp(MIN_SIDE, bounds.y);
            if (new_h - c.size.y).abs() > f32::EPSILON {
                let center = c.pos.y + c.size.y * 0.5;
                let mut new_y = center - new_h * 0.5;
                new_y = new_y.clamp(0.0, (bounds.y - new_h).max(0.0));
                c.pos.y = new_y;
                c.size.y = new_h;
                changed = true;
            }
        }
        if changed {
            self.refresh_canvas_from_image();
            self.emit_crop_edit_dimensions();
        }
        changed
    }

    /// Scale the crop rect by `factor` around its geometric center,
    /// applying the same multiplier to both axes so the aspect ratio
    /// is preserved (until one side hits a clamp). Used by the Ctrl+
    /// wheel gesture in Crop edit mode — scroll up grows toward the
    /// canvas outsides (clamped to image bounds), scroll down shrinks
    /// toward the crop's middle. Returns whether anything changed.
    pub fn resize_proportional(&mut self, factor: f32) -> bool {
        if !factor.is_finite() || factor <= 0.0 || (factor - 1.0).abs() < f32::EPSILON {
            return false;
        }
        let (cur_w, cur_h, bounds) = match (&self.crop, self.image_bounds) {
            (Some(c), Some(b)) => (c.size.x, c.size.y, b),
            _ => return false,
        };
        let new_w = (cur_w * factor).clamp(1.0, bounds.x);
        let new_h = (cur_h * factor).clamp(1.0, bounds.y);
        let dw = new_w - cur_w;
        let dh = new_h - cur_h;
        if dw.abs() < f32::EPSILON && dh.abs() < f32::EPSILON {
            return false;
        }
        self.resize_symmetric(dw, dh)
    }

    /// Snap the crop to one of five preset positions sized at a
    /// quarter of the image:
    ///   1 = upper-left,   2 = upper-right,
    ///   3 = lower-left,   4 = lower-right,
    ///   5 = centered.
    /// Returns true if it actually applied a preset so the caller
    /// can decide whether to redraw. Used by the `1`-`5` digit
    /// shortcuts in Crop mode for a fully-keyboard "pick a
    /// quadrant, then shift+arrows to nudge" flow.
    pub fn apply_quadrant_preset(&mut self, preset: u32) -> bool {
        let Some(bounds) = self.image_bounds else {
            return false;
        };
        let Some(c) = self.crop.as_mut() else {
            return false;
        };
        let half_w = bounds.x * 0.5;
        let half_h = bounds.y * 0.5;
        let (pos, size) = match preset {
            1 => (Vec2D::zero(), Vec2D::new(half_w, half_h)),
            2 => (Vec2D::new(half_w, 0.0), Vec2D::new(half_w, half_h)),
            3 => (Vec2D::new(0.0, half_h), Vec2D::new(half_w, half_h)),
            4 => (Vec2D::new(half_w, half_h), Vec2D::new(half_w, half_h)),
            5 => (
                Vec2D::new(bounds.x * 0.25, bounds.y * 0.25),
                Vec2D::new(half_w, half_h),
            ),
            _ => return false,
        };
        c.pos = pos;
        c.size = size;
        self.refresh_canvas_from_image();
        self.emit_crop_edit_dimensions();
        true
    }

    /// Threshold within which an edge "sticks" to the image boundary,
    /// in image-space pixels. Stays in image units because all snap
    /// math is in image-space; we don't try to compensate for zoom
    /// (a tighter pixel threshold at high zoom is acceptable since
    /// the user is also more precise then).
    const SNAP_PIXELS: f32 = 8.0;

    fn snap_active(&self, modifier: ModifierType) -> bool {
        // "Snap on, hold the modifier to defeat" semantic. Moved from 
        // Ctrl to Alt. Shift is already the global "snap-to-angle"
        // modifier on Line/Arrow/Rect/Ellipse so reusing it as a
        // snap-defeat would be the opposite of the established
        // convention; Alt was unused for crop drag and reads cleanly
        // as the "alternate / temporary override" modifier.
        self.snap_to_edges && !modifier.contains(ModifierType::ALT_MASK)
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
        bounds: Option<Vec2D>,
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

        // Final clamp to image bounds. The aspect-ratio block can
        // re-grow the rect past the boundary (e.g. dragging a
        // corner outward with 16:9 locked once the long axis is
        // already at the image edge), so a per-axis clamp here
        // catches whatever the snap clamps didn't already pin.
        // We deliberately clamp coordinates only — preserving the
        // aspect ratio at the boundary would require shrinking
        // both dims, which fights against the user still dragging.
        if let Some(b) = bounds {
            tl.x = tl.x.clamp(0.0, b.x);
            tl.y = tl.y.clamp(0.0, b.y);
            br.x = br.x.clamp(0.0, b.x);
            br.y = br.y.clamp(0.0, b.y);
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
        self.emit_crop_edit_dimensions();
        self.refresh_canvas_from_image();
    }

    pub fn aspect_ratio(&self) -> AspectRatio {
        self.aspect_ratio
    }

    /// Set the matte color shown outside the crop rectangle. Mirrors
    /// the choice onto the live `Crop` (if any) so the next redraw
    /// reflects the change immediately.
    pub fn set_bg_color(&mut self, bg: CropBgColor) {
        self.bg_color = bg;
        if let Some(c) = self.crop.as_mut() {
            c.bg_color = bg;
        }
    }

    pub fn bg_color(&self) -> CropBgColor {
        self.bg_color
    }

    /// Toolbar W/H text inputs: resize (and recenter) the current
    /// crop rect to the explicit pixel dimensions. The new rect is
    /// centered on the current rect's midpoint when one exists, or
    /// on the image center otherwise — that way typing 800×600 with
    /// no prior crop frames the middle of the screenshot.
    /// Falls back to a no-op if we don't even have image bounds
    /// yet (no screenshot loaded).
    pub fn set_dimensions(&mut self, width: f32, height: f32) {
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        let Some(bounds) = self.image_bounds else {
            return;
        };
        let center = match self.crop.as_ref() {
            Some(c) => {
                let (p, s) = c.get_rectangle();
                Vec2D::new(p.x + s.x / 2.0, p.y + s.y / 2.0)
            }
            None => Vec2D::new(bounds.x / 2.0, bounds.y / 2.0),
        };
        let new_pos = Vec2D::new(center.x - width / 2.0, center.y - height / 2.0);
        let new_size = Vec2D::new(width, height);
        if let Some(c) = self.crop.as_mut() {
            c.pos = new_pos;
            c.size = new_size;
            c.active = true;
        } else {
            self.crop = Some(Crop {
                pos: new_pos,
                size: new_size,
                active: true,
                committed: false,
                ever_committed: false,
                last_committed: None,
                canvas_pos: Vec2D::zero(),
                canvas_size: Vec2D::zero(),
                bg_color: self.bg_color,
            });
            self.emit_crop_presence(true);
        }
        self.emit_crop_edit_dimensions();
        self.refresh_canvas_from_image();
    }

    /// Emit a live "crop rect dimensions" tick for the toolbar's
    /// W/H entries. Distinct from `DimensionsUpdate` — the bottom-
    /// right output-dimensions readout doesn't watch this; it
    /// only updates on commit / revert / un-commit so the readout
    /// always reflects the OUTPUT (full image during edit, cropped
    /// size after commit).
    fn emit_crop_edit_dimensions(&self) {
        if let (Some(crop), Some(sender)) = (&self.crop, &self.sender) {
            let (_pos, size) = crop.get_rectangle();
            let width = size.x.round() as i32;
            let height = size.y.round() as i32;
            sender
                .send(SketchBoardInput::Output(
                    SketchBoardOutput::CropEditDimensions { width, height },
                ))
                .ok();
        }
    }

    /// Emit the bottom-right output-dimensions readout's new
    /// value. `None` resets to full image bounds; `Some((w, h))`
    /// shows a committed crop's dims. Called on commit, revert,
    /// re-enter (un-commit), and `revert_to_seed`.
    fn emit_output_dimensions(&self, dims: Option<(i32, i32)>) {
        if let Some(sender) = &self.sender {
            sender
                .send(SketchBoardInput::Output(
                    SketchBoardOutput::DimensionsUpdate(dims),
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
        // Image→canvas scale snapshot from the live transform; used
        // by `hit_kind` to compute a screen-constant handle hit
        // radius. Without this the radius is a fixed image-pixel
        // tolerance (20 px), which at 22 % auto-fit zoom shrinks
        // to ~4 screen pixels — almost impossible to click on, and
        // the reason the seed-crop's corner handles felt
        // ungrabbable. Falls back to 1.0 before the first render
        // (defensive; in practice the first `set_render_transform`
        // call runs before any user click).
        let scale = self
            .render_transform
            .map(|t| t.effective_scale)
            .unwrap_or(1.0);
        let is_handle = |c: &Crop, pos: Vec2D| {
            matches!(c.hit_kind(pos, scale), Some(CropHit::Handle(_)))
        };
        let handle_of = |c: &Crop, pos: Vec2D| match c.hit_kind(pos, scale) {
            Some(CropHit::Handle(h)) => Some(h),
            _ => None,
        };
        // Ignore clicks that landed outside the image — the crop
        // tool's job is to define a subregion of the source image,
        // so a click past the image edge has no meaningful target.
        // EXCEPT a near-edge click on a handle of an existing crop:
        // dragging a handle inward from just outside the image edge
        // is a legitimate way to grab a handle that's resting right
        // at the edge.
        if let Some(b) = self.image_bounds {
            let outside = pos.x < 0.0 || pos.y < 0.0 || pos.x > b.x || pos.y > b.y;
            if outside {
                let near_handle = self
                    .crop
                    .as_ref()
                    .is_some_and(|c| is_handle(c, pos));
                if !near_handle {
                    return ToolUpdateResult::Unmodified;
                }
            }
        }
        match &self.crop {
            None => {
                // No crop exists, create a new one. In practice
                // `handle_activated` seeds a full-image crop on
                // entry so this branch is only reached if some
                // future flow tears the crop down — kept as a
                // defensive fallback.
                self.crop = Some(Crop::new(pos, self.bg_color));
                self.action = Some(CropToolAction::NewCrop);
            }
            Some(c) => {
                if !c.active {
                    activate = true;
                }
                if let Some(handle) = handle_of(c, pos) {
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
                    // Click landed outside the crop and not near
                    // any handle. Earlier behavior was to start a
                    // fresh 1×1 NewCrop drag from the click point,
                    // which left the user with a tiny throwaway
                    // crop on any stray click outside the active
                    // region. Ignore the click instead — the user
                    // can resize the existing crop's handles or
                    // move it from inside.
                    return ToolUpdateResult::Unmodified;
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
        // Refresh the canvas-coord twin so a brand-new crop has a
        // canvas-fixed anchor as soon as it exists. Existing crops
        // get refreshed too but it's a no-op there — pos/size didn't
        // change in this branch, only `action` did.
        self.refresh_canvas_from_image();
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
        // Clamping to image bounds always runs, independent of the
        // snap-to-edges checkbox / Alt bypass — the crop rect can
        // only ever be a subregion of the source image, so dragging
        // a handle past the image edge clips at the edge instead of
        // extending the crop beyond it.
        let snap_x = move |v: f32| -> f32 {
            let mut v = v;
            if snap_active && let Some(b) = bounds {
                for t in [0.0, b.x] {
                    if (v - t).abs() <= Self::SNAP_PIXELS {
                        v = t;
                        break;
                    }
                }
            }
            if let Some(b) = bounds {
                v = v.clamp(0.0, b.x);
            }
            v
        };
        let snap_y = move |v: f32| -> f32 {
            let mut v = v;
            if snap_active && let Some(b) = bounds {
                for t in [0.0, b.y] {
                    if (v - t).abs() <= Self::SNAP_PIXELS {
                        v = t;
                        break;
                    }
                }
            }
            if let Some(b) = bounds {
                v = v.clamp(0.0, b.y);
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

        let result = match action {
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
                // Clamp the dragged endpoint to image bounds — the
                // aspect-ratio branch may have re-grown the rect past
                // an edge that snap_x/snap_y already pinned.
                if let Some(b) = bounds {
                    let end_x = (crop.pos.x + sx).clamp(0.0, b.x);
                    let end_y = (crop.pos.y + sy).clamp(0.0, b.y);
                    sx = end_x - crop.pos.x;
                    sy = end_y - crop.pos.y;
                }
                crop.size = Vec2D::new(sx, sy);
                self.emit_crop_edit_dimensions();
                ToolUpdateResult::Redraw
            }
            CropToolAction::DragHandle(state) => {
                Self::apply_drag_handle_transformation(
                    crop, state, direction, aspect, bounds, snap_x, snap_y,
                );
                self.emit_crop_edit_dimensions();
                ToolUpdateResult::Redraw
            }
            CropToolAction::Move(state) => {
                // Move: snap whichever edge of the crop is closest to
                // an image edge. Try the leading (top/left) edge first;
                // if neither it nor the trailing edge wants to snap,
                // the crop moves freely. Keeps the user's chosen size
                // intact (we only translate, never resize on Move).
                // After snap-or-passthrough, clamp the final position
                // so the ENTIRE crop stays inside the image bounds —
                // not just one edge — so a drag well past an edge
                // pins the crop flush against that edge instead of
                // letting half of it slide off the image.
                let new_pos = state.start + direction;
                let final_x = {
                    let left = snap_x(new_pos.x);
                    let candidate = if left != new_pos.x {
                        left
                    } else {
                        let right = snap_x(new_pos.x + crop.size.x);
                        if right != new_pos.x + crop.size.x {
                            right - crop.size.x
                        } else {
                            new_pos.x
                        }
                    };
                    if let Some(b) = bounds {
                        let max_x = (b.x - crop.size.x).max(0.0);
                        candidate.clamp(0.0, max_x)
                    } else {
                        candidate
                    }
                };
                let final_y = {
                    let top = snap_y(new_pos.y);
                    let candidate = if top != new_pos.y {
                        top
                    } else {
                        let bottom = snap_y(new_pos.y + crop.size.y);
                        if bottom != new_pos.y + crop.size.y {
                            bottom - crop.size.y
                        } else {
                            new_pos.y
                        }
                    };
                    if let Some(b) = bounds {
                        let max_y = (b.y - crop.size.y).max(0.0);
                        candidate.clamp(0.0, max_y)
                    } else {
                        candidate
                    }
                };
                crop.pos = Vec2D::new(final_x, final_y);
                ToolUpdateResult::Redraw
            }
        };
        // Refresh the canvas-coord twin so the inside-out workflow's
        // canvas-fixed anchor reflects the just-applied image-coord
        // mutation. Cheap (a few multiplies); runs on every drag tick.
        self.refresh_canvas_from_image();
        result
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
        // Arrow keys. Without Ctrl the crop MOVES; plain = 30 px
        // (the fast default — most edit gestures want big steps),
        // Shift = 1 px (the precision opt-in). With Ctrl the crop
        // RESIZES symmetrically (both opposite edges move so the
        // center holds steady); Ctrl alone = 30 px / 15 per edge,
        // Ctrl+Shift = 5 px / 2.5 per edge for fine-tuning.
        // Holding two arrows combines them into one diagonal step
        // per event (e.g. holding Up+Right moves up-right) rather
        // than alternating axis-aligned steps as the auto-repeats
        // interleave. The held-arrow bitmask is updated on press
        // and cleared on release / tool-deactivate.
        // RedrawAndStopPropagation in both branches keeps the
        // global key chain (Alt+arrow pans the canvas) from also
        // firing on the same event.
        let ctrl = event.modifier.contains(ModifierType::CONTROL_MASK);
        let shift = event.modifier.contains(ModifierType::SHIFT_MASK);
        let other_mods = event.modifier
            & !(ModifierType::SHIFT_MASK | ModifierType::CONTROL_MASK);
        if other_mods.is_empty()
            && let Some(bit) = arrow_bit(event.key)
        {
            self.held_arrows |= bit;
            return if ctrl {
                let step = if shift { 5.0 } else { 30.0 };
                let (dw, dh) = resize_delta(self.held_arrows, step);
                if self.resize_symmetric(dw, dh) {
                    ToolUpdateResult::RedrawAndStopPropagation
                } else {
                    ToolUpdateResult::StopPropagation
                }
            } else {
                let step = if shift { 1.0 } else { 30.0 };
                let (dx, dy) = move_delta(self.held_arrows, step);
                if self.nudge(dx, dy) {
                    ToolUpdateResult::RedrawAndStopPropagation
                } else {
                    // Even a no-op nudge consumes the key — without
                    // StopPropagation an at-edge arrow would fall
                    // through to the global handler and pan the
                    // canvas, which isn't what the user wants when
                    // they meant "move the crop".
                    ToolUpdateResult::StopPropagation
                }
            };
        }
        match event.key {
            Key::Escape => self.cancel(),
            Key::Return | Key::KP_Enter if self.crop.is_some() => self.commit(),
            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn handle_key_release_event(&mut self, event: KeyEventMsg) -> ToolUpdateResult {
        if let Some(bit) = arrow_bit(event.key) {
            self.held_arrows &= !bit;
        }
        ToolUpdateResult::Unmodified
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
            // window back up to fit it, and reset the bottom-right
            // output-dims readout back to "full image" (it had been
            // showing the committed crop's dims).
            if was_committed {
                if let Some(bounds) = self.image_bounds {
                    self.emit_content_size(bounds.x, bounds.y);
                }
                self.emit_output_dimensions(None);
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
                canvas_pos: Vec2D::zero(),
                canvas_size: Vec2D::zero(),
                bg_color: self.bg_color,
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
        // Drop any in-flight held-arrow state so re-entering the
        // tool starts fresh — otherwise a held key whose release
        // we missed (e.g. user Alt-tabbed out mid-press) would
        // shadow the next keyboard gesture.
        self.held_arrows = 0;
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
