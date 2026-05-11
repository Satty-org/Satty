//! Custom drawing-tool cursors. Both Brush and Highlighter use a
//! "double ring" cursor (dark outer, light inner) so the cursor stays
//! readable on any background — same reasoning as Mac apps using a
//! drawing cursors.
//!
//! - Brush: circular, diameter = stroke line width. Tells the user
//!   exactly how thick the next pen stroke will be.
//! - Highlighter: vertical capsule (chisel tip) whose outer height —
//!   the top of one rounded cap to the top of the other — equals the
//!   width of the highlight stripe that gets laid down. The cursor's
//!   width is a thin marker-tip value so it still reads as a chisel
//!   rather than a fat capsule.
//!
//! On HiDPI displays, GTK4 paints cursor textures at a larger
//! on-screen size than their texture pixel count would suggest, so the
//! cursor builders divide by DPR to compensate — without this, a
//! 30 px highlight stroke rendered at 2x DPR drew at 30 CSS px on
//! screen while the cursor texture (30 px) showed at 60 CSS px, and
//! the highlight came out at ~60 % of the cursor.
//!
//! Cursors are recreated whenever the relevant style inputs change
//! (size, annotation_size_factor); neither cursor encodes color, so
//! changing the picker color does not require regeneration.

use relm4::gtk::cairo;
use relm4::gtk::gdk;
use relm4::gtk::gdk_pixbuf::Pixbuf;
use std::f64::consts::PI;

use crate::style::{Size, Style};

/// Padding around the cursor shape, in pixels, to leave room for the
/// outer ring stroke without it being clipped at the texture edge.
const RING_PAD: f64 = 2.0;

/// Outer ring stroke width — slightly thicker than the inner ring so
/// the dark outline reads clearly on light backgrounds.
const OUTER_LINE_WIDTH: f64 = 1.6;
const INNER_LINE_WIDTH: f64 = 1.0;

/// Don't render cursors smaller than this — a 2px wide cursor would
/// be invisible after the rings are drawn. Floors XSmall to a
/// reasonable minimum.
const MIN_CURSOR_PX: f64 = 8.0;

/// Build a circular double-ring cursor for the Brush tool. Diameter
/// matches the brush's stroke line width AS RENDERED on screen —
/// `render_scale` is the renderer's image→canvas multiplier, and
/// `device_pixel_ratio` divides out the extra scaling GTK4 applies
/// to cursor textures on HiDPI surfaces (without this, a HiDPI
/// cursor reads ~DPR× larger than the stroke that comes out of it).
pub fn build_brush_cursor(
    style: &Style,
    render_scale: f64,
    device_pixel_ratio: f64,
) -> Option<gdk::Cursor> {
    let dpr = device_pixel_ratio.max(1.0);
    let diameter = style.size.to_line_width(style.annotation_size_factor) as f64
        * render_scale
        / dpr;
    let diameter = diameter.max(MIN_CURSOR_PX);
    build_double_ring_cursor(diameter, diameter)
}

/// Build a vertical-capsule (chisel-tip) double-ring cursor for the
/// Highlighter tool. The capsule's outer HEIGHT — from the top of one
/// rounded cap to the top of the other — equals the highlight stroke
/// width that will be laid down. Width is a proportionally narrow
/// marker-tip value (~1/6 the height, floored at 4 px so XSmall
/// doesn't degenerate to a vertical line).
///
/// `render_scale` is the image→canvas multiplier (zoom); `device_pixel_ratio`
/// undoes the on-screen upscaling GTK4 applies to cursor textures on
/// HiDPI surfaces.
pub fn build_highlighter_cursor(
    style: &Style,
    render_scale: f64,
    device_pixel_ratio: f64,
) -> Option<gdk::Cursor> {
    let dpr = device_pixel_ratio.max(1.0);
    let height = style
        .size
        .to_highlight_width(style.annotation_size_factor) as f64
        * render_scale
        / dpr;
    let height = height.max(MIN_CURSOR_PX);
    // Marker-tip width: about a sixth of the height, but never
    // narrower than 4 px so smaller sizes still read as a tip
    // rather than a vertical line. For XSmall the floor wins; for
    // XXLarge we get ~15 px.
    let width = (height / 6.0).max(4.0).min(height);
    build_double_ring_cursor(width, height)
}

/// Render a capsule (rounded rectangle with full-end semicircles) of
/// the given pixel width and height with the dark+light double ring
/// applied as outline strokes. Returns a `gdk::Cursor` with hotspot
/// at the geometric center.
fn build_double_ring_cursor(width: f64, height: f64) -> Option<gdk::Cursor> {
    let total_w = (width + RING_PAD * 2.0).ceil() as i32;
    let total_h = (height + RING_PAD * 2.0).ceil() as i32;

    // GTK / GDK refuses huge cursors silently on some compositors.
    // Cap the dimensions so XXLarge highlighter (~90px) plus padding
    // doesn't exceed typical 128px cursor support.
    if total_w > 128 || total_h > 128 {
        return None;
    }

    let surface =
        cairo::ImageSurface::create(cairo::Format::ARgb32, total_w, total_h).ok()?;
    let ctx = cairo::Context::new(&surface).ok()?;

    let cx = total_w as f64 / 2.0;
    let cy = total_h as f64 / 2.0;
    let half_w = width / 2.0;
    let half_h = height / 2.0;
    // Capsule rounding radius = the smaller half-dimension. For a
    // square (width == height), this naturally degenerates into a
    // full circle — the brush case.
    let r = half_w.min(half_h);

    // Outer ring (dark) drawn first; inner light ring overlays it.
    draw_capsule_path(&ctx, cx, cy, half_w, half_h, r);
    ctx.set_source_rgba(0.0, 0.0, 0.0, 0.65);
    ctx.set_line_width(OUTER_LINE_WIDTH);
    let _ = ctx.stroke_preserve();

    // Inner ring stroked along the same path so the two rings are
    // perfectly concentric. Cairo strokes are centered on the path,
    // so a thinner light ring renders inside the dark one.
    ctx.set_source_rgba(1.0, 1.0, 1.0, 0.95);
    ctx.set_line_width(INNER_LINE_WIDTH);
    let _ = ctx.stroke();

    drop(ctx);

    let pixbuf: Pixbuf =
        gdk::pixbuf_get_from_surface(&surface, 0, 0, total_w, total_h)?;
    let texture = gdk::Texture::for_pixbuf(&pixbuf);
    Some(gdk::Cursor::from_texture(
        &texture,
        total_w / 2,
        total_h / 2,
        None,
    ))
}

/// Append a capsule (rounded rectangle with semicircular caps) to the
/// cairo context's current path. Geometry is centered on `(cx, cy)`
/// with the given half-width and half-height; `r` is the corner
/// radius (clamped to half-w for cap fullness).
fn draw_capsule_path(
    ctx: &cairo::Context,
    cx: f64,
    cy: f64,
    half_w: f64,
    half_h: f64,
    r: f64,
) {
    let r = r.min(half_w).min(half_h);
    let left = cx - half_w;
    let right = cx + half_w;
    let top = cy - half_h;
    let bottom = cy + half_h;

    ctx.new_path();
    // Top semicircle
    ctx.arc(cx, top + r, r, PI, 2.0 * PI);
    // Right edge (only if there's a flat region — for a circle we
    // skip directly to the bottom semicircle)
    if (half_h - r).abs() > 0.001 {
        ctx.line_to(right, bottom - r);
    }
    // Bottom semicircle
    ctx.arc(cx, bottom - r, r, 0.0, PI);
    if (half_h - r).abs() > 0.001 {
        ctx.line_to(left, top + r);
    }
    ctx.close_path();
}

/// Convenience: pick the right cursor builder for the given tool +
/// style. Returns `None` for tools that should keep their existing
/// system cursor.
pub fn drawing_tool_cursor(
    tool: crate::tools::Tools,
    style: &Style,
    render_scale: f64,
    device_pixel_ratio: f64,
) -> Option<gdk::Cursor> {
    use crate::tools::Tools;
    match tool {
        Tools::Brush => build_brush_cursor(style, render_scale, device_pixel_ratio),
        Tools::Highlighter => build_highlighter_cursor(style, render_scale, device_pixel_ratio),
        _ => None,
    }
}

// Suppress "unused" if Size is referenced only for the public API.
#[allow(dead_code)]
fn _force_use_size(_: Size) {}
