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
    Canvas, CompositeOperation, FontId, ImageFlags, ImageId, ImageSource, Paint, Path, PixelFormat,
    RenderTarget, Transform2D,
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

/// Breathing room (in CSS px) around the rendered screenshot inside
/// the canvas. Gives the image visual separation from the toolbars
/// and lets the drop shadow fall outside the image edges. Scaled to
/// canvas pixels at render time via the device pixel ratio. Sized to
/// fully contain the `SHADOW_KEY_*` extent below so the wide key
/// shadow doesn't get clipped at the canvas edge.
const CANVAS_PADDING_CSS: f32 = 60.0;
/// Lowest auto-fit zoom a vertical window resize will shrink the
/// image to. Enforced two ways so it holds under any window manager:
/// `apply_vertical_resize_floor` pins a minimum height on `outer_box`
/// so a *floating* window can't be dragged shorter, and
/// `auto_fit_scale` floors the vertical fit term at render time so a
/// *tiled* window — whose compositor ignores that min-size request —
/// clips the image instead of squeezing it past this zoom.
const MIN_AUTO_FIT_ZOOM: f32 = 0.10;
/// Ambient ("contact") shadow: a tight, even halo right at the image
/// edge. No vertical offset — sells the "the image is sitting on the
/// surface" half of the macOS shadow model.
const SHADOW_AMBIENT_BLUR_CSS: f32 = 12.0;
const SHADOW_AMBIENT_ALPHA: f32 = 0.25;
/// Key ("elevation") shadow: wider, offset downward. This is the
/// layer that actually reads as "the window is floating above the
/// desktop" — the look macOS uses. Layered on top of the
/// ambient so the two combine into a soft, asymmetric shadow that
/// pools more below than above.
const SHADOW_KEY_BLUR_CSS: f32 = 40.0;
const SHADOW_KEY_OFFSET_Y_CSS: f32 = 14.0;
const SHADOW_KEY_ALPHA: f32 = 0.45;
/// Maximum overshoot (CSS px) the rubber-band rendering will display
/// when the user pans past the image edge. The hyperbolic damping
/// in `rubber_band` asymptotes to this value, so it's the "elastic
/// stretch" budget.
const RUBBER_BAND_MAX_OVERSHOOT_CSS: f32 = 100.0;
/// Hyperbolic-damping constant. Apple's reference value is 0.55;
/// lower numbers add more resistance (1 px of drag produces less
/// visible motion at the limit), higher numbers feel loose. 0.30
/// gives the canvas a heavier, more deliberate feel when you push
/// past the edge — less of a flick, more of a tug.
const RUBBER_BAND_RESISTANCE: f32 = 0.30;
/// How long the user must be idle before the spring-back animation
/// kicks in. Short enough that releasing your fingers feels snappy,
/// long enough that mid-gesture pauses don't trigger a jarring
/// retreat.
const SPRING_BACK_IDLE_MS: u128 = 40;
/// Tick interval for the spring-back timer. ~60 fps so the recovery
/// looks fluid on standard refresh rates.
pub const SPRING_BACK_TICK_MS: u64 = 16;
/// Natural angular frequency of the spring-back animation
/// (`ω = sqrt(k/m)` with m=1). Settling time for a critically
/// damped spring is roughly `4/ω`. ω=18 ≈ 220 ms settling — a hair
/// faster than macOS' overscroll recovery so the long exponential
/// tail doesn't drag on. Raise it for snappier, lower for softer.
const SPRING_BACK_OMEGA: f32 = 18.0;
/// Position-snap threshold (image-space px). Past this proximity to
/// the limit, force `drag_offset` to exactly the limit so we stop
/// drifting on subpixel residuals from the long exponential tail.
const SPRING_BACK_SNAP_EPS: f32 = 0.5;
/// Fraction of the initial displacement that triggers the snap
/// (combined with `SPRING_BACK_SNAP_EPS` so we stop the timer once
/// we're effectively at the target rather than chasing the
/// asymptotic tail forever).
const SPRING_BACK_DONE_FRACTION: f32 = 0.004;
/// Hyperbolic rubber-band damping. Returns the rendered offset for a
/// given raw `value`: untouched while within `±limit`, then damped
/// past the limit so the visible offset asymptotes at `limit +
/// max_overshoot`. Matches the curve used by UIScrollView's elastic
/// scrolling — the further the user pulls, the more resistance.
fn rubber_band(value: f32, limit: f32, max_overshoot: f32) -> f32 {
    if value.abs() <= limit || max_overshoot <= 0.0 {
        return value;
    }
    let sign = value.signum();
    let beyond = value.abs() - limit;
    let damped =
        max_overshoot * (1.0 - 1.0 / (1.0 + beyond * RUBBER_BAND_RESISTANCE / max_overshoot));
    sign * (limit + damped)
}

/// Inverse of `rubber_band`: given the desired visible offset, return
/// the `drag_offset` value that would produce it. Used by the
/// spring-back animation so we can drive the VISIBLE offset on a
/// smooth curve and let the renderer's rubber-band map handle the
/// rest — animating `drag_offset` directly through this non-linear
/// map produced the "stuck then snap" feel (most of the curve was
/// spent near the asymptote where visible motion barely changes).
fn inverse_rubber_band(visible: f32, limit: f32, max_overshoot: f32) -> f32 {
    if visible.abs() <= limit || max_overshoot <= 0.0 || RUBBER_BAND_RESISTANCE <= 0.0 {
        return visible;
    }
    let sign = visible.signum();
    let v_over = (visible.abs() - limit).min(max_overshoot - 0.001);
    let drag_over = max_overshoot * v_over / (RUBBER_BAND_RESISTANCE * (max_overshoot - v_over));
    sign * (limit + drag_over)
}

/// Closed-form critically damped spring response. Returns the
/// fraction of the initial displacement that REMAINS at time `t`
/// (seconds). At `t = 0` the value is `1` (no movement yet); as `t`
/// grows the value approaches `0` (fully recovered). The curve has
/// zero slope at `t = 0` (gentle start), accelerates quickly, then
/// decelerates into the target via a long exponential tail — same
/// shape UIScrollView uses for overscroll release.
fn critically_damped_remaining(t: f32) -> f32 {
    let wt = SPRING_BACK_OMEGA * t;
    (1.0 + wt) * (-wt).exp()
}

/// Compute the spring-back position for a single axis given the
/// animation's start position and elapsed time. `start` is where
/// `drag_offset` sat the moment the animation began; the target is
/// the nearest hard limit (or `start` itself if already inside).
/// Returns `(new_value, done)` where `done` is true once we're
/// either snapped to the limit or the spring has decayed enough
/// that the residual is invisible.
fn spring_back_progress(start: f32, limit: f32, elapsed_ms: f32) -> (f32, bool) {
    if start.abs() <= limit {
        return (start, true);
    }
    let target = if start > 0.0 { limit } else { -limit };
    let t = elapsed_ms / 1000.0;
    let remaining = critically_damped_remaining(t);
    let value = target + (start - target) * remaining;
    let snapped =
        remaining < SPRING_BACK_DONE_FRACTION || (value - target).abs() < SPRING_BACK_SNAP_EPS;
    if snapped {
        (target, true)
    } else {
        (value, false)
    }
}

/// How many pixels deep to sample inward from each edge when picking
/// the auto-extend strip color. Each side gets ONE solid color
/// (mode-quantized average over the whole sample area) so we don't
/// get the "stretched-pixel-row" stripe artifacts that per-row
/// averaging produced over text-bearing edges.
const AUTO_EXTEND_EDGE_SAMPLE_DEPTH: i32 = 8;

/// Build a new Pixbuf representing the rectangle `(src_x, src_y,
/// new_w, new_h)` taken out of `original`'s coordinate space. Where
/// that rect lies inside `original`, the pixels are copied directly;
/// where it lies outside (negative src or past edge), the new strip
/// is painted with the dominant color of the corresponding `original`
/// edge — preserves the screenshot's edge color when growing.
/// Handles pure grow, pure shrink, and any mix (e.g. grow-left while
/// shrink-right in the same operation). Returns `None` if the new
/// Pixbuf can't be allocated or `new_w`/`new_h` are non-positive.
fn resize_pixbuf_to_rect(
    original: &Pixbuf,
    src_x: i32,
    src_y: i32,
    new_w: i32,
    new_h: i32,
) -> Option<Pixbuf> {
    if new_w <= 0 || new_h <= 0 {
        return None;
    }
    let orig_w = original.width();
    let orig_h = original.height();
    let new = Pixbuf::new(
        original.colorspace(),
        original.has_alpha(),
        original.bits_per_sample(),
        new_w,
        new_h,
    )?;
    new.fill(0x000000ff);
    let depth = AUTO_EXTEND_EDGE_SAMPLE_DEPTH.min(orig_w).min(orig_h).max(1);
    let has_alpha = original.has_alpha();
    let left_color = dominant_color(original, 0, 0, depth, orig_h, has_alpha);
    let right_color = dominant_color(original, orig_w - depth, 0, depth, orig_h, has_alpha);
    let top_color = dominant_color(original, 0, 0, orig_w, depth, has_alpha);
    let bottom_color = dominant_color(original, 0, orig_h - depth, orig_w, depth, has_alpha);

    // Grow amounts on each side (0 when that side is shrinking or
    // unchanged). These delineate the strips of `new` whose source
    // would be outside `original` and so need an edge-color fill.
    let grow_left = (-src_x).max(0);
    let grow_top = (-src_y).max(0);
    let grow_right = ((src_x + new_w) - orig_w).max(0);
    let grow_bottom = ((src_y + new_h) - orig_h).max(0);

    if grow_left > 0 {
        fill_rect(
            &new,
            0,
            grow_top,
            grow_left,
            new_h - grow_top - grow_bottom,
            left_color,
        );
    }
    if grow_right > 0 {
        fill_rect(
            &new,
            new_w - grow_right,
            grow_top,
            grow_right,
            new_h - grow_top - grow_bottom,
            right_color,
        );
    }
    if grow_top > 0 {
        fill_rect(
            &new,
            grow_left,
            0,
            new_w - grow_left - grow_right,
            grow_top,
            top_color,
        );
    }
    if grow_bottom > 0 {
        fill_rect(
            &new,
            grow_left,
            new_h - grow_bottom,
            new_w - grow_left - grow_right,
            grow_bottom,
            bottom_color,
        );
    }
    // Corners (where both axes grew). Pick the adjacent edge color
    // matching the longer of the two strips so we get a continuous
    // band along the dominant direction.
    if grow_top > 0 && grow_left > 0 {
        let c = if grow_top >= grow_left {
            top_color
        } else {
            left_color
        };
        fill_rect(&new, 0, 0, grow_left, grow_top, c);
    }
    if grow_top > 0 && grow_right > 0 {
        let c = if grow_top >= grow_right {
            top_color
        } else {
            right_color
        };
        fill_rect(&new, new_w - grow_right, 0, grow_right, grow_top, c);
    }
    if grow_bottom > 0 && grow_left > 0 {
        let c = if grow_bottom >= grow_left {
            bottom_color
        } else {
            left_color
        };
        fill_rect(&new, 0, new_h - grow_bottom, grow_left, grow_bottom, c);
    }
    if grow_bottom > 0 && grow_right > 0 {
        let c = if grow_bottom >= grow_right {
            bottom_color
        } else {
            right_color
        };
        fill_rect(
            &new,
            new_w - grow_right,
            new_h - grow_bottom,
            grow_right,
            grow_bottom,
            c,
        );
    }

    // Copy the intersection of the requested rect with `original`.
    let isec_src_x = src_x.max(0);
    let isec_src_y = src_y.max(0);
    let isec_end_x = (src_x + new_w).min(orig_w);
    let isec_end_y = (src_y + new_h).min(orig_h);
    let isec_w = isec_end_x - isec_src_x;
    let isec_h = isec_end_y - isec_src_y;
    if isec_w > 0 && isec_h > 0 {
        let dst_x = isec_src_x - src_x;
        let dst_y = isec_src_y - src_y;
        original.copy_area(isec_src_x, isec_src_y, isec_w, isec_h, &new, dst_x, dst_y);
    }
    Some(new)
}

fn read_pixel(p: &Pixbuf, x: i32, y: i32, has_alpha: bool) -> (u8, u8, u8, u8) {
    let stride = p.rowstride() as usize;
    let bpp = if has_alpha { 4 } else { 3 };
    let idx = y as usize * stride + x as usize * bpp;
    unsafe {
        let buf = p.pixels();
        let r = buf[idx];
        let g = buf[idx + 1];
        let b = buf[idx + 2];
        let a = if has_alpha { buf[idx + 3] } else { 255 };
        (r, g, b, a)
    }
}

/// Pick the dominant color of a rectangular sample area by 5-bit
/// quantization (32 levels per channel ⇒ ~32 K bins). Builds a
/// histogram, picks the bin with the most samples, then returns the
/// mean of all pixels that landed in that bin (so we don't snap to
/// the quantization grid). This filters out antialiased text
/// pixels at edges — the background dominates the bin count, and
/// the few stray text pixels fall into different bins.
fn dominant_color(
    p: &Pixbuf,
    x0: i32,
    y0: i32,
    w: i32,
    h: i32,
    has_alpha: bool,
) -> (u8, u8, u8, u8) {
    use std::collections::HashMap;
    let mut bins: HashMap<u32, (u32, [u64; 4])> = HashMap::new();
    for y in y0..(y0 + h) {
        for x in x0..(x0 + w) {
            let (r, g, b, a) = read_pixel(p, x, y, has_alpha);
            let key = ((r as u32) >> 3) << 15
                | ((g as u32) >> 3) << 10
                | ((b as u32) >> 3) << 5
                | ((a as u32) >> 3);
            let entry = bins.entry(key).or_insert((0, [0u64; 4]));
            entry.0 += 1;
            entry.1[0] += r as u64;
            entry.1[1] += g as u64;
            entry.1[2] += b as u64;
            entry.1[3] += a as u64;
        }
    }
    let Some((_, top)) = bins.iter().max_by_key(|(_, (count, _))| *count) else {
        return (0, 0, 0, 255);
    };
    let n = top.0.max(1) as u64;
    (
        (top.1[0] / n).min(255) as u8,
        (top.1[1] / n).min(255) as u8,
        (top.1[2] / n).min(255) as u8,
        (top.1[3] / n).min(255) as u8,
    )
}

fn fill_rect(p: &Pixbuf, x: i32, y: i32, w: i32, h: i32, (r, g, b, a): (u8, u8, u8, u8)) {
    for yy in y..(y + h) {
        for xx in x..(x + w) {
            p.put_pixel(xx as u32, yy as u32, r, g, b, a);
        }
    }
}

/// Dark gray fill behind the screenshot (replaces solid black). Matches
/// the surrounding toolbar chrome so the canvas reads as part of the
/// app surface, not a void.
const CANVAS_BG: femtovg::Color = femtovg::Color {
    r: 0x24 as f32 / 255.0,
    g: 0x24 as f32 / 255.0,
    b: 0x24 as f32 / 255.0,
    a: 1.0,
};

#[derive(Default)]
pub struct FemtoVGArea {
    canvas: RefCell<Option<femtovg::Canvas<femtovg::renderer::OpenGl>>>,
    font: RefCell<Option<FontId>>,
    inner: RefCell<Option<FemtoVgAreaMut>>,
    request_render: RefCell<Option<Vec<Action>>>,
    sender: RefCell<Option<Sender<SketchBoardInput>>>,
    /// Last `scale_factor` we emitted to the parent so we can suppress
    /// redundant `ZoomDisplayChanged` notifications during steady-state
    /// frame rendering.
    last_emitted_scale: RefCell<f32>,
    /// Last `PanInfo` we emitted upstream. Stops us forwarding the same
    /// scrollbar-update payload on every `update_transformation` —
    /// without dedup, every spring-back / pinch / scroll tick fired a
    /// fresh PanChanged → sync_scrollbars cycle even when nothing had
    /// actually moved, which showed up as visible UI stutter.
    last_emitted_pan: RefCell<Option<crate::sketch_board::PanInfo>>,
    /// Active spring-back timer source. Started on each pan when the
    /// drag offset is past its hard limit, cleared once the offset
    /// has fully recovered — keeps the timer from running forever
    /// while there's no rubber-band stretch to recover.
    pub spring_back_timer: RefCell<Option<gtk::glib::SourceId>>,
}

pub struct FemtoVgAreaMut {
    background_image: Pixbuf,
    background_image_id: Option<femtovg::ImageId>,
    /// Image-space rect of the original (pre-auto-extension)
    /// screenshot inside the current `background_image`. Initially
    /// `(0, 0, orig_w, orig_h)`. When the canvas auto-extends, the
    /// origin shifts (for left/top extensions) and the size stays
    /// fixed. `auto_resize_for_drawables` uses this rect as the
    /// "must keep visible" floor — it never crops away original
    /// screenshot pixels, even if the user deletes every drawable.
    original_rect: crate::math::Rect,
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
    /// Per-kind monotonic counter for `Stacked::auto_label_index`.
    /// Incremented at every commit, never decremented — so a layer's
    /// ordinal stays stable across reorders (and across delete + redo
    /// chains, which carry the original index in `UndoAction::Remove`).
    next_label_index: std::collections::HashMap<&'static str, u32>,
    zoom_scale: f32,
    last_scale: f32,
    pointer_offset: Vec2D,
    last_offset: Vec2D,
    drag_offset: Vec2D,
    is_drag: bool,
    is_reset: bool,
    /// Set by `set_zoom_scale_at` to tell `update_transformation`
    /// to KEEP the freshly-computed `drag_offset` (which positions
    /// the image so the user's anchor point stays under the cursor
    /// after a zoom). Without this flag, the centering logic at
    /// line ~2003 would zero out drag_offset on the very tick the
    /// zoom takes effect, defeating the anchor.
    zoom_anchor_pending: bool,
    /// Device pixel ratio of the host display (1 on standard DPI, 2 on
    /// retina). Updated on `resize`. Used so per-frame UI elements
    /// (selection handles) can render at constant CSS-pixel size while
    /// still looking sharp on HiDPI screens.
    device_pixel_ratio: f32,
    /// Global darkness for the spotlight overlay (0.10–0.90, slider
    /// range). Sketch_board pushes the toolbar slider value here on
    /// every change; the renderer's spotlight pass reads it directly
    /// at render time so the overlay updates live without redrawing
    /// each spotlight Drawable.
    spotlight_darkness: f32,
    /// Scale + offset actually used by the most recent on-screen
    /// render. Equal to `scale_factor` / `offset` in the normal case,
    /// but switches to a "fit the committed crop into the canvas"
    /// transform whenever a committed crop is present. Coordinate
    /// conversions read these so a click on the zoomed crop lands at
    /// the right image-space position.
    effective_scale: f32,
    effective_offset: Vec2D,
    /// Canvas-pixel rect of the visible content (full image in the
    /// regular view; the cropped region in committed-crop mode).
    /// Captured during `render_framebuffer` so the drop-shadow path
    /// in `render` can draw a shadow around whichever rect is
    /// actually on screen without re-deriving it.
    display_rect_origin: Vec2D,
    display_rect_size: Vec2D,
    /// Canvas pixel dimensions captured at the last
    /// `update_transformation` call. Used by
    /// `set_pan_from_scrollbar` to translate a scrollbar adjustment
    /// value (which is expressed in canvas pixels) into the
    /// renderer's centered `drag_offset` representation without
    /// having to thread the canvas reference all the way down.
    last_canvas_size: Vec2D,
    /// User-applied zoom multiplier ON TOP of the committed-crop's
    /// fit-to-canvas scale. 1.0 = exactly fit, 2.0 = 2× the fit.
    /// Lives separately from `zoom_scale` because committed crop has
    /// its own base scale (the fit) and we want wheel-zoom inputs to
    /// scale that base, not the underlying image's full-resolution
    /// scale. Reset to 1.0 whenever the crop is dropped.
    crop_zoom: f32,
    /// Timestamp of the most recent pan input (wheel scroll or
    /// trackpad swipe). `update_transformation` only applies the
    /// spring-back lerp when this is older than `SPRING_BACK_IDLE_MS`
    /// — otherwise we'd fight the user's active gesture.
    last_pan_input: std::time::Instant,
    /// In-flight spring-back animation, if any. `Some((start_time,
    /// start_visible))` once the user has been idle long enough that
    /// we start easing the canvas back to the limit; cleared on
    /// `pan_by` and when the animation completes. `start_visible` is
    /// the **rendered** offset at release (after rubber-band), not
    /// the raw `drag_offset` — we animate that smoothly to the
    /// nearest limit and back-solve a `drag_offset` per frame via
    /// `inverse_rubber_band`. Animating the raw offset directly was
    /// where the "stuck-then-snap" recovery came from: the
    /// nonlinear rubber-band map ate most of the visible motion in
    /// the first half of the curve, then released it in the second.
    spring_back_anim: Option<(std::time::Instant, Vec2D)>,
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

        // update scale factor + pan; capture the snapshot we need
        // for the upstream notifications BEFORE releasing the inner
        // borrow so the emit paths don't have to re-acquire it.
        // `effective_scale` is what the indicator should show — for
        // committed-crop mode it's `crop_zoom`, for the regular view
        // it equals `scale_factor`. update_transformation keeps it in
        // sync now, so one emit covers both paths.
        let (eff_scale, pan_info, min_canvas_h) = {
            let mut inner_ref = self.inner();
            let inner = inner_ref
                .as_mut()
                .expect("Did you call init before using FemtoVgArea?");
            inner.device_pixel_ratio = dpr;
            inner.update_transformation(canvas);
            // Keep the crop tool's cached image→canvas scale fresh so
            // its handle hit-testing stays screen-constant as the user
            // zooms.
            let eff_scale_for_crop = inner.effective_scale;
            let crop_tool = inner.crop_tool.clone();
            {
                let mut ct = crop_tool.borrow_mut();
                if ct.is_active_edit() {
                    ct.set_render_scale(eff_scale_for_crop);
                }
            }
            let image_w = inner.background_image.width() as f32;
            let image_h = inner.background_image.height() as f32;
            let pan_info = crate::sketch_board::PanInfo {
                drag_x: inner.drag_offset.x,
                drag_y: inner.drag_offset.y,
                image_w_scaled: image_w * inner.scale_factor,
                image_h_scaled: image_h * inner.scale_factor,
                canvas_w: canvas.width() as f32,
                canvas_h: canvas.height() as f32,
            };
            (
                inner.effective_scale,
                pan_info,
                inner.min_canvas_height_logical(),
            )
        };
        self.notify_zoom_display(eff_scale);
        self.notify_pan_display(pan_info);
        self.apply_vertical_resize_floor(min_canvas_h);
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
    /// Forward a `ZoomDisplayChanged` event to the parent component when
    /// the rendered scale factor changes. Idempotent: skips emission when
    /// the value matches what we sent last time.
    /// Pin the window's content (`outer_box`) to a minimum height so
    /// a vertical resize can't shrink the image past
    /// `MIN_AUTO_FIT_ZOOM`. The floor is `min_canvas_h` (the canvas
    /// height for that zoom) plus the *measured* chrome — the live
    /// `outer_box` height minus this canvas's height — so it stays
    /// correct no matter how tall the toolbars currently are (the top
    /// bar's height changes when it wraps). Setting the request on
    /// `outer_box` rather than the window keeps it clear of the
    /// launch-time `set_size_request` size-pinning, which targets the
    /// window itself.
    fn apply_vertical_resize_floor(&self, min_canvas_h: f32) {
        let canvas = self.obj();
        let mut node = canvas.parent();
        let outer = loop {
            match node {
                Some(w) if w.has_css_class("outer_box") => break Some(w),
                Some(w) => node = w.parent(),
                None => break None,
            }
        };
        let Some(outer) = outer else { return };
        let chrome = (outer.height() - canvas.height()).max(0);
        let floor = min_canvas_h.ceil() as i32 + chrome;
        if outer.height_request() != floor {
            outer.set_size_request(outer.width_request(), floor);
        }
    }

    fn notify_zoom_display(&self, scale_factor: f32) {
        let mut last = self.last_emitted_scale.borrow_mut();
        if (*last - scale_factor).abs() > 0.0005 {
            *last = scale_factor;
            if let Some(sender) = self.sender.borrow().as_ref() {
                sender.emit(SketchBoardInput::ZoomDisplayChanged(scale_factor));
            }
        }
    }

    /// Forward a `PanDisplayChanged` event so the App's scrollbars
    /// can sync their visibility + values. Deduped against the last
    /// emitted value — `update_transformation` runs on every render
    /// tick (including animation timers), and forwarding identical
    /// PanInfo through SketchBoard → App → sync_scrollbars on each
    /// tick was producing measurable UI lag on every relayout.
    fn notify_pan_display(&self, info: crate::sketch_board::PanInfo) {
        {
            let mut last = self.last_emitted_pan.borrow_mut();
            if last.as_ref() == Some(&info) {
                return;
            }
            *last = Some(info);
        }
        if let Some(sender) = self.sender.borrow().as_ref() {
            sender.emit(SketchBoardInput::PanDisplayChanged(info));
        }
    }

    pub fn init(
        &self,
        sender: Sender<SketchBoardInput>,
        crop_tool: Rc<RefCell<CropTool>>,
        active_tool: Rc<RefCell<dyn Tool>>,
        pointer_tool: Rc<RefCell<dyn Tool>>,
        background_image: Pixbuf,
    ) {
        let original_rect = crate::math::Rect::new(
            Vec2D::zero(),
            Vec2D::new(
                background_image.width() as f32,
                background_image.height() as f32,
            ),
        );
        self.inner().replace(FemtoVgAreaMut {
            background_image,
            background_image_id: None,
            original_rect,
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
            next_label_index: std::collections::HashMap::new(),
            zoom_scale: 0.0,
            pointer_offset: Vec2D::zero(),
            last_offset: Vec2D::zero(),
            drag_offset: Vec2D::zero(),
            last_scale: 0.0,
            is_drag: false,
            is_reset: false,
            zoom_anchor_pending: false,
            device_pixel_ratio: 1.0,
            spotlight_darkness: 0.50,
            effective_scale: 1.0,
            effective_offset: Vec2D::zero(),
            display_rect_origin: Vec2D::zero(),
            display_rect_size: Vec2D::zero(),
            last_canvas_size: Vec2D::zero(),
            crop_zoom: 1.0,
            last_pan_input: std::time::Instant::now(),
            spring_back_anim: None,
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

        // Prefer the user-configured font ONLY when they've explicitly
        // set `font.family`. With no override we skip straight to the
        // bundled Inter Display SemiBold below — the previous code's
        // `unwrap_or("")` flow let fontconfig substitute whatever system
        // default it picked (often a generic sans-serif that looked
        // visually unrelated), defeating the point of bundling a
        // font.
        if let Some(family) = app_config.font().family() {
            match load_font(family, app_config.font().style()) {
                Ok(id) => {
                    loaded_fonts.push(id);
                }
                Err(e) => {
                    eprintln!("Primary font: {}", e);
                }
            }
        }

        if loaded_fonts.is_empty() {
            // Bundled Inter Display SemiBold — a clean sans-serif that
            // reads well at small annotation-label sizes. Ships in
            // `src/assets/`, license at `Inter-LICENSE.txt`.
            let fallback = text_context
                .add_font_mem(&resource!("src/assets/InterDisplay-SemiBold.ttf"))
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

/// Auto-fit scale that fits `content` (device px) inside the padded
/// `inner` area, capped at 1:1. The *vertical* fit term is floored at
/// `MIN_AUTO_FIT_ZOOM`: shrinking the window's height — including a
/// tiling-WM resize that ignores our `outer_box` min-size request —
/// can't squeeze the image past that zoom; the canvas clips it
/// instead. The horizontal term is left unfloored, matching the
/// height-only `min_canvas_height_logical` / size-request floor.
fn auto_fit_scale(inner_w: f32, inner_h: f32, content_w: f32, content_h: f32) -> f32 {
    let fit_h = (inner_h / content_h).max(MIN_AUTO_FIT_ZOOM);
    (inner_w / content_w).min(fit_h).min(1.0)
}

impl FemtoVgAreaMut {
    pub fn commit(&mut self, drawable: Box<dyn Drawable>) -> DrawableId {
        let id = DrawableId(self.next_drawable_id);
        self.next_drawable_id += 1;
        // Assign + bump the per-kind ordinal. Indices start at 1 so the
        // first rectangle reads as "Rectangle 1" not "Rectangle 0".
        let kind = drawable.kind_label();
        let counter = self.next_label_index.entry(kind).or_insert(1);
        let label_index = *counter;
        *counter += 1;
        self.drawables.push(Stacked::new(id, drawable, label_index));
        self.undo_stack.push(UndoAction::Add(id));
        self.redo_stack.clear();
        id
    }

    /// After a drawable mutation, re-fit the canvas so it tightly
    /// contains `original_rect` (the un-extended screenshot) plus the
    /// union of all current drawable bounds. Grows the background
    /// Pixbuf (with dominant-color edge fill for new strips) when a
    /// drawable spills past the current image, and shrinks it back
    /// toward `original_rect` when no drawable still needs the
    /// previously-added strips. Translates all drawables EXCEPT those
    /// in `ids_to_exclude` by the resulting shift, and wraps the most
    /// recent undo entry with a `ResizeCanvas` action inside a `Batch`
    /// so one Ctrl+Z reverses both. The excluded ids are the
    /// drawables whose just-pushed Add/Modify/Remove carries
    /// pre-resize state (translating them would double-apply on
    /// redo). Returns the new `(width, height)` if a resize happened,
    /// else `None`.
    pub fn auto_resize_for_drawables(
        &mut self,
        ids_to_exclude: &[DrawableId],
    ) -> Option<(f32, f32)> {
        if self.undo_stack.is_empty() {
            return None;
        }
        // Tight rect we want the new image to cover, in CURRENT image
        // coordinates. Always includes the original screenshot rect
        // (we never crop into the user's actual screenshot pixels).
        let mut tight = self.original_rect;
        for s in &self.drawables {
            if let Some(b) = s.drawable.bounds() {
                tight = tight.union(b);
            }
        }
        let cur_w = self.background_image.width() as f32;
        let cur_h = self.background_image.height() as f32;
        let dx_min = tight.pos.x.floor() as i32;
        let dy_min = tight.pos.y.floor() as i32;
        let dx_max = (tight.pos.x + tight.size.x).ceil() as i32;
        let dy_max = (tight.pos.y + tight.size.y).ceil() as i32;
        if dx_min == 0 && dy_min == 0 && dx_max == cur_w as i32 && dy_max == cur_h as i32 {
            return None;
        }
        let new_w = dx_max - dx_min;
        let new_h = dy_max - dy_min;
        if new_w <= 0 || new_h <= 0 {
            return None;
        }
        let prev_image = self.background_image.clone();
        let resized = resize_pixbuf_to_rect(&self.background_image, dx_min, dy_min, new_w, new_h)?;
        let translation = Vec2D::new(-dx_min as f32, -dy_min as f32);
        self.original_rect.pos += translation;
        let exclude: HashSet<DrawableId> = ids_to_exclude.iter().copied().collect();
        let mut translated_ids: Vec<DrawableId> = Vec::new();
        for s in &mut self.drawables {
            s.drawable.translate(translation);
            if !exclude.contains(&s.id) {
                translated_ids.push(s.id);
            }
        }
        self.background_image = resized;
        self.background_image_id = None;

        let resize = UndoAction::ResizeCanvas {
            prev_image,
            applied_offset: translation,
            translated_ids,
        };
        let prior = self
            .undo_stack
            .pop()
            .expect("auto_resize called with empty undo stack");
        self.undo_stack.push(UndoAction::Batch(vec![resize, prior]));
        Some((new_w as f32, new_h as f32))
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
            visible: stacked.visible,
            locked: stacked.locked,
            custom_name: stacked.custom_name,
            auto_label_index: stacked.auto_label_index,
        });
        self.redo_stack.clear();
        true
    }

    /// Replace the drawable with `id` in-place, folding the change into
    /// the top of the undo stack when that top is already a `Modify`
    /// for the same id. The "first" prev (i.e. the state before the
    /// burst started) is preserved, so a single Ctrl+Z reverses the
    /// whole burst. Falls back to `modify` when the top doesn't match
    /// — e.g. an unrelated action slipped in between.
    pub fn modify_coalesce(&mut self, id: DrawableId, new: Box<dyn Drawable>) -> bool {
        let Some(pos) = self.drawables.iter().position(|s| s.id == id) else {
            return false;
        };
        let top_matches = matches!(
            self.undo_stack.last(),
            Some(UndoAction::Modify { id: top_id, .. }) if *top_id == id
        );
        if top_matches {
            // Keep the existing Modify's `prev` (the burst's original
            // state) and just swap the live drawable forward.
            self.drawables[pos].drawable = new;
            true
        } else {
            self.modify(id, new)
        }
    }

    /// Multi-select counterpart of `modify_coalesce`. Coalesces only
    /// when the top undo entry is a `Batch` whose contained `Modify`
    /// ids match the requested update set exactly.
    pub fn modify_many_coalesce(&mut self, updates: Vec<(DrawableId, Box<dyn Drawable>)>) -> bool {
        let top_matches = if let Some(UndoAction::Batch(actions)) = self.undo_stack.last() {
            let top_ids: Vec<DrawableId> = actions
                .iter()
                .filter_map(|a| {
                    if let UndoAction::Modify { id, .. } = a {
                        Some(*id)
                    } else {
                        None
                    }
                })
                .collect();
            top_ids.len() == actions.len()
                && top_ids.len() == updates.len()
                && updates.iter().all(|(id, _)| top_ids.contains(id))
        } else {
            false
        };
        if !top_matches {
            return self.modify_many(updates);
        }
        for (id, new) in updates {
            if let Some(pos) = self.drawables.iter().position(|s| s.id == id) {
                self.drawables[pos].drawable = new;
            }
        }
        true
    }

    /// Move the drawable with `id` to the top of the stack. Records a
    /// `Reorder` undo entry; if the previous undo entry is already a
    /// `Reorder` for the same id, the older entry's `prev_order` is reused
    /// and the new entry replaces it — so a chain of consecutive raises of
    /// one shape unwinds in a single Ctrl+Z.
    ///
    /// Returns true if anything moved. No-ops (already topmost, missing id)
    /// don't touch undo state.
    pub fn reorder_to_top_coalesce(&mut self, id: DrawableId) -> bool {
        let Some(pos) = self.drawables.iter().position(|s| s.id == id) else {
            return false;
        };
        if pos + 1 == self.drawables.len() {
            return false;
        }
        let mut snapshot: Vec<DrawableId> = self.drawables.iter().map(|s| s.id).collect();
        let stacked = self.drawables.remove(pos);
        self.drawables.push(stacked);

        let coalesce_with_prior = matches!(
            self.undo_stack.last(),
            Some(UndoAction::Reorder { last_raised: Some(prev_id), .. }) if *prev_id == id
        );
        if coalesce_with_prior
            && let Some(UndoAction::Reorder { prev_order, .. }) = self.undo_stack.pop()
        {
            snapshot = prev_order;
        }
        self.undo_stack.push(UndoAction::Reorder {
            prev_order: snapshot,
            last_raised: Some(id),
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
                visible: stacked.visible,
                locked: stacked.locked,
                custom_name: stacked.custom_name,
                auto_label_index: stacked.auto_label_index,
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
            .filter(|s| s.visible && !s.locked)
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

    /// Per-instance UI state for a drawable. `None` if `id` isn't in the
    /// stack. Both fields default to (visible=true, locked=false) at
    /// commit time and are persisted across undo/redo via `Remove` and
    /// `SetLayerFlags` action variants.
    pub fn drawable_flags(&self, id: DrawableId) -> Option<(bool, bool)> {
        self.drawables
            .iter()
            .find(|s| s.id == id)
            .map(|s| (s.visible, s.locked))
    }

    pub fn drawable_custom_name(&self, id: DrawableId) -> Option<String> {
        self.drawables
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.custom_name.clone())
    }

    /// Auto-label ordinal assigned at commit. Stable across reorders so
    /// the layer panel can show "Rectangle 3" regardless of where the
    /// row currently sits in the panel. `None` if `id` isn't in the
    /// stack.
    pub fn drawable_auto_label_index(&self, id: DrawableId) -> Option<u32> {
        self.drawables
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.auto_label_index)
    }

    /// Set or clear the custom panel name for `id`. Records a `Rename`
    /// undo entry; no-op when the new value matches the current one.
    pub fn set_drawable_custom_name(&mut self, id: DrawableId, name: Option<String>) -> bool {
        let Some(pos) = self.drawables.iter().position(|s| s.id == id) else {
            return false;
        };
        if self.drawables[pos].custom_name == name {
            return false;
        }
        let prev = self.drawables[pos].custom_name.take();
        self.drawables[pos].custom_name = name;
        self.undo_stack.push(UndoAction::Rename { id, prev });
        self.redo_stack.clear();
        true
    }

    /// Set the visible+locked flags for `id`, recording a `SetLayerFlags`
    /// undo entry when anything actually changes. Returns true on apply.
    pub fn set_drawable_flags(&mut self, id: DrawableId, visible: bool, locked: bool) -> bool {
        let Some(pos) = self.drawables.iter().position(|s| s.id == id) else {
            return false;
        };
        let prev_visible = self.drawables[pos].visible;
        let prev_locked = self.drawables[pos].locked;
        if prev_visible == visible && prev_locked == locked {
            return false;
        }
        self.drawables[pos].visible = visible;
        self.drawables[pos].locked = locked;
        self.undo_stack.push(UndoAction::SetLayerFlags {
            id,
            prev_visible,
            prev_locked,
        });
        self.redo_stack.clear();
        true
    }

    /// Move `id` one position toward the top of the stack (forward in the
    /// Vec). Records a non-coalescing `Reorder` undo entry. Returns true
    /// on apply; false if `id` is missing or already at the top.
    pub fn move_drawable_up(&mut self, id: DrawableId) -> bool {
        let Some(pos) = self.drawables.iter().position(|s| s.id == id) else {
            return false;
        };
        if pos + 1 == self.drawables.len() {
            return false;
        }
        let snapshot: Vec<DrawableId> = self.drawables.iter().map(|s| s.id).collect();
        self.drawables.swap(pos, pos + 1);
        self.undo_stack.push(UndoAction::Reorder {
            prev_order: snapshot,
            last_raised: None,
        });
        self.redo_stack.clear();
        true
    }

    /// Move `id` one position toward the bottom of the stack.
    pub fn move_drawable_down(&mut self, id: DrawableId) -> bool {
        let Some(pos) = self.drawables.iter().position(|s| s.id == id) else {
            return false;
        };
        if pos == 0 {
            return false;
        }
        let snapshot: Vec<DrawableId> = self.drawables.iter().map(|s| s.id).collect();
        self.drawables.swap(pos, pos - 1);
        self.undo_stack.push(UndoAction::Reorder {
            prev_order: snapshot,
            last_raised: None,
        });
        self.redo_stack.clear();
        true
    }

    /// Send `id` all the way to the bottom of the stack.
    pub fn move_drawable_to_bottom(&mut self, id: DrawableId) -> bool {
        let Some(pos) = self.drawables.iter().position(|s| s.id == id) else {
            return false;
        };
        if pos == 0 {
            return false;
        }
        let snapshot: Vec<DrawableId> = self.drawables.iter().map(|s| s.id).collect();
        let stacked = self.drawables.remove(pos);
        self.drawables.insert(0, stacked);
        self.undo_stack.push(UndoAction::Reorder {
            prev_order: snapshot,
            last_raised: None,
        });
        self.redo_stack.clear();
        true
    }

    /// Bring `id` all the way to the top of the stack. Non-coalescing
    /// counterpart of `reorder_to_top_coalesce` — used by the explicit
    /// "Front" button so a deliberate button press never collapses into
    /// a prior auto-raise of the same id.
    pub fn move_drawable_to_top(&mut self, id: DrawableId) -> bool {
        let Some(pos) = self.drawables.iter().position(|s| s.id == id) else {
            return false;
        };
        if pos + 1 == self.drawables.len() {
            return false;
        }
        let snapshot: Vec<DrawableId> = self.drawables.iter().map(|s| s.id).collect();
        let stacked = self.drawables.remove(pos);
        self.drawables.push(stacked);
        self.undo_stack.push(UndoAction::Reorder {
            prev_order: snapshot,
            last_raised: None,
        });
        self.redo_stack.clear();
        true
    }

    /// Replace the stack order with `new_order` if it's a permutation of
    /// the current ids. Used by drag-to-reorder. Records a single
    /// `Reorder` undo entry.
    pub fn reorder_to(&mut self, new_order: Vec<DrawableId>) -> bool {
        if new_order.len() != self.drawables.len() {
            return false;
        }
        let cur: std::collections::HashSet<DrawableId> =
            self.drawables.iter().map(|s| s.id).collect();
        if !new_order.iter().all(|id| cur.contains(id)) {
            return false;
        }
        let snapshot: Vec<DrawableId> = self.drawables.iter().map(|s| s.id).collect();
        if snapshot == new_order {
            return false;
        }
        let mut by_id: std::collections::HashMap<DrawableId, Stacked> =
            self.drawables.drain(..).map(|s| (s.id, s)).collect();
        for id in &new_order {
            if let Some(s) = by_id.remove(id) {
                self.drawables.push(s);
            }
        }
        self.undo_stack.push(UndoAction::Reorder {
            prev_order: snapshot,
            last_raised: None,
        });
        self.redo_stack.clear();
        true
    }

    /// True if some other *visible* drawable above `id` in the stack has
    /// bounds that intersect `id`'s bounds. Hidden drawables are skipped
    /// (nothing to see); locked drawables still count (visually present).
    pub fn has_visible_overlapper_above(&self, id: DrawableId) -> bool {
        let Some(my_idx) = self.drawables.iter().position(|s| s.id == id) else {
            return false;
        };
        let Some(my_bounds) = self.drawables[my_idx].drawable.bounds() else {
            return false;
        };
        self.drawables.iter().skip(my_idx + 1).any(|s| {
            if !s.visible {
                return false;
            }
            s.drawable
                .bounds()
                .map(|b| b.intersects(my_bounds))
                .unwrap_or(false)
        })
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
                    visible: stacked.visible,
                    locked: stacked.locked,
                    custom_name: stacked.custom_name,
                    auto_label_index: stacked.auto_label_index,
                }
            }
            UndoAction::Remove {
                id,
                idx,
                mut drawable,
                visible,
                locked,
                custom_name,
                auto_label_index,
            } => {
                drawable.handle_redo();
                let insert_at = idx.min(self.drawables.len());
                self.drawables.insert(
                    insert_at,
                    Stacked {
                        id,
                        drawable,
                        visible,
                        locked,
                        custom_name,
                        auto_label_index,
                    },
                );
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
            UndoAction::SetLayerFlags {
                id,
                prev_visible,
                prev_locked,
            } => {
                let pos = self
                    .drawables
                    .iter()
                    .position(|s| s.id == id)
                    .expect("SetLayerFlags references missing drawable");
                let cur_visible = self.drawables[pos].visible;
                let cur_locked = self.drawables[pos].locked;
                self.drawables[pos].visible = prev_visible;
                self.drawables[pos].locked = prev_locked;
                UndoAction::SetLayerFlags {
                    id,
                    prev_visible: cur_visible,
                    prev_locked: cur_locked,
                }
            }
            UndoAction::Rename { id, prev } => {
                let pos = self
                    .drawables
                    .iter()
                    .position(|s| s.id == id)
                    .expect("Rename references missing drawable");
                let cur = self.drawables[pos].custom_name.take();
                self.drawables[pos].custom_name = prev;
                UndoAction::Rename { id, prev: cur }
            }
            UndoAction::Reorder {
                prev_order,
                last_raised,
            } => {
                let cur_order: Vec<DrawableId> = self.drawables.iter().map(|s| s.id).collect();
                // Rebuild stack in `prev_order`. Move-by-take with a HashMap so
                // each Stacked transfers exactly once and drawables not named
                // in `prev_order` (shouldn't happen, but defensive) end up at
                // the top in their original relative order.
                let mut by_id: std::collections::HashMap<DrawableId, Stacked> =
                    self.drawables.drain(..).map(|s| (s.id, s)).collect();
                for id in &prev_order {
                    if let Some(s) = by_id.remove(id) {
                        self.drawables.push(s);
                    }
                }
                // Anything that survived isn't in prev_order — push at top.
                for (_, s) in by_id.drain() {
                    self.drawables.push(s);
                }
                // Preserve `last_raised` on the inverse so a later live raise
                // can still coalesce against this entry if it ends up back on
                // the undo stack after a redo.
                UndoAction::Reorder {
                    prev_order: cur_order,
                    last_raised,
                }
            }
            UndoAction::ResizeCanvas {
                prev_image,
                applied_offset,
                translated_ids,
            } => {
                let cur_image = std::mem::replace(&mut self.background_image, prev_image);
                self.background_image_id = None;
                let translated_set: HashSet<DrawableId> = translated_ids.iter().copied().collect();
                for s in &mut self.drawables {
                    if translated_set.contains(&s.id) {
                        s.drawable.translate(-applied_offset);
                    }
                }
                self.original_rect.pos -= applied_offset;
                UndoAction::ResizeCanvas {
                    prev_image: cur_image,
                    applied_offset: -applied_offset,
                    translated_ids,
                }
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
    /// the most recently drawn (visually on top) wins. Drawables hidden via
    /// either tool's `dragging_drawable_id` are skipped — they're effectively
    /// invisible (working copy renders on top), so they shouldn't be hit-test
    /// targets either. `try_borrow` falls back to no filter when a tool is
    /// already mutably borrowed (e.g. when PointerTool itself is calling
    /// hit_test from inside its own handler), which is the safe direction:
    /// worst case we hit-test more drawables than strictly necessary.
    pub fn hit_test(&self, point: Vec2D, tolerance: f32) -> Option<DrawableId> {
        let dragging_active = self
            .active_tool
            .try_borrow()
            .ok()
            .and_then(|t| t.dragging_drawable_id());
        let dragging_pointer = self
            .pointer_tool
            .try_borrow()
            .ok()
            .and_then(|t| t.dragging_drawable_id());
        for s in self.drawables.iter().rev() {
            if dragging_active == Some(s.id) || dragging_pointer == Some(s.id) {
                continue;
            }
            // Hidden drawables can't be hit (they're invisible) and locked
            // drawables can't be hit (they're a fixed background that the
            // pointer should pass through to whatever's beneath).
            if !s.visible || s.locked {
                continue;
            }
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
        // Publish DPR for text/UI sizing during the offscreen render
        // (used for save/clipboard export).
        super::set_current_device_pixel_ratio(self.device_pixel_ratio);
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
        canvas.set_render_target(RenderTarget::Image(image_id));

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
            true,
            RenderTarget::Image(image_id),
            transform,
            None,
        )?;

        // return screenshot
        let result = canvas.screenshot();

        // clean up
        canvas.set_render_target(RenderTarget::Screen);
        canvas.delete_image(image_id);

        Ok(result?)
    }

    pub fn render_framebuffer(
        &mut self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        font: FontId,
    ) -> Result<()> {
        canvas.set_render_target(RenderTarget::Screen);
        // Publish current DPR so drawables can size CSS-pixel UI
        // (text editing handles, outlines) inside `Drawable::draw`
        // without us having to thread it through every impl.
        super::set_current_device_pixel_ratio(self.device_pixel_ratio);

        // Choose between the regular pan/zoom transform and a
        // committed-crop fit transform. The crop fit centers and
        // scales the cropped region into the canvas; combined with
        // a scissor at the same canvas-space rect, anything outside
        // the crop renders as the canvas's clear color (black) so
        // the user sees only the cropped image.
        let canvas_w = canvas.width() as f32;
        let canvas_h = canvas.height() as f32;
        let (transform, scissor, eff_scale, eff_offset) = if let Some((crop_pos, crop_size)) =
            self.crop_tool.borrow().get_committed_rect()
            && crop_size.x > 0.0
            && crop_size.y > 0.0
        {
            // Render the committed crop at 1:1 when it fits in the
            // canvas with padding, with reduced padding when it just
            // fits the canvas, and scaled down only when it can't fit
            // at all. Mirror image of `update_transformation`'s
            // non-crop branch — same "100 % first, shrink padding,
            // scale only as last resort" cascade. Main.rs resizes the
            // window to fit (cropped + padding) on commit so the
            // canvas usually has enough room for the 1:1 path.
            // `crop_zoom` still multiplies on top so Ctrl+scroll
            // zooms further once cropped.
            let pad = CANVAS_PADDING_CSS * self.device_pixel_ratio.max(0.0001);
            let inner_w = (canvas_w - 2.0 * pad).max(canvas_w * 0.5).max(1.0);
            let inner_h = (canvas_h - 2.0 * pad).max(canvas_h * 0.5).max(1.0);
            let base_scale = auto_fit_scale(inner_w, inner_h, crop_size.x, crop_size.y);
            let scale = base_scale * self.crop_zoom;
            let crop_canvas_w = crop_size.x * scale;
            let crop_canvas_h = crop_size.y * scale;
            // pad_* can be negative when crop_zoom × base_scale > 1
            // (zoomed past the canvas edge); the scissor below clips
            // back to the visible window.
            let pad_x = (canvas_w - crop_canvas_w) / 2.0;
            let pad_y = (canvas_h - crop_canvas_h) / 2.0;
            // Clamp the user's pan to the in-bounds range for the
            // zoomed crop. If the crop fits entirely (excess ≤ 0)
            // there's no room to scroll, so the pan is pinned to 0
            // and the crop stays centered.
            let excess_x = (crop_canvas_w - canvas_w).max(0.0);
            let excess_y = (crop_canvas_h - canvas_h).max(0.0);
            self.drag_offset.x = self.drag_offset.x.clamp(-excess_x / 2.0, excess_x / 2.0);
            self.drag_offset.y = self.drag_offset.y.clamp(-excess_y / 2.0, excess_y / 2.0);
            self.last_offset = self.drag_offset;
            let offset_x = pad_x - scale * crop_pos.x + self.drag_offset.x;
            let offset_y = pad_y - scale * crop_pos.y + self.drag_offset.y;
            // Visible-content canvas-pixel rect — used by the
            // drop-shadow path so the shadow falls around the
            // cropped region, not the full background image
            // (whose edges are off-canvas / scissored out).
            self.display_rect_origin =
                Vec2D::new(pad_x + self.drag_offset.x, pad_y + self.drag_offset.y);
            self.display_rect_size = Vec2D::new(crop_canvas_w, crop_canvas_h);
            let mut t = Transform2D::identity();
            t.scale(scale, scale);
            t.translate(offset_x, offset_y);
            // Scissor takes coords in the CURRENT transform's space —
            // i.e., image space once the crop-fit transform is applied.
            // Passing canvas-pixel values here would silently mis-clip
            // every drawable whose geometry extends past the crop edges
            // (the background image alone clips correctly by virtue of
            // the transform mapping its non-crop pixels off-canvas;
            // strokes that crossed the crop boundary leaked through).
            (
                t,
                Some((crop_pos.x, crop_pos.y, crop_size.x, crop_size.y)),
                scale,
                Vec2D::new(offset_x, offset_y),
            )
        } else {
            // Leaving committed-crop view (or never entered) — reset
            // the user's crop-zoom multiplier so the next commit
            // starts cleanly at 100 % (1.0×). Without this, a user
            // who zoomed inside a crop, reverted, and re-cropped
            // would land in the new committed view at the OLD zoom
            // multiplier (surprising).
            self.crop_zoom = 1.0;
            // Non-crop view: visible rect is the full background image.
            let image_w = self.background_image.width() as f32;
            let image_h = self.background_image.height() as f32;
            self.display_rect_origin = self.offset;
            self.display_rect_size =
                Vec2D::new(image_w * self.scale_factor, image_h * self.scale_factor);
            let mut t = Transform2D::identity();
            t.scale(self.scale_factor, self.scale_factor);
            t.translate(self.offset.x, self.offset.y);
            (t, None, self.scale_factor, self.offset)
        };

        // (Effective-scale → zoom indicator emit happens in the
        //  outer FemtoVGArea::render after this returns, because the
        //  parent sender lives there.)

        // Cache the effective transform so input-coord conversion
        // routes through the same scale/offset the user is seeing.
        self.effective_scale = eff_scale;
        self.effective_offset = eff_offset;

        // Pre-scissor stage: fill the full canvas with CANVAS_BG and
        // draw the drop shadow in canvas-pixel space. Doing this here
        // (rather than inside `render`'s clear + shadow path) is what
        // lets the soft shadow blur fall OUTSIDE a committed crop's
        // scissor rectangle — if we cleared and drew the shadow after
        // setting the scissor, the blur would be clipped and the
        // cropped view would have no visible shadow.
        canvas.reset_transform();
        canvas.clear_rect(0, 0, canvas.width(), canvas.height(), CANVAS_BG);

        {
            let dpr = self.device_pixel_ratio.max(0.0001);
            let img_w = self.display_rect_size.x;
            let img_h = self.display_rect_size.y;
            let img_x = self.display_rect_origin.x;
            let img_y = self.display_rect_origin.y;

            let mut draw_layer = |center_x: f32, center_y: f32, blur: f32, alpha: f32| {
                let mut path = Path::new();
                path.rect(
                    center_x - blur,
                    center_y - blur,
                    img_w + 2.0 * blur,
                    img_h + 2.0 * blur,
                );
                let paint = Paint::box_gradient(
                    center_x,
                    center_y,
                    img_w,
                    img_h,
                    0.0,
                    blur,
                    femtovg::Color::rgbaf(0.0, 0.0, 0.0, alpha),
                    femtovg::Color::rgbaf(0.0, 0.0, 0.0, 0.0),
                );
                canvas.fill_path(&path, &paint);
            };

            // Ambient (contact) layer — tight halo, no offset.
            draw_layer(
                img_x,
                img_y,
                SHADOW_AMBIENT_BLUR_CSS * dpr,
                SHADOW_AMBIENT_ALPHA,
            );

            // Key (elevation) layer — wide, offset downward.
            draw_layer(
                img_x,
                img_y + SHADOW_KEY_OFFSET_Y_CSS * dpr,
                SHADOW_KEY_BLUR_CSS * dpr,
                SHADOW_KEY_ALPHA,
            );
        }

        canvas.reset_transform();
        canvas.set_transform(&transform);

        if let Some((sx, sy, sw, sh)) = scissor {
            canvas.scissor(sx, sy, sw, sh);
        }

        // Canvas + shadow are already painted above; tell `render`
        // to skip its own clear_rect so the shadow survives until
        // the image is drawn over it.
        self.render(
            canvas,
            font,
            true,
            CANVAS_BG,
            true,
            false,
            RenderTarget::Screen,
            transform,
            scissor,
        )?;

        if scissor.is_some() {
            canvas.reset_scissor();
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn render(
        &mut self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        font: FontId,
        render_crop: bool,
        outside_bg_color: femtovg::Color,
        onscreen: bool,
        clear_canvas: bool,
        restore_target: RenderTarget,
        restore_transform: Transform2D,
        restore_scissor: Option<(f32, f32, f32, f32)>,
    ) -> Result<()> {
        // Clear canvas. Skipped when the caller has already filled
        // the canvas + drawn the drop shadow pre-scissor (the
        // `render_framebuffer` path does this so the shadow blur can
        // fall outside a committed-crop's scissor without being
        // clipped).
        if clear_canvas {
            canvas.clear_rect(0, 0, canvas.width(), canvas.height(), outside_bg_color);
        }

        // render background
        self.render_background_image(canvas, onscreen)?;

        // Debug overlay: when `TENSAKU_DEBUG_BANDS=1`, draw a faint
        // colored stripe at each detected text band so we can
        // visually correlate the cursor's anchored position against
        // the heuristic's output. Temporary — strip once the
        // detector is dialed in.
        if std::env::var("TENSAKU_DEBUG_BANDS").is_ok() {
            for b in crate::text_bands::bands() {
                let mut path = femtovg::Path::new();
                path.rect(
                    0.0,
                    b.y_start,
                    self.background_image.width() as f32,
                    b.height(),
                );
                let paint = femtovg::Paint::color(femtovg::Color::rgba(255, 60, 60, 50));
                canvas.fill_path(&path, &paint);
                // Solid edge lines at top/bottom for sharp visual.
                let mut edge = femtovg::Path::new();
                edge.move_to(0.0, b.y_start);
                edge.line_to(self.background_image.width() as f32, b.y_start);
                edge.move_to(0.0, b.y_end);
                edge.line_to(self.background_image.width() as f32, b.y_end);
                let mut edge_paint = femtovg::Paint::color(femtovg::Color::rgba(255, 60, 60, 200));
                edge_paint.set_line_width(1.0);
                canvas.stroke_path(&edge, &edge_paint);
            }
        }

        let bounds = (
            Vec2D::zero(),
            Vec2D::new(
                self.background_image.width() as f32,
                self.background_image.height() as f32,
            ),
        );
        // Spotlight pass runs BEFORE the annotation loop so the dark
        // overlay sits BENEATH every drawable. Annotations (arrows,
        // text, shapes) need to stay legible regardless of spotlight
        // darkness — running this pass after them would dim every
        // annotation outside the spotlight cutout, including labels
        // the user explicitly placed there to point at the focused
        // region. Inside the cutout the punch-through still shows
        // the background untouched, so the spotlight effect on the
        // focus area itself is unchanged.
        //
        // Multiple spotlight shapes still union into one dark layer
        // because the punch-out happens against an offscreen image
        // first; running this pass earlier doesn't change that.
        self.render_spotlight_overlay(
            canvas,
            bounds,
            restore_target,
            restore_transform,
            restore_scissor,
        )?;

        // Skip rendering of any drawable currently being dragged by either
        // tool — the tool will render the moved/transformed copy below.
        let dragging_active = self.active_tool.borrow().dragging_drawable_id();
        let dragging_pointer = self.pointer_tool.borrow().dragging_drawable_id();
        let selected_ids = self.pointer_tool.borrow().selected_drawables();

        for s in &mut self.drawables {
            if dragging_active == Some(s.id) || dragging_pointer == Some(s.id) {
                continue;
            }
            // Layer-panel visibility: hidden drawables stay in the stack
            // (so canvas auto-resize still includes their bounds, and undo
            // restores them exactly) but don't render.
            if !s.visible {
                continue;
            }
            // Spotlights themselves don't `draw()` — their contribution
            // is the punch-out path collected by the spotlight pass
            // above. Skip so the loop only renders the annotation
            // stack on top of the (already-composited) overlay.
            if s.drawable.is_spotlight() {
                continue;
            }
            let is_selected = selected_ids.contains(&s.id);
            // Render the selection glow underneath each selected drawable so
            // the wide blue trace is half-clipped by the drawable on top —
            // leaving only an outer halo.
            if is_selected {
                s.drawable
                    .render_glow(canvas, font, bounds, self.device_pixel_ratio)?;
            }
            // Publish selection state so drawables that draw their
            // own selection decorations (e.g. text's outline) can
            // see fresh layout in the same draw call.
            super::set_current_drawable_is_selected(is_selected);
            s.drawable.draw(canvas, font, bounds)?;
            super::set_current_drawable_is_selected(false);
        }

        let pointer_is_active = Rc::ptr_eq(&self.active_tool, &self.pointer_tool);

        // In-progress drawable from the active tool (e.g. the shape currently
        // being drawn). When the pointer tool is the active tool *and* it's
        // mid-drag, this is the selection's working copy — drawn without the
        // selection glow so the user can see exactly where the edges land.
        //
        // Exception: during a handle *resize* we still publish the selection
        // flag. A text box's dashed outline IS the box being resized — hiding
        // it would leave nothing to aim the drag with. Only `Text::draw`
        // reads the flag, so this is a no-op for every other drawable, and
        // the glow stays off either way (`render_glow` isn't called here).
        // A move drag (`DragMode::Body`) keeps the old no-decoration look.
        {
            let at = self.active_tool.borrow();
            if let Some(d) = at.get_drawable() {
                let resizing = at.is_resizing();
                super::set_current_drawable_is_selected(resizing);
                d.draw(canvas, font, bounds)?;
                super::set_current_drawable_is_selected(false);
            }
        }

        // The pointer tool's working copy during an implicit-mode drag (active
        // tool is something else, like Arrow). Same treatment as the
        // active-tool branch above — including the resize exception that
        // keeps a text box's dashed outline visible while its handles drag.
        if !pointer_is_active {
            let pt = self.pointer_tool.borrow();
            if let Some(d) = pt.get_drawable() {
                super::set_current_drawable_is_selected(pt.is_resizing());
                d.draw(canvas, font, bounds)?;
                super::set_current_drawable_is_selected(false);
            }
        }

        // Selection overlay (marquee + handles for single selection).
        // The spotlight overlay already ran before the annotation
        // loop, so handles and marquee draw on top of the dim layer
        // at full brightness without needing extra ordering tricks
        // here.
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

    /// Build the inverse-mask dark overlay and composite it on top of
    /// the current canvas. No-ops when there are no spotlight shapes
    /// or when darkness rounds to zero. Multiple spotlight shapes
    /// union correctly because the punch-out happens against an
    /// offscreen layer first — doing it directly on the main canvas
    /// would erase the underlying screenshot in the punched regions.
    ///
    /// `restore_target` is the render target the caller had set
    /// before invoking this pass. We switch to a temporary offscreen
    /// image to build the punched overlay, then restore to
    /// `restore_target` and composite back. The caller's transform
    /// is re-established here too (image-space → canvas-space) so
    /// callers don't need to re-set their transform afterward.
    ///
    /// `restore_scissor` is the caller's clip rect (image-space
    /// coords, as passed to `Canvas::scissor`), or `None` when the
    /// caller had no scissor set. The offscreen pass must run with
    /// the clip *off* — the overlay buffer spans the whole image, so
    /// inheriting the committed-crop scissor would dark-fill only a
    /// misplaced sub-rectangle of it. We clear the scissor for the
    /// offscreen pass and re-apply this rect before compositing, so
    /// the final paint still clips to the crop and the clip stays
    /// active for the annotation pass that follows.
    fn render_spotlight_overlay(
        &self,
        canvas: &mut Canvas<renderer::OpenGl>,
        bounds: (Vec2D, Vec2D),
        restore_target: RenderTarget,
        restore_transform: Transform2D,
        restore_scissor: Option<(f32, f32, f32, f32)>,
    ) -> Result<()> {
        let darkness = self.spotlight_darkness.clamp(0.0, 1.0);
        if darkness < 0.001 {
            return Ok(());
        }

        // Collect every spotlight path (committed + the active tool's
        // in-progress one, if any). Pointer-tool drag previews can
        // also be spotlights when a user grabs an existing spotlight
        // to move it — surface those too so the live drag follows.
        let mut paths: Vec<Path> = Vec::new();
        let dragging_active = self.active_tool.borrow().dragging_drawable_id();
        let dragging_pointer = self.pointer_tool.borrow().dragging_drawable_id();
        for s in &self.drawables {
            if dragging_active == Some(s.id) || dragging_pointer == Some(s.id) {
                continue;
            }
            if !s.visible {
                continue;
            }
            if s.drawable.is_spotlight() {
                let mut p = Path::new();
                s.drawable.append_spotlight_path(&mut p);
                paths.push(p);
            }
        }
        {
            let at = self.active_tool.borrow();
            if let Some(d) = at.get_drawable()
                && d.is_spotlight()
            {
                let mut p = Path::new();
                d.append_spotlight_path(&mut p);
                paths.push(p);
            }
        }
        if !Rc::ptr_eq(&self.active_tool, &self.pointer_tool)
            && let Some(d) = self.pointer_tool.borrow().get_drawable()
            && d.is_spotlight()
        {
            let mut p = Path::new();
            d.append_spotlight_path(&mut p);
            paths.push(p);
        }
        if paths.is_empty() {
            return Ok(());
        }

        let img_w = (bounds.1.x - bounds.0.x).max(1.0) as usize;
        let img_h = (bounds.1.y - bounds.0.y).max(1.0) as usize;

        // Offscreen target for the punched overlay. FLIP_Y because
        // GL framebuffer-attached textures are bottom-up; without it
        // the composited image lands upside-down on the screen
        // target.
        let overlay_id =
            canvas.create_image_empty(img_w, img_h, PixelFormat::Rgba8, ImageFlags::FLIP_Y)?;

        canvas.flush();
        canvas.set_render_target(RenderTarget::Image(overlay_id));
        canvas.reset_transform();
        // Drop any scissor the caller left set. femtovg keeps the
        // scissor in canvas state across `set_render_target`, so in
        // committed-crop mode the clip would still be the on-screen
        // crop rect — and it would clip the dark fill below to that
        // rect *inside* this full-image offscreen buffer, leaving the
        // overlay dark only in a misplaced sub-rectangle. The clip is
        // re-applied before the composite (see below).
        canvas.reset_scissor();
        canvas.clear_rect(
            0,
            0,
            img_w as u32,
            img_h as u32,
            femtovg::Color::rgbaf(0.0, 0.0, 0.0, 0.0),
        );

        // Lay down the dark fill across the entire overlay.
        let mut fill = Path::new();
        fill.rect(0.0, 0.0, img_w as f32, img_h as f32);
        let dark = Paint::color(femtovg::Color::rgbaf(0.0, 0.0, 0.0, darkness));
        canvas.fill_path(&fill, &dark);

        // Punch the spotlight shapes out of the dark overlay. The
        // composite operation only cares about the source's alpha;
        // any opaque color works.
        canvas.global_composite_operation(CompositeOperation::DestinationOut);
        let punch = Paint::color(femtovg::Color::rgbaf(1.0, 1.0, 1.0, 1.0));
        for p in &paths {
            canvas.fill_path(p, &punch);
        }
        canvas.global_composite_operation(CompositeOperation::SourceOver);
        canvas.flush();

        // Restore the caller's target + transform and composite the
        // punched overlay on top.
        canvas.set_render_target(restore_target);
        canvas.reset_transform();
        canvas.set_transform(&restore_transform);
        // Re-apply the caller's scissor that the offscreen pass
        // cleared. `Canvas::scissor` bakes in the current transform,
        // so this has to follow `set_transform`. It clips the
        // composite to the committed crop and leaves the clip active
        // for the annotation pass that runs after this returns.
        if let Some((sx, sy, sw, sh)) = restore_scissor {
            canvas.scissor(sx, sy, sw, sh);
        }

        let mut final_path = Path::new();
        final_path.rect(0.0, 0.0, img_w as f32, img_h as f32);
        let composited = Paint::image(overlay_id, 0.0, 0.0, img_w as f32, img_h as f32, 0.0, 1.0);
        canvas.fill_path(&final_path, &composited);
        canvas.flush();

        canvas.delete_image(overlay_id);
        Ok(())
    }

    /// Update the global spotlight darkness used by the next render.
    /// Sketch_board calls this on every slider change; the change
    /// becomes visible after the next `request_render`.
    pub fn set_spotlight_darkness(&mut self, value: f32) {
        self.spotlight_darkness = value.clamp(0.0, 1.0);
    }

    /// Current global spotlight darkness (0.0–1.0). Read by the
    /// layer panel to render a swatch that matches the dim overlay
    /// the spotlight effect actually paints.
    pub fn spotlight_darkness(&self) -> f32 {
        self.spotlight_darkness
    }

    /// Mirror the background image horizontally and invalidate the
    /// uploaded GL texture so the next render uploads the flipped
    /// pixels. Existing drawables keep their image-space positions
    /// (so a flip immediately followed by drawing lands annotations
    /// over the mirrored content; a flip AFTER drawing leaves the
    /// annotations where they were, no longer tracking the image
    /// content — that's a documented limitation, fixable by
    /// extending the Drawable trait with a mirror op later if it
    /// shows up as friction).
    ///
    /// Returns `true` when the flip succeeded; `false` when the
    /// Pixbuf couldn't be flipped (out of memory).
    pub fn flip_image_horizontal(&mut self) -> bool {
        let Some(flipped) = self.background_image.flip(true) else {
            return false;
        };
        self.background_image = flipped;
        self.background_image_id = None;
        true
    }

    /// Rotate the background image 90° counter-clockwise and
    /// invalidate the uploaded GL texture. Returns the NEW
    /// `(width, height)` in image-space pixels (width and height
    /// swap) so the caller can update the crop tool's bounds and
    /// emit a `ContentSizeChanged` to resize the window around
    /// the rotated image.
    ///
    /// Drawables don't rotate with the image (same limitation as
    /// `flip_image_horizontal`). Typical workflow is "rotate first,
    /// annotate after".
    pub fn rotate_image_ccw(&mut self) -> Option<(f32, f32)> {
        let rotated = self
            .background_image
            .rotate_simple(gtk::gdk_pixbuf::PixbufRotation::Counterclockwise)?;
        let new_w = rotated.width() as f32;
        let new_h = rotated.height() as f32;
        self.background_image = rotated;
        self.background_image_id = None;
        Some((new_w, new_h))
    }

    /// Resample the background image to the target pixel dimensions
    /// via `Pixbuf::scale_simple` (bilinear). Invalidates the
    /// uploaded GL texture so the next render uploads the resampled
    /// pixels. Returns the new `(width, height)` once the resample
    /// succeeds; `None` on a degenerate request (zero / negative
    /// dim) or out-of-memory failure inside `scale_simple`.
    ///
    /// Drawables don't scale with the image — same limitation as
    /// the other transforms in this section. Resizing typically
    /// happens before annotating; flatten-into-image first if you
    /// need to ship pre-annotated artwork at a smaller dim.
    pub fn resize_image(&mut self, new_w: i32, new_h: i32) -> Option<(f32, f32)> {
        if new_w <= 0 || new_h <= 0 {
            return None;
        }
        let resized = self.background_image.scale_simple(
            new_w,
            new_h,
            gtk::gdk_pixbuf::InterpType::Bilinear,
        )?;
        let w = resized.width() as f32;
        let h = resized.height() as f32;
        self.background_image = resized;
        self.background_image_id = None;
        Some((w, h))
    }

    /// Current image-space dimensions of the background. Used by
    /// the toolbar's "Image size: W×H" label to show what the
    /// resize popover would default the W/H inputs to.
    pub fn image_dimensions(&self) -> (i32, i32) {
        (
            self.background_image.width(),
            self.background_image.height(),
        )
    }

    /// Canvas height (CSS px) at which the auto-fit zoom's vertical
    /// term equals `MIN_AUTO_FIT_ZOOM` — the shortest the canvas may
    /// get before a window resize would shrink the image past that
    /// zoom. The content is the committed crop region when one is
    /// active, otherwise the full image.
    ///
    /// This inverts `update_transformation`'s height term exactly,
    /// including its `inner_h = (canvas − 2·pad).max(canvas · 0.5)`
    /// degenerate guard: the guard wins for short content, so the
    /// answer is the smaller of the two candidate heights.
    fn min_canvas_height_logical(&self) -> f32 {
        let content_h = self
            .crop_tool
            .borrow()
            .get_committed_rect()
            .map(|(_, size)| size.y)
            .filter(|h| *h > 0.0)
            .unwrap_or(self.background_image.height() as f32);
        let dpr = self.device_pixel_ratio.max(0.0001);
        let pad = CANVAS_PADDING_CSS * dpr;
        // `inner_h = 0.1·content_h` solved for the canvas DEVICE
        // height, once via `canvas − 2·pad` and once via the
        // `canvas · 0.5` guard; the consistent root is the smaller.
        let device =
            (MIN_AUTO_FIT_ZOOM * content_h + 2.0 * pad).min(2.0 * MIN_AUTO_FIT_ZOOM * content_h);
        device / dpr
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

        let w = self.background_image.width() as f32;
        let h = self.background_image.height() as f32;

        // (The on-screen drop shadow is drawn pre-scissor by
        //  `render_framebuffer` so it doesn't get clipped to the
        //  cropped region in committed-crop mode — see the shadow
        //  block at the top of that function. Saved exports skip
        //  shadow entirely.)

        // render the image
        let mut path = Path::new();
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

        let canvas_width = canvas.width() as f32;
        let canvas_height = canvas.height() as f32;
        self.last_canvas_size = Vec2D::new(canvas_width, canvas_height);

        // update scale_factor
        if self.zoom_scale != 0.0 {
            if self.zoom_scale != self.last_scale {
                self.last_scale = self.zoom_scale;
                self.scale_factor = self.zoom_scale;

                if !self.is_reset && !self.zoom_anchor_pending {
                    // Keep the image centered on zoom — clear the
                    // accumulated drag offset so `center_offset`
                    // (below) places the image at the canvas's
                    // middle. Skipped when the zoom came from
                    // `set_zoom_scale_at`, which has already
                    // computed a `drag_offset` that anchors the
                    // image under the cursor.
                    self.drag_offset = Vec2D::zero();
                    self.store_last_offset();
                }
                self.zoom_anchor_pending = false;
            } else {
                self.scale_factor = self.zoom_scale;
            }
        } else {
            // Auto-fit branch (no user zoom yet): always reserve
            // `CANVAS_PADDING_CSS` of breathing room on every side.
            // If the image fits inside that padded area at 1:1, render
            // at 100 %. Otherwise scale it down so it still fits
            // inside the padded area — never go edge-to-edge
            // automatically. The user can pinch / scroll to zoom in
            // past this if they want. `auto_fit_scale` floors the
            // result at `MIN_AUTO_FIT_ZOOM` so a window squeezed
            // shorter than that clips the image instead of shrinking
            // it further. The `.max(canvas * 0.5)` floor is a
            // degenerate-case guard for canvases smaller than 2 × pad
            // — keeps the inner area positive so the computed scale
            // stays finite during initial layout.
            let pad = CANVAS_PADDING_CSS * self.device_pixel_ratio.max(0.0001);
            let inner_w = (canvas_width - 2.0 * pad).max(canvas_width * 0.5).max(1.0);
            let inner_h = (canvas_height - 2.0 * pad)
                .max(canvas_height * 0.5)
                .max(1.0);
            self.scale_factor = auto_fit_scale(inner_w, inner_h, image_width, image_height);
        }

        // `effective_scale` is what the zoom indicator should show:
        // it's the on-screen scale a fresh `render_framebuffer` will
        // actually use. For the regular view that's just
        // `scale_factor`. For a committed crop we re-run the same
        // auto-fit-with-padding cascade `render_framebuffer` uses —
        // 100 % when the crop fits with padding, scaled-down only
        // when it can't — multiplied by `crop_zoom`. Computing this
        // here (rather than waiting for `render_framebuffer`) means
        // the existing `notify_zoom_display` path in `resize` picks
        // up the right value without special-casing crop mode.
        let committed_crop = self
            .crop_tool
            .borrow()
            .get_committed_rect()
            .filter(|(_, s)| s.x > 0.0 && s.y > 0.0);
        self.effective_scale = if let Some((_, crop_size)) = committed_crop {
            let pad = CANVAS_PADDING_CSS * self.device_pixel_ratio.max(0.0001);
            let inner_w = (canvas_width - 2.0 * pad).max(canvas_width * 0.5).max(1.0);
            let inner_h = (canvas_height - 2.0 * pad)
                .max(canvas_height * 0.5)
                .max(1.0);
            auto_fit_scale(inner_w, inner_h, crop_size.x, crop_size.y) * self.crop_zoom
        } else {
            self.scale_factor
        };

        let center_offset = Vec2D::new(
            (canvas_width - image_width * self.scale_factor) / 2.0,
            (canvas_height - image_height * self.scale_factor) / 2.0,
        );

        // When the image fully fits the canvas on an axis (`excess
        // == 0`) there's no scroll affordance at all, so we hard-pin
        // the drag offset to zero on that axis — rubber-banding a
        // fit-to-canvas image would let the user pull a perfectly
        // centered screenshot off-center for no reason.
        //
        // For axes that DO have excess we let `drag_offset` grow
        // freely; the rubber-band map below produces an ever-
        // diminishing visible overshoot so further pulling at the
        // extreme still translates to a sliver of motion (matches
        // macOS' bottomless-elastic behavior) instead of slamming
        // into a hard cap. The clamp at a very large multiple of
        // `max_overshoot` is purely a guard against runaway float
        // growth from minutes of held-down scrolling.
        let excess_x = (image_width * self.scale_factor - canvas_width).max(0.0);
        let excess_y = (image_height * self.scale_factor - canvas_height).max(0.0);
        let limit_x = excess_x / 2.0;
        let limit_y = excess_y / 2.0;
        let max_overshoot = RUBBER_BAND_MAX_OVERSHOOT_CSS * self.device_pixel_ratio.max(0.0001);
        let runaway_cap = 100.0 * max_overshoot.max(1.0);
        if excess_x <= 0.0 {
            self.drag_offset.x = 0.0;
        } else {
            self.drag_offset.x = self
                .drag_offset
                .x
                .clamp(-limit_x - runaway_cap, limit_x + runaway_cap);
        }
        if excess_y <= 0.0 {
            self.drag_offset.y = 0.0;
        } else {
            self.drag_offset.y = self
                .drag_offset
                .y
                .clamp(-limit_y - runaway_cap, limit_y + runaway_cap);
        }

        // Spring-back: once the user is idle, ease `drag_offset` back
        // toward the nearest hard limit so the rubber-band stretch
        // recovers smoothly. Skipped while a gesture is mid-flight
        // (we'd fight the user's input) or while a drawable is being
        // dragged (`is_drag`).
        let idle_ms = std::time::Instant::now()
            .duration_since(self.last_pan_input)
            .as_millis();
        if idle_ms > SPRING_BACK_IDLE_MS && !self.is_drag {
            // Lock in the recovery start state — the VISIBLE offset
            // at release (rubber-banded), not the raw drag_offset.
            // Subsequent ticks ease this value toward the limit on a
            // smooth curve, and we back-solve a drag_offset that
            // reproduces the eased visible offset via the
            // rubber-band map.
            let (start_time, start_visible) = match self.spring_back_anim {
                Some(s) => s,
                None => {
                    let visible = Vec2D::new(
                        rubber_band(self.drag_offset.x, limit_x, max_overshoot),
                        rubber_band(self.drag_offset.y, limit_y, max_overshoot),
                    );
                    let s = (std::time::Instant::now(), visible);
                    self.spring_back_anim = Some(s);
                    s
                }
            };
            let elapsed_ms = start_time.elapsed().as_millis() as f32;
            let (vis_x, done_x) = spring_back_progress(start_visible.x, limit_x, elapsed_ms);
            let (vis_y, done_y) = spring_back_progress(start_visible.y, limit_y, elapsed_ms);
            // Back-solve drag_offset so the rubber-band render below
            // reproduces the eased visible value.
            self.drag_offset.x = inverse_rubber_band(vis_x, limit_x, max_overshoot);
            self.drag_offset.y = inverse_rubber_band(vis_y, limit_y, max_overshoot);
            if done_x && done_y {
                self.spring_back_anim = None;
            }
        } else {
            // Active gesture (or no overshoot) — drop any pending
            // animation so the next idle stretch starts a fresh
            // recovery from the user's release point.
            self.spring_back_anim = None;
        }
        self.last_offset = self.drag_offset;

        // Rubber-band map for rendering: even with `drag_offset` past
        // the limit, the OFFSET we hand to the canvas is damped via a
        // hyperbolic curve that asymptotes at `limit + max_overshoot`.
        // Pulling past the edge feels stretchy instead of slamming.
        let effective_x = rubber_band(self.drag_offset.x, limit_x, max_overshoot);
        let effective_y = rubber_band(self.drag_offset.y, limit_y, max_overshoot);

        if self.is_reset {
            //centered
            self.is_reset = false;
            self.offset = center_offset;
        } else {
            //dragged
            self.offset = center_offset + Vec2D::new(effective_x, effective_y);
        }
    }

    /// Pan the canvas by `(dx, dy)` canvas-space pixels. Accumulates
    /// into `drag_offset`; the next `update_transformation` applies
    /// rubber-band damping to the render side and (once the user is
    /// idle) drives the ease-in-out recovery back inside the limit.
    pub fn pan_by(&mut self, dx: f32, dy: f32) {
        self.drag_offset.x += dx;
        self.drag_offset.y += dy;
        self.last_offset = self.drag_offset;
        self.last_pan_input = std::time::Instant::now();
        // User took over — abandon any in-flight recovery so the
        // next idle stretch starts fresh from the new release point.
        self.spring_back_anim = None;
    }

    /// True when `drag_offset` is currently outside the hard pan
    /// limits — i.e. the rubber-band stretch is non-zero and the
    /// spring-back timer should keep ticking until it's recovered.
    pub fn drag_offset_overshoots(&self) -> bool {
        let canvas_w = self.last_canvas_size.x;
        let canvas_h = self.last_canvas_size.y;
        if canvas_w <= 0.0 || canvas_h <= 0.0 {
            return false;
        }
        let image_w = self.background_image.width() as f32 * self.scale_factor;
        let image_h = self.background_image.height() as f32 * self.scale_factor;
        let limit_x = (image_w - canvas_w).max(0.0) / 2.0;
        let limit_y = (image_h - canvas_h).max(0.0) / 2.0;
        self.drag_offset.x.abs() > limit_x + SPRING_BACK_SNAP_EPS
            || self.drag_offset.y.abs() > limit_y + SPRING_BACK_SNAP_EPS
    }

    /// Apply a scrollbar value to one axis. Scrollbar values run
    /// 0..=excess (where excess = image*scale − canvas), counted
    /// from the top/left of the scaled image. Our `drag_offset` is
    /// centered: `-excess/2` means the image is fully shifted left
    /// (right edge visible), `+excess/2` is fully shifted right.
    /// So `drag = excess/2 − value`. If the canvas size hasn't been
    /// captured yet (no `update_transformation` has run), this is a
    /// no-op — there's nothing to scroll on a zero-sized canvas.
    pub fn set_pan_from_scrollbar(&mut self, is_horizontal: bool, value: f32) {
        let image_w = self.background_image.width() as f32 * self.scale_factor;
        let image_h = self.background_image.height() as f32 * self.scale_factor;
        if is_horizontal {
            let excess = (image_w - self.last_canvas_size.x).max(0.0);
            if excess <= 0.0 {
                return;
            }
            self.drag_offset.x = (excess / 2.0 - value).clamp(-excess / 2.0, excess / 2.0);
        } else {
            let excess = (image_h - self.last_canvas_size.y).max(0.0);
            if excess <= 0.0 {
                return;
            }
            self.drag_offset.y = (excess / 2.0 - value).clamp(-excess / 2.0, excess / 2.0);
        }
        self.last_offset = self.drag_offset;
    }

    /// Current image-to-canvas scale used for the most recent render.
    /// Falls back to `scale_factor` if `update_transformation` hasn't
    /// run yet (which would leave `effective_scale` at its 1.0 init).
    pub fn effective_scale_or_fallback(&self) -> f32 {
        if self.effective_scale > 0.0 {
            self.effective_scale
        } else {
            self.scale_factor.max(1.0)
        }
    }

    /// The renderer's current image→canvas transform: effective scale
    /// and offset. The crop tool reads the scale on activation and after
    /// transform-changing gestures to keep handle hit-testing
    /// screen-constant.
    pub fn render_transform(&self) -> (f32, Vec2D) {
        (self.effective_scale, self.effective_offset)
    }

    pub fn abs_canvas_to_image_coordinates(&self, input: Vec2D, dpi_scale_factor: f32) -> Vec2D {
        Vec2D::new(
            (input.x * dpi_scale_factor - self.effective_offset.x) / self.effective_scale,
            (input.y * dpi_scale_factor - self.effective_offset.y) / self.effective_scale,
        )
    }
    pub fn rel_canvas_to_image_coordinates(&self, input: Vec2D, dpi_scale_factor: f32) -> Vec2D {
        Vec2D::new(
            input.x * dpi_scale_factor / self.effective_scale,
            input.y * dpi_scale_factor / self.effective_scale,
        )
    }

    pub fn set_zoom_scale(&mut self, factor: f32, abs: bool) {
        if self.is_drag {
            return;
        }

        // In committed-crop mode the base scale is the fit-to-canvas
        // calculation done at render time, not `scale_factor`. Route
        // the user's zoom into `crop_zoom` (a multiplier on top of
        // the fit) so wheel-up makes the crop larger and wheel-down
        // makes it smaller. Clamp to 0.5×–8× so the user can't lose
        // the image off-screen at one extreme or zoom out so far it
        // becomes a dot at the other.
        if self.crop_tool.borrow().get_committed_rect().is_some() {
            if abs {
                self.crop_zoom = factor.clamp(0.5, 8.0);
            } else {
                self.crop_zoom = (self.crop_zoom * factor).clamp(0.5, 8.0);
            }
            return;
        }

        // User-zoom range: 10% to 500%. Anything outside is either too
        // dot-like to make out (below 10%) or so blown up that the
        // user can only see a sliver of the image (above 500%).
        // `factor == 0.0` is the FitCanvas sentinel — preserved as-is
        // so `update_transformation` re-enters the auto-fit branch.
        const MIN_ZOOM: f32 = 0.10;
        const MAX_ZOOM: f32 = 5.00;

        if abs {
            if factor == 0.0 {
                self.zoom_scale = 0.0;
            } else {
                self.zoom_scale = factor.clamp(MIN_ZOOM, MAX_ZOOM);
            }
        } else {
            if self.zoom_scale == 0.0 {
                self.zoom_scale = self.scale_factor;
            }

            self.zoom_scale = (self.zoom_scale * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        }
    }

    pub fn set_pointer_offset(&mut self, offset: Vec2D) {
        self.pointer_offset = offset;
    }

    /// Last known cursor position in canvas (physical) pixels. The
    /// Motion controller pushes this via `set_pointer_offset` on
    /// every move, so it tracks the user's cursor across the canvas
    /// continuously — used by `set_zoom_scale_at_cursor` to anchor
    /// wheel-zoom on whatever the user is hovering over.
    pub fn pointer_offset(&self) -> Vec2D {
        self.pointer_offset
    }

    /// Zoom while keeping `anchor_canvas` (in canvas physical pixels,
    /// same units as `pointer_offset` / `drag_offset`) under the same
    /// canvas position before and after. Reduces to `set_zoom_scale`
    /// when the scale doesn't actually change, or when committed crop
    /// is active (the crop view has its own zoom semantics).
    pub fn set_zoom_scale_at(&mut self, factor: f32, abs: bool, anchor_canvas: Vec2D) {
        if self.is_drag {
            return;
        }
        // Committed-crop mode routes zoom through `crop_zoom` (a
        // multiplier on top of the fit) — no drag_offset to adjust,
        // so just defer to the existing path.
        if self.crop_tool.borrow().get_committed_rect().is_some() {
            self.set_zoom_scale(factor, abs);
            return;
        }
        // Capture pre-zoom state so we can solve for the new
        // drag_offset that keeps the anchor pinned.
        let canvas_w = self.last_canvas_size.x;
        let canvas_h = self.last_canvas_size.y;
        let image_w = self.background_image.width() as f32;
        let image_h = self.background_image.height() as f32;
        let old_scale = self.scale_factor;
        if canvas_w <= 0.0 || canvas_h <= 0.0 || old_scale <= 0.0 {
            self.set_zoom_scale(factor, abs);
            return;
        }
        let old_center = Vec2D::new(
            (canvas_w - image_w * old_scale) / 2.0,
            (canvas_h - image_h * old_scale) / 2.0,
        );
        // Image-space point (in original image pixels) currently
        // displayed at `anchor_canvas`.
        let image_pt = (anchor_canvas - old_center - self.drag_offset) * (1.0 / old_scale);

        // Apply the zoom request through the standard path so the
        // crop-zoom branch + min/max clamps + FitCanvas sentinel
        // all stay centralised.
        self.set_zoom_scale(factor, abs);

        // `set_zoom_scale` writes `zoom_scale`; `scale_factor`
        // doesn't update until the next `update_transformation`.
        // Compute the future scale ourselves so we can set the
        // matching `drag_offset` right now (avoids a one-frame
        // flicker where the image briefly recenters).
        let new_scale = if self.zoom_scale > 0.0 {
            self.zoom_scale
        } else {
            // FitCanvas / cold start — let auto-fit run, no anchor.
            return;
        };
        if (new_scale - old_scale).abs() < 1e-4 {
            return;
        }
        let new_center = Vec2D::new(
            (canvas_w - image_w * new_scale) / 2.0,
            (canvas_h - image_h * new_scale) / 2.0,
        );
        self.drag_offset = anchor_canvas - new_center - image_pt * new_scale;
        self.store_last_offset();
        // Tell update_transformation NOT to zero this drag_offset
        // when it picks up the new scale on the next render tick.
        self.zoom_anchor_pending = true;
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
