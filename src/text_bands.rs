//! Heuristic detection of horizontal "text bands" in the captured
//! screenshot. Used by the Highlighter tool to snap freehand strokes
//! to the bounds of underlying text lines (the "smart" highlighter
//! mode), and to render a hover preview that hints at the band the
//! cursor is over.
//!
//! Why a heuristic and not OCR: macOS apps can lean on Apple's Vision
//! framework — a system-provided text recognizer with no analogue on
//! wlroots/Linux. Bundling Tesseract would add a heavy native dependency
//! and noticeable startup latency. The heuristic below is essentially
//! free (one O(w·h) pass at app init), works on the common text-on-light
//! and text-on-dark layouts we care about, and degrades gracefully — when
//! the heuristic fails (icons, photos) the user gets the regular
//! free-form highlighter behavior.
//!
//! The detector runs once at app start and the result is cached
//! globally via `OnceLock`. The Highlighter tool reads from the
//! cache; no per-frame cost.
//!
//! Algorithm: per-row horizontal edge density. For each row we count
//! the number of sampled positions where `|lum(x) - lum(x+STRIDE)|`
//! exceeds `EDGE_DELTA` — i.e. horizontal brightness transitions.
//! Text rows have many such transitions (glyph edges); solid-
//! background rows have ~0. Crucially this is invariant to the row's
//! absolute background color, which makes the heuristic work on UI
//! screenshots whose rows span multiple background regions (dialog
//! backdrop + button surface + accent button) — where the
//! row-mean-based approach used to fail because no single mean
//! described "the background" for the row.
//!
//! Vertically dilate the per-row "is-text" signal by
//! `VERTICAL_MERGE_GAP` to bridge intra-line gaps (between ascender
//! and x-height body, around bowls of `e`/`a`/`o`, antialiased ramp
//! rows that fall below threshold), then group contiguous-true
//! regions into bands. Filter by height to drop obvious non-text
//! (single horizontal rules, multi-row blocks of solid color, etc.).

use relm4::gtk::gdk_pixbuf::Pixbuf;
use std::cell::RefCell;
use std::sync::OnceLock;

/// One detected horizontal text band, in image pixel coordinates.
/// `y_start` is the topmost row that tested as text-like; `y_end` is
/// one past the last (half-open). `center_y` and `height` are
/// computed helpers so the call sites don't have to recompute.
#[derive(Clone, Copy, Debug)]
pub struct TextBand {
    pub y_start: f32,
    pub y_end: f32,
}

impl TextBand {
    pub fn center_y(&self) -> f32 {
        (self.y_start + self.y_end) * 0.5
    }
    pub fn height(&self) -> f32 {
        self.y_end - self.y_start
    }
    /// True when `y` (image-space) sits inside this band's vertical
    /// extent — used by both the snap-on-drag path and the hover
    /// preview to test whether the pointer is "in" a text row.
    pub fn contains_y(&self, y: f32) -> bool {
        y >= self.y_start && y < self.y_end
    }
}

/// Brightness delta (out of 255) between two horizontally-spaced
/// samples that counts as a "glyph edge." Set low enough to catch
/// subdued UI text (light gray on slightly-lighter gray hits ~25
/// delta) while ignoring JPEG artifacts and antialiased gradients
/// (≤10 delta).
const EDGE_DELTA: i32 = 22;

/// Horizontal stride (image px) between the two samples whose
/// luminance we diff. Two pixels apart catches most glyph stroke
/// edges (stroke widths run 1–3 image px in body text); larger
/// strides start missing thin strokes.
const EDGE_SAMPLE_STRIDE: i32 = 2;

/// Fraction of sampled columns per row required for the row to flip
/// to "text-like." Text rows on a typical screenshot land around
/// 8–25 % edge density; solid-color rows are ~0; a single button
/// boundary contributes only 1–2 edges per row (well below 3 %).
const TEXT_DENSITY_THRESHOLD: f32 = 0.03;

/// Maximum vertical gap (in rows) between adjacent text-like rows
/// that still gets merged into a single band. Bridges the intra-glyph
/// gaps inside `e`, `a`, `o`, the gap between an ascender and the
/// x-height body of letters, and antialiased ramp rows whose density
/// drops below `TEXT_DENSITY_THRESHOLD`. Set generously (6 px) so a
/// single line of text doesn't fragment into a stack of partial
/// bands — that fragmentation was the root cause of "missed" text
/// lines that the detector silently dropped because each fragment
/// fell below `MIN_BAND_HEIGHT`.
const VERTICAL_MERGE_GAP: usize = 6;

/// Reject bands whose total height (in rows) falls outside these
/// bounds. Below 6 is single-pixel underlines / hairlines; above 60
/// is typically a solid-color UI block (sidebar, toolbar), not text.
/// Sized to comfortably accept body text at 1×–2× HiDPI.
const MIN_BAND_HEIGHT: usize = 6;
const MAX_BAND_HEIGHT: usize = 60;

/// Fraction of the detected band height added on EACH side at the
/// *consume* site (cursor height override, locked-stroke
/// `forced_width`) — NOT to the globally cached band list. The
/// contrast scan only catches rows that hit the density threshold,
/// which lops off the very top/bottom of glyphs where antialiased
/// edges fade below threshold — typical underestimate of a couple
/// percent per side. Proportional padding scales with text size:
/// big headings get a comfortable margin; small body text gets a
/// proportionally subtler one. Caching stays tight so band lookup
/// remains unambiguous.
pub const BAND_PAD_PERCENT_PER_SIDE: f32 = 0.05;

/// Horizontal subsample stride. Reading every pixel of a 4K
/// screenshot is ~8 M reads; sampling every 4th column is plenty for
/// the contrast statistic (text glyph features are wider than a
/// single column) and brings the scan well under 30 ms on a 4K
/// capture.
const COL_STRIDE: i32 = 4;

static BANDS: OnceLock<Vec<TextBand>> = OnceLock::new();

// Pixbuf isn't `Send` (it's a GObject pinned to the GTK main thread),
// so the cached reference lives in a thread-local. Only the main
// thread ever calls into this module, so this is sound.
thread_local! {
    static IMAGE: RefCell<Option<Pixbuf>> = const { RefCell::new(None) };
}

/// Run the per-row contrast scan once and stash the result for the
/// Highlighter tool to read. Idempotent — additional calls after the
/// first are no-ops (the heuristic depends only on the *original*
/// screenshot; the user's annotations don't change the bands).
pub fn init_from_pixbuf(pixbuf: &Pixbuf) {
    IMAGE.with(|cell| {
        *cell.borrow_mut() = Some(pixbuf.clone());
    });
    if BANDS.get().is_some() {
        return;
    }
    let bands = detect(pixbuf);
    // Temporary diagnostic — dump every band so we can correlate
    // against the source image when the cursor previews land in the
    // wrong row. Remove once detection is tuned.
    eprintln!(
        "text_bands: image {}x{} → {} bands",
        pixbuf.width(),
        pixbuf.height(),
        bands.len()
    );
    for (i, b) in bands.iter().enumerate() {
        eprintln!(
            "  [{:2}] y=[{:.0}..{:.0}] h={:.0}",
            i,
            b.y_start,
            b.y_end,
            b.height()
        );
    }
    let _ = BANDS.set(bands);
}

/// Read the cached bands. Returns an empty slice if detection hasn't
/// run yet (shouldn't happen in normal startup order — main.rs calls
/// `init_from_pixbuf` before the toolbar is built — but lets the
/// Highlighter tool boot defensively).
pub fn bands() -> &'static [TextBand] {
    BANDS.get().map(|v| v.as_slice()).unwrap_or(&[])
}

#[derive(Clone)]
struct BandCacheEntry {
    band: TextBand,
    cache_x: f32,
}

thread_local! {
    // Hysteresis cache: per-hover detection is sensitive to sub-pixel
    // cursor motion (the windowed column set shifts, a row right at
    // density threshold can flip text/non-text, the band's edges
    // jitter by a row). Caching last-known band stops that flicker
    // when the cursor wiggles inside what's clearly still the same
    // text row. Invalidates when the cursor moves out of the band's
    // y extent OR drifts more than CACHE_HORIZONTAL_RADIUS_PX from
    // the x at which the cache entry was created — the latter so a
    // horizontal slide onto an adjacent text element with different
    // height (e.g. "Hello there friends…" → "Restore Defaults") still
    // re-runs detection at the new location.
    static BAND_CACHE: RefCell<Option<BandCacheEntry>> =
        const { RefCell::new(None) };
}

/// Horizontal travel (image px) past which the hysteresis cache is
/// invalidated. Set generously so cursor positioning feels stable
/// — once the user has hovered onto a text row, small horizontal
/// wiggles inside (or just outside) that row stay locked to the
/// row. Crossing to a different column-aligned text element
/// (40+ px away in a typical UI layout) still re-detects.
const CACHE_HORIZONTAL_RADIUS_PX: f32 = 60.0;

/// How far past the cached band's y extent the cursor can wander
/// (image px) before the cache invalidates. Without this slack, a
/// tiny vertical wiggle into the gap above/below a row would knock
/// the cache out and force a fresh detection — which would re-snap
/// to (possibly) a different band and the cursor would visibly jump.
/// Set close to the snap-distance the local detector uses, so the
/// cache stays valid across the whole region where the cursor would
/// have snapped to this band anyway.
const CACHE_VERTICAL_SLACK_PX: f32 = 20.0;

/// Clear the hysteresis cache. Called when the highlighter tool
/// commits a stroke / releases — the next hover should re-evaluate
/// from scratch in case the user moved the pointer significantly
/// while drawing.
pub fn clear_local_band_cache() {
    BAND_CACHE.with(|c| *c.borrow_mut() = None);
}

/// Local edge-density detection around `(x, y)` with hysteresis.
/// Scans a small horizontal × vertical window centered on the
/// cursor, finds the nearest text-like row, and grows the band by
/// walking outward (with `VERTICAL_MERGE_GAP` bridging) until the
/// run ends. The window's HALF_W limits how many horizontal
/// neighbors contribute to each row's edge density, so a button
/// (with text on one side and a colored backdrop on the other)
/// snaps to the text element directly under the cursor rather than
/// averaging across the entire row.
///
/// The thread-local cache fixes the "jumpy cursor" symptom: when
/// the cursor wiggles inside a text row, the cached band is reused
/// without re-running detection, so sub-pixel density noise can't
/// shift the reported band's edges frame to frame. Cache flushes
/// when the cursor crosses the band's y range or drifts past the
/// horizontal radius — both signal "the cursor is somewhere new."
pub fn detect_local_band(x: f32, y: f32) -> Option<TextBand> {
    let cached = BAND_CACHE.with(|c| c.borrow().clone());
    if let Some(entry) = cached
        && (x - entry.cache_x).abs() <= CACHE_HORIZONTAL_RADIUS_PX
    {
        // Cache stays valid for the whole magnetic zone: inside the
        // band, or up to CACHE_VERTICAL_SLACK_PX past either edge.
        // That's the region in which a fresh detection would *still*
        // have snapped to this same band — pinning it via the cache
        // just makes the snap stable instead of recomputing (and
        // potentially flickering between adjacent bands) every motion
        // sample.
        let dy_outside = if y < entry.band.y_start {
            entry.band.y_start - y
        } else if y >= entry.band.y_end {
            y - entry.band.y_end
        } else {
            0.0
        };
        if dy_outside <= CACHE_VERTICAL_SLACK_PX {
            return Some(entry.band);
        }
    }
    let fresh = detect_local_band_uncached(x, y);
    BAND_CACHE.with(|c| {
        *c.borrow_mut() = fresh.map(|band| BandCacheEntry { band, cache_x: x });
    });
    fresh
}

/// Uncached body of `detect_local_band` — does the actual windowed
/// scan. The wrapper above adds the hysteresis cache.
fn detect_local_band_uncached(x: f32, y: f32) -> Option<TextBand> {
    IMAGE.with(|cell| -> Option<TextBand> {
        let pixbuf_ref = cell.borrow();
        let pixbuf = pixbuf_ref.as_ref()?;
        let img_w = pixbuf.width();
        let img_h = pixbuf.height();
        if !(0.0..img_w as f32).contains(&x) || !(0.0..img_h as f32).contains(&y) {
            return None;
        }
        // Window half-extents in image pixels. The horizontal half
        // limits how far the per-row density samples reach — narrow
        // enough that adjacent text elements at the same y but
        // different x don't pollute each other's signal, wide enough
        // that a 60–80 px tall capital glyph still has plenty of
        // horizontal samples for a meaningful density. The vertical
        // half must cover `SNAP_DISTANCE_PX` (so out-of-band hovers
        // can still find the nearest text run) plus `MAX_BAND_HEIGHT`
        // (so the run we find can be fully measured).
        const HALF_W: i32 = 80;
        const HALF_H: i32 = (SNAP_DISTANCE_ROWS + MAX_BAND_HEIGHT) as i32;

        let cx = x.round() as i32;
        let cy = y.round() as i32;
        let x0 = (cx - HALF_W).max(0);
        let x1 = (cx + HALF_W).min(img_w - 1 - EDGE_SAMPLE_STRIDE);
        if x1 < x0 {
            return None;
        }
        let y0 = (cy - HALF_H).max(0);
        let y1 = (cy + HALF_H).min(img_h - 1);

        let has_alpha = pixbuf.has_alpha();
        let bpp = if has_alpha { 4 } else { 3 };
        let stride_bytes = pixbuf.rowstride() as usize;
        let pixels = unsafe { pixbuf.pixels() };

        // Sample every 2nd column inside the window — same stride
        // the global detection uses so density thresholds stay
        // comparable.
        let cols: Vec<i32> = (x0..=x1).step_by(COL_STRIDE as usize).collect();
        let sample_count = cols.len().max(1) as f32;

        // Per-row edge density inside the local window.
        let mut row_text = vec![false; (y1 - y0 + 1) as usize];
        for (i, yy) in (y0..=y1).enumerate() {
            let row_offset = (yy as usize) * stride_bytes;
            let mut edges = 0;
            for &xx in &cols {
                let p1 = row_offset + (xx as usize) * bpp;
                let p2 = row_offset + ((xx + EDGE_SAMPLE_STRIDE) as usize) * bpp;
                let r1 = pixels[p1] as i32;
                let g1 = pixels[p1 + 1] as i32;
                let b1 = pixels[p1 + 2] as i32;
                let r2 = pixels[p2] as i32;
                let g2 = pixels[p2 + 1] as i32;
                let b2 = pixels[p2 + 2] as i32;
                let l1 = (299 * r1 + 587 * g1 + 114 * b1) / 1000;
                let l2 = (299 * r2 + 587 * g2 + 114 * b2) / 1000;
                if (l1 - l2).abs() >= EDGE_DELTA {
                    edges += 1;
                }
            }
            let density = edges as f32 / sample_count;
            row_text[i] = density >= TEXT_DENSITY_THRESHOLD;
        }

        // Anchor: nearest text-like row to the cursor (so the snap
        // works when the pointer is in a line gap). Search both
        // directions outward up to SNAP_DISTANCE_ROWS.
        let center_idx = (cy - y0) as usize;
        let mut anchor: Option<usize> = None;
        if row_text[center_idx] {
            anchor = Some(center_idx);
        } else {
            for r in 1..=SNAP_DISTANCE_ROWS {
                let down = center_idx + r;
                if down < row_text.len() && row_text[down] {
                    anchor = Some(down);
                    break;
                }
                let up = center_idx.checked_sub(r);
                if let Some(u) = up
                    && u < row_text.len()
                    && row_text[u]
                {
                    anchor = Some(u);
                    break;
                }
            }
        }
        let anchor_idx = anchor?;

        // Grow the band upward and downward from the anchor, with
        // small-gap bridging matching the global detector. Trim
        // trailing non-text rows so the band's edges sit on
        // actual text content.
        let mut top = anchor_idx;
        let mut gap = 0;
        while top > 0 {
            let next = top - 1;
            if row_text[next] {
                top = next;
                gap = 0;
            } else if gap < VERTICAL_MERGE_GAP {
                top = next;
                gap += 1;
            } else {
                break;
            }
        }
        while top < row_text.len() && !row_text[top] {
            top += 1;
        }

        let mut bot = anchor_idx;
        gap = 0;
        while bot + 1 < row_text.len() {
            let next = bot + 1;
            if row_text[next] {
                bot = next;
                gap = 0;
            } else if gap < VERTICAL_MERGE_GAP {
                bot = next;
                gap += 1;
            } else {
                break;
            }
        }
        while bot > 0 && !row_text[bot] {
            bot -= 1;
        }

        if top > bot {
            return None;
        }
        let h = bot - top + 1;
        if !(MIN_BAND_HEIGHT..=MAX_BAND_HEIGHT).contains(&h) {
            return None;
        }
        Some(TextBand {
            y_start: (y0 + top as i32) as f32,
            y_end: (y0 + (bot + 1) as i32) as f32,
        })
    })
}

/// How far (in image-px rows) the local detector will search away
/// from the cursor's row to find a text-like anchor. Same intent as
/// `SNAP_DISTANCE_PX` on the global path.
const SNAP_DISTANCE_ROWS: usize = 25;

/// Per-row edge-density scan → global band grouping. Internal; the
/// public entry point is `init_from_pixbuf`.
fn detect(pixbuf: &Pixbuf) -> Vec<TextBand> {
    let width = pixbuf.width();
    let height = pixbuf.height();
    if width < 2 || height < MIN_BAND_HEIGHT as i32 {
        return Vec::new();
    }
    let has_alpha = pixbuf.has_alpha();
    let bpp = if has_alpha { 4 } else { 3 };
    let stride = pixbuf.rowstride() as usize;
    // Read directly from the pixbuf's raw byte buffer. The pixbuf
    // is alive for as long as the app runs (the renderer holds a
    // strong reference), so this borrow is sound for the duration
    // of the scan.
    let pixels = unsafe { pixbuf.pixels() };
    // Pre-collect sampled column indices and their offset partners
    // (x + EDGE_SAMPLE_STRIDE) so each row's inner loop is just two
    // pointer reads + a diff. The partner must stay inside `width`,
    // so we trim the right edge.
    let sampled_cols: Vec<i32> = (0..width.saturating_sub(EDGE_SAMPLE_STRIDE))
        .step_by(COL_STRIDE as usize)
        .collect();
    let sample_count = sampled_cols.len().max(1) as f32;

    let lum = |offset: usize| -> i32 {
        let r = pixels[offset] as i32;
        let g = pixels[offset + 1] as i32;
        let b = pixels[offset + 2] as i32;
        (299 * r + 587 * g + 114 * b) / 1000
    };

    let mut row_is_text = vec![false; height as usize];
    for y in 0..height {
        let row_offset = (y as usize) * stride;
        let mut edges = 0;
        for &x in &sampled_cols {
            let p1 = row_offset + (x as usize) * bpp;
            let p2 = row_offset + ((x + EDGE_SAMPLE_STRIDE) as usize) * bpp;
            if (lum(p1) - lum(p2)).abs() >= EDGE_DELTA {
                edges += 1;
            }
        }
        let density = edges as f32 / sample_count;
        row_is_text[y as usize] = density >= TEXT_DENSITY_THRESHOLD;
    }

    // Group contiguous text rows (allowing small gaps) into bands.
    let mut bands: Vec<TextBand> = Vec::new();
    let mut y = 0usize;
    let n = row_is_text.len();
    while y < n {
        if !row_is_text[y] {
            y += 1;
            continue;
        }
        let start = y;
        let mut last_true = y;
        y += 1;
        while y < n {
            if row_is_text[y] {
                last_true = y;
                y += 1;
            } else {
                // Tolerate up to `VERTICAL_MERGE_GAP` consecutive
                // non-text rows before declaring the band finished.
                let gap_start = y;
                while y < n && !row_is_text[y] && (y - gap_start) < VERTICAL_MERGE_GAP {
                    y += 1;
                }
                if y < n && row_is_text[y] {
                    // Gap was bridged — continue the band.
                    last_true = y;
                    y += 1;
                } else {
                    // Gap too wide; band ends at last_true.
                    break;
                }
            }
        }
        let end = last_true + 1;
        let h = end - start;
        if (MIN_BAND_HEIGHT..=MAX_BAND_HEIGHT).contains(&h) {
            bands.push(TextBand {
                y_start: start as f32,
                y_end: end as f32,
            });
        }
    }
    bands
}
