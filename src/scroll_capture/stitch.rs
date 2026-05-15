use anyhow::{Result, bail};
use relm4::gtk::gdk_pixbuf::{Colorspace, Pixbuf};
use relm4::gtk::glib::Bytes;

/// Number of rows compared per offset candidate. Bigger = more accurate
/// match within the search window, slower. Keep modest; we're refining
/// within a narrow ±RADIUS range, so a small band is plenty discriminating.
const BAND_HEIGHT: usize = 32;

/// Pixels from the top of the frame we skip before starting the band.
const BAND_TOP: usize = 16;

/// How far around the caller's hint we search for the actual scroll
/// offset, in pixels. The hint comes from "we sent N Down-arrow keys so
/// the page scrolled approximately Y physical px"; reality usually lands
/// within ±SEARCH_RADIUS of that, accounting for browser line heights,
/// page zoom, and sub-pixel rendering.
const SEARCH_RADIUS: usize = 60;

/// Stitch captured frames into one tall Pixbuf. The first frame is included
/// in full; for each subsequent frame the scroll delta is refined by SAD
/// search *within a narrow window of `expected_delta`*, then only the
/// newly-revealed bottom rows are appended.
pub fn stitch(frames: &[Pixbuf], expected_delta: usize) -> Result<Pixbuf> {
    if frames.is_empty() {
        bail!("nothing to stitch (no frames captured)");
    }
    if frames.len() == 1 {
        return Ok(frames[0].clone());
    }

    let width = frames[0].width();
    let height = frames[0].height();
    let row_bytes = (width as usize) * 4;
    if !frames.iter().all(|f| f.width() == width && f.height() == height) {
        bail!("frames have inconsistent dimensions");
    }

    let pixels: Vec<Vec<u8>> = frames
        .iter()
        .map(|p| p.read_pixel_bytes().as_ref().to_vec())
        .collect();
    let rowstrides: Vec<usize> = frames.iter().map(|p| p.rowstride() as usize).collect();
    let h = height as usize;

    // First frame contributes its full height. Each subsequent frame
    // contributes a `delta` band of newly-revealed bottom rows.
    let mut deltas: Vec<usize> = Vec::with_capacity(frames.len());
    deltas.push(h);
    for i in 1..frames.len() {
        let dy = find_scroll_offset(
            &pixels[i - 1], rowstrides[i - 1],
            &pixels[i], rowstrides[i],
            width as usize, h,
            expected_delta,
        );
        deltas.push(dy);
    }
    let total_h: usize = deltas.iter().sum();
    if total_h > i32::MAX as usize {
        bail!("stitched image too tall ({total_h} px) for Pixbuf");
    }

    let mut out = vec![0u8; total_h * row_bytes];
    let mut out_y = 0usize;

    for row in 0..h {
        let src_off = row * rowstrides[0];
        out[out_y * row_bytes..out_y * row_bytes + row_bytes]
            .copy_from_slice(&pixels[0][src_off..src_off + row_bytes]);
        out_y += 1;
    }

    for i in 1..frames.len() {
        let delta = deltas[i];
        if delta == 0 {
            continue;
        }
        let stride = rowstrides[i];
        for row in (h - delta)..h {
            let src_off = row * stride;
            out[out_y * row_bytes..out_y * row_bytes + row_bytes]
                .copy_from_slice(&pixels[i][src_off..src_off + row_bytes]);
            out_y += 1;
        }
    }

    let bytes = Bytes::from_owned(out);
    Ok(Pixbuf::from_bytes(
        &bytes,
        Colorspace::Rgb,
        true,
        8,
        width,
        total_h as i32,
        row_bytes as i32,
    ))
}

/// Refine the scroll offset around a `hint`. Searches `hint ± SEARCH_RADIUS`
/// only, so we skip the sticky-header region's spurious local minima at
/// small offsets and avoid the O(height) cost of a full search.
fn find_scroll_offset(
    prev: &[u8],
    prev_stride: usize,
    cur: &[u8],
    cur_stride: usize,
    width: usize,
    height: usize,
    hint: usize,
) -> usize {
    let row_bytes = width * 4;
    let band_top = BAND_TOP.min(height / 4);
    let band_height = BAND_HEIGHT.min(height / 4).max(8);
    let absolute_max = height.saturating_sub(band_top + band_height + 1);

    let min_off = hint.saturating_sub(SEARCH_RADIUS).max(4);
    let max_off = hint.saturating_add(SEARCH_RADIUS).min(absolute_max);
    if min_off > max_off {
        return hint.min(absolute_max);
    }

    let mut best_offset = hint.min(absolute_max);
    let mut best_sad = u64::MAX;

    for offset in min_off..=max_off {
        let mut sad: u64 = 0;
        for row in 0..band_height {
            let prev_row_idx = band_top + offset + row;
            let cur_row_idx = band_top + row;
            if prev_row_idx >= height || cur_row_idx >= height {
                continue;
            }
            let prev_off = prev_row_idx * prev_stride;
            let cur_off = cur_row_idx * cur_stride;
            let pr = &prev[prev_off..prev_off + row_bytes];
            let cr = &cur[cur_off..cur_off + row_bytes];
            for (a, b) in pr.iter().zip(cr.iter()) {
                sad += (*a as i32 - *b as i32).unsigned_abs() as u64;
            }
        }
        if sad < best_sad {
            best_sad = sad;
            best_offset = offset;
        }
    }

    best_offset
}
