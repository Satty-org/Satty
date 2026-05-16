use anyhow::{Result, bail};
use relm4::gtk::gdk_pixbuf::{Colorspace, Pixbuf};
use relm4::gtk::glib::Bytes;

/// Width-downsample factor for the SAD matching pass. The full-resolution
/// frames stay untouched for the final stitch; SAD only needs enough
/// horizontal signal to discriminate the right scroll offset. Going from
/// 4-byte RGBA at full width to 1-byte grayscale at 1/4 width is a 16x
/// reduction in band-comparison work, dropping a 10 s stitch down to
/// under 1 s on typical 1500-px-wide selections.
const DOWNSAMPLE_W: usize = 4;

/// Two band positions for the multi-band SAD. False matches at one band
/// (e.g., from repetitive layouts at the search-window's far end) rarely
/// also exist at a band sampled from a different y-position, so the joint
/// SAD has a sharper, more reliable minimum than a single band.
const BAND_TOP_FRAC_NUM: usize = 1;
const BAND_TOP_FRAC_DEN: usize = 4;
const BAND_TOP_2_FRAC_NUM: usize = 1;
const BAND_TOP_2_FRAC_DEN: usize = 2;

/// Each band's height as a fraction of the frame height. Smaller than the
/// previous single-band version because we have two of them.
const BAND_HEIGHT_FRAC_NUM: usize = 1;
const BAND_HEIGHT_FRAC_DEN: usize = 6;

/// Search ±N pixels around the hint. Wide enough to absorb per-cycle
/// scroll variance (smooth scrolling, partial paint-settle) without
/// drifting into the next page-row's repeated content.
const SEARCH_RADIUS: usize = 140;

/// Downsampled, grayscale view of a frame used only by SAD. Row major,
/// `width` grayscale bytes per row × `height` rows.
pub struct GrayView {
    pub pixels: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

pub fn downsample_to_gray(p: &Pixbuf) -> GrayView {
    let src_w = p.width() as usize;
    let src_h = p.height() as usize;
    let src_stride = p.rowstride() as usize;
    let src_bytes = p.read_pixel_bytes();
    let src = src_bytes.as_ref();

    let dst_w = src_w / DOWNSAMPLE_W;
    let mut dst = vec![0u8; dst_w * src_h];

    // Naive nearest-neighbour width downsample + RGB→gray (luminance).
    // The downsample is more about cutting band-comparison cost than
    // exact pixel fidelity; SAD only needs enough variance per row to
    // pick the right offset.
    for y in 0..src_h {
        let src_row = &src[y * src_stride..y * src_stride + src_w * 4];
        let dst_row = &mut dst[y * dst_w..y * dst_w + dst_w];
        for gx in 0..dst_w {
            let off = (gx * DOWNSAMPLE_W) * 4;
            let r = src_row[off] as u32;
            let g = src_row[off + 1] as u32;
            let b = src_row[off + 2] as u32;
            // Luminance (Rec. 601, integer approx): (77R + 150G + 29B) / 256.
            dst_row[gx] = ((r * 77 + g * 150 + b * 29) >> 8) as u8;
        }
    }
    GrayView {
        pixels: dst,
        width: dst_w,
        height: src_h,
    }
}

/// Stitch captured frames into one tall Pixbuf.
///
/// One frame per scroll cycle. For each pair we run multi-band SAD on
/// the downsampled grayscale view to measure that cycle's scroll delta,
/// then take the median across all pairs as the canonical per-cycle
/// delta (robust to a stray SAD false match on any one pair). Finally
/// we copy the bottom `delta` rows of each subsequent original frame
/// into the output.
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

    let h = height as usize;

    // Downsample once per frame. SAD reads only from the downsampled
    // grayscale; the original Pixbuf bytes are read once at the very end
    // when we write the output.
    let t_down = std::time::Instant::now();
    let gray: Vec<GrayView> = frames.iter().map(downsample_to_gray).collect();
    eprintln!(
        "scroll-capture: downsample (4x grayscale) {} frames in {:?}",
        gray.len(),
        t_down.elapsed()
    );

    // Per-pair multi-band SAD with running-average hint.
    let t_sad = std::time::Instant::now();
    let mut raw_deltas: Vec<usize> = Vec::with_capacity(frames.len() - 1);
    let mut raw_confs: Vec<f64> = Vec::with_capacity(frames.len() - 1);
    let mut hint = expected_delta;
    for i in 1..frames.len() {
        let (dy, conf) = find_scroll_offset_multiband(&gray[i - 1], &gray[i], hint);
        eprintln!("  pair {}: delta={} conf={:.3}", i, dy, conf);
        raw_deltas.push(dy);
        raw_confs.push(conf);
        hint = (hint + dy) / 2;
    }
    eprintln!(
        "scroll-capture: multi-band SAD {} pairs in {:?}",
        raw_deltas.len(),
        t_sad.elapsed()
    );

    // Confidence ratio threshold: above this, trust the per-pair measurement;
    // below it, the chosen offset has a close runner-up (often caused by
    // dynamic content showing/hiding mid-cycle on the underlying page), so
    // fall back to the median of the confident pairs.
    const CONF_THRESHOLD: f64 = 1.5;
    let confident_pairs: Vec<usize> = raw_deltas
        .iter()
        .zip(raw_confs.iter())
        .filter(|&(_, &c)| c > CONF_THRESHOLD)
        .map(|(&d, _)| d)
        .collect();
    let consensus_delta = if !confident_pairs.is_empty() {
        let mut sorted = confident_pairs.clone();
        sorted.sort_unstable();
        sorted[sorted.len() / 2]
    } else {
        // All pairs are ambiguous — fall back to median of everything.
        let mut sorted = raw_deltas.clone();
        sorted.sort_unstable();
        sorted[sorted.len() / 2]
    };
    eprintln!(
        "scroll-capture: stitch {} confident pairs (conf > {:.2}) → consensus delta {}",
        confident_pairs.len(),
        CONF_THRESHOLD,
        consensus_delta
    );

    // Build per-pair deltas: trust the measurement where SAD is confident,
    // substitute the consensus where it isn't. That preserves real
    // per-pair variance (e.g., partial smooth-scroll renders at end of
    // content) while neutralising SAD's misfires on ambiguous pairs.
    let mut deltas: Vec<usize> = Vec::with_capacity(frames.len());
    deltas.push(h);
    for i in 0..raw_deltas.len() {
        let chosen = if raw_confs[i] > CONF_THRESHOLD {
            raw_deltas[i]
        } else {
            consensus_delta
        };
        deltas.push(chosen.max(1).min(h));
    }
    eprintln!("scroll-capture: per-pair stitch deltas: {:?}", &deltas[1..]);

    let total_h: usize = deltas.iter().sum();
    if total_h > i32::MAX as usize {
        bail!("stitched image too tall ({total_h} px) for Pixbuf");
    }

    let t_copy = std::time::Instant::now();
    let mut out = vec![0u8; total_h * row_bytes];
    let mut out_y = 0usize;

    // First frame full.
    let stride0 = frames[0].rowstride() as usize;
    let p0_bytes = frames[0].read_pixel_bytes();
    let p0 = p0_bytes.as_ref();
    for row in 0..h {
        let src = row * stride0;
        out[out_y * row_bytes..out_y * row_bytes + row_bytes]
            .copy_from_slice(&p0[src..src + row_bytes]);
        out_y += 1;
    }
    // Subsequent frames: bottom `deltas[i]` rows each (per-pair).
    for (i, f) in frames[1..].iter().enumerate() {
        let delta = deltas[i + 1];
        if delta == 0 {
            continue;
        }
        let stride = f.rowstride() as usize;
        let p_bytes = f.read_pixel_bytes();
        let p = p_bytes.as_ref();
        for row in (h - delta)..h {
            let src = row * stride;
            out[out_y * row_bytes..out_y * row_bytes + row_bytes]
                .copy_from_slice(&p[src..src + row_bytes]);
            out_y += 1;
        }
    }
    eprintln!(
        "scroll-capture: copy-out {} rows in {:?}",
        out_y,
        t_copy.elapsed()
    );

    let bytes = Bytes::from_owned(out);
    let pixbuf = Pixbuf::from_bytes(
        &bytes,
        Colorspace::Rgb,
        true,
        8,
        width,
        total_h as i32,
        row_bytes as i32,
    );

    // Debug aid: write the stitched output to a known path so it can be
    // examined after the satty canvas closes (lost-clipboard scenario).
    let dbg_path = "/tmp/tensaku-scroll-capture-debug.png";
    let t_save = std::time::Instant::now();
    match pixbuf.savev(dbg_path, "png", &[]) {
        Ok(()) => eprintln!(
            "scroll-capture: debug-saved stitched output to {} in {:?}",
            dbg_path,
            t_save.elapsed()
        ),
        Err(e) => eprintln!("scroll-capture: failed to debug-save: {}", e),
    }

    Ok(pixbuf)
}

/// Multi-band SAD: evaluate two bands at different y-positions and pick
/// the offset minimizing the SUM. False matches at one band rarely line
/// up with false matches at another, so the joint minimum is much more
/// discriminative than single-band SAD. Returns `(offset, confidence)`
/// where confidence is the ratio of runner-up SAD to best SAD (≫ 1 means
/// the best offset is clearly the winner).
fn find_scroll_offset_multiband(
    prev: &GrayView,
    cur: &GrayView,
    hint: usize,
) -> (usize, f64) {
    let w = prev.width;
    let h = prev.height;
    let band_height = (h * BAND_HEIGHT_FRAC_NUM / BAND_HEIGHT_FRAC_DEN).max(32);
    let band_top_a = h * BAND_TOP_FRAC_NUM / BAND_TOP_FRAC_DEN;
    let band_top_b = h * BAND_TOP_2_FRAC_NUM / BAND_TOP_2_FRAC_DEN;
    let absolute_max =
        h.saturating_sub(band_height.max(band_top_a).max(band_top_b) + 1);

    let min_off = hint.saturating_sub(SEARCH_RADIUS).max(4);
    let max_off = hint.saturating_add(SEARCH_RADIUS).min(absolute_max);
    if min_off > max_off {
        return (hint.min(absolute_max), 1.0);
    }

    let mut best_offset = hint.min(absolute_max);
    let mut best_sad = u64::MAX;
    let mut runner_up_sad = u64::MAX;

    for offset in min_off..=max_off {
        let mut total: u64 = 0;
        for &band_top in &[band_top_a, band_top_b] {
            for row in 0..band_height {
                let cur_y = band_top + row;
                let prev_y = band_top + offset + row;
                if prev_y >= h || cur_y >= h {
                    continue;
                }
                let cr = &cur.pixels[cur_y * w..cur_y * w + w];
                let pr = &prev.pixels[prev_y * w..prev_y * w + w];
                total += sad_row(pr, cr);
            }
        }
        if total < best_sad {
            runner_up_sad = best_sad;
            best_sad = total;
            best_offset = offset;
        } else if total < runner_up_sad {
            runner_up_sad = total;
        }
    }

    let conf = if best_sad == 0 {
        f64::INFINITY
    } else {
        runner_up_sad as f64 / best_sad as f64
    };
    (best_offset, conf)
}

/// Byte-by-byte sum of absolute differences. The naive loop auto-
/// vectorises under `cargo build --release` (LLVM emits PSADBW on x86),
/// so we keep the body simple.
#[inline]
fn sad_row(a: &[u8], b: &[u8]) -> u64 {
    let mut s: u64 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        let d = if x >= y { x - y } else { y - x };
        s += d as u64;
    }
    s
}

/// Compute total multi-band SAD between `prev` and `cur` at a specific
/// scroll `offset`. Exposed so the capture-time dedup can compare SAD@0
/// (frames identical) against SAD@expected_delta (frames one cycle apart)
/// to decide whether the page actually scrolled.
pub fn sad_at_offset(prev: &GrayView, cur: &GrayView, offset: usize) -> u64 {
    let w = prev.width;
    let h = prev.height;
    let band_height = (h * BAND_HEIGHT_FRAC_NUM / BAND_HEIGHT_FRAC_DEN).max(32);
    let band_top_a = h * BAND_TOP_FRAC_NUM / BAND_TOP_FRAC_DEN;
    let band_top_b = h * BAND_TOP_2_FRAC_NUM / BAND_TOP_2_FRAC_DEN;

    let mut total: u64 = 0;
    for &band_top in &[band_top_a, band_top_b] {
        for row in 0..band_height {
            let cur_y = band_top + row;
            let prev_y = band_top + offset + row;
            if prev_y >= h || cur_y >= h {
                continue;
            }
            let cr = &cur.pixels[cur_y * w..cur_y * w + w];
            let pr = &prev.pixels[prev_y * w..prev_y * w + w];
            total += sad_row(pr, cr);
        }
    }
    total
}
