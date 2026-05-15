//! Inverse-mask "spotlight" tool. The shape the user draws stays as
//! the original screenshot; everything outside the union of all
//! spotlight shapes is darkened by a global, slider-controlled alpha
//! (see `Style::spotlight_darkness`). Renderer-specific notes:
//!
//! - Spotlight drawables' `draw()` is intentionally a no-op. They
//!   participate via `append_spotlight_path()`, which the renderer
//!   collects in a single end-of-frame pass to punch holes in the
//!   dark overlay.
//! - Darkness is not stored per-drawable. Sliding the toolbar slider
//!   updates ALL spotlights at once because the renderer reads
//!   `sketch_board.style.spotlight_darkness` at render time.

use std::ops::{Add, Sub};

use anyhow::Result;
use femtovg::{Paint, Path};

use relm4::{
    Sender,
    gtk::gdk::{Key, ModifierType},
};

use crate::{
    configuration::APP_CONFIG,
    math::{self, Rect, Vec2D, point_to_segment_distance},
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    style::Style,
    tools::DrawableClone,
};

use super::{
    Drawable, GLOW_COLOR, Handle, HandleId, Tool, ToolUpdateResult, Tools, bbox_handles,
    bbox_resize, halo_in_image_units,
};

#[derive(Clone, Debug)]
struct BlockSpotlight {
    top_left: Vec2D,
    size: Option<Vec2D>,
}

#[derive(Clone, Debug)]
struct FreehandSpotlight {
    /// First point is absolute; subsequent points are *offsets from
    /// the first*, matching the highlighter's storage layout so the
    /// translate-by-moving-first-point trick works the same way.
    points: Vec<Vec2D>,
    shift_pressed: bool,
}

#[derive(Clone, Debug)]
struct Spotlighter<T> {
    data: T,
    style: Style,
}

#[derive(Clone, Debug)]
enum SpotlightKind {
    Block(Spotlighter<BlockSpotlight>),
    Freehand(Spotlighter<FreehandSpotlight>),
}

impl SpotlightKind {
    fn freehand_thickness(style: &Style) -> f32 {
        style
            .size
            .to_highlight_width(style.annotation_size_factor)
    }
}

impl Drawable for SpotlightKind {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn kind_label(&self) -> &'static str {
        "Spotlight"
    }
    fn icon_name(&self) -> &'static str {
        "flashlight-regular"
    }
    fn panel_swatch(&self) -> crate::tools::PanelSwatch {
        // Spotlight's effect is a dim overlay with a "highlighted"
        // cutout — render that exact metaphor in the swatch instead
        // of trying to express it as a single color.
        crate::tools::PanelSwatch::SpotlightOverlay
    }

    /// Spotlights render in a separate pass at the end of the frame
    /// (see `FemtoVgAreaMut::render`); their main-pass `draw` is a
    /// no-op so they don't render twice and so the dark overlay sits
    /// on top of every other annotation.
    fn draw(
        &self,
        _canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: femtovg::FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        Ok(())
    }

    fn is_spotlight(&self) -> bool {
        true
    }

    /// Contribute this shape to the renderer's punch-out mask. Block
    /// spotlights add a rounded rectangle; freehand spotlights add a
    /// thick polyline reified as a filled stroke (the user expects
    /// the visible spotlight region to match the stroke outline).
    fn append_spotlight_path(&self, path: &mut Path) {
        match self {
            SpotlightKind::Block(s) => {
                let Some(size) = s.data.size else { return };
                let (pos, size) = math::rect_ensure_positive_size(s.data.top_left, size);
                path.rounded_rect(
                    pos.x,
                    pos.y,
                    size.x,
                    size.y,
                    APP_CONFIG.read().corner_roundness(),
                );
            }
            SpotlightKind::Freehand(s) => {
                let Some(first) = s.data.points.first().copied() else {
                    return;
                };
                let half = Self::freehand_thickness(&s.style) / 2.0;
                if s.data.points.len() < 2 {
                    // Single point — drop a circle so a click-and-tap
                    // still produces a visible spotlight.
                    let r = half.max(1.0);
                    path.arc(first.x, first.y, r, 0.0, std::f32::consts::TAU, femtovg::Solidity::Solid);
                    return;
                }
                // Reify the polyline as a filled outline by walking
                // the centerline twice — once forward expanded
                // outward, once backward — so the final fill exactly
                // covers the visible stroke. Matches what the
                // highlighter painter produces visually.
                let mut prev = first;
                let mut forward: Vec<Vec2D> = Vec::with_capacity(s.data.points.len());
                let mut backward: Vec<Vec2D> = Vec::with_capacity(s.data.points.len());
                forward.push(first);
                backward.push(first);
                for p in s.data.points.iter().skip(1) {
                    let cur = first + *p;
                    let dir = cur - prev;
                    let len = (dir.x * dir.x + dir.y * dir.y).sqrt().max(0.0001);
                    let nx = -dir.y / len;
                    let ny = dir.x / len;
                    forward.push(Vec2D::new(cur.x + nx * half, cur.y + ny * half));
                    backward.push(Vec2D::new(cur.x - nx * half, cur.y - ny * half));
                    prev = cur;
                }
                path.move_to(forward[0].x, forward[0].y);
                for v in forward.iter().skip(1) {
                    path.line_to(v.x, v.y);
                }
                for v in backward.iter().rev() {
                    path.line_to(v.x, v.y);
                }
                path.close();
            }
        }
    }

    fn bounds(&self) -> Option<Rect> {
        match self {
            SpotlightKind::Block(s) => s.data.size.map(|sz| Rect::new(s.data.top_left, sz)),
            SpotlightKind::Freehand(s) => {
                if s.data.points.len() < 2 {
                    return None;
                }
                let first = *s.data.points.first()?;
                let mut min = first;
                let mut max = first;
                for p in s.data.points.iter().skip(1) {
                    let abs = first + *p;
                    min.x = min.x.min(abs.x);
                    min.y = min.y.min(abs.y);
                    max.x = max.x.max(abs.x);
                    max.y = max.y.max(abs.y);
                }
                let stroke = Self::freehand_thickness(&s.style);
                Some(
                    Rect {
                        pos: min,
                        size: max - min,
                    }
                    .inflated(stroke / 2.0),
                )
            }
        }
    }

    fn hit_test(&self, point: Vec2D, tolerance: f32) -> bool {
        match self {
            SpotlightKind::Block(_) => {
                // Edge-only hit test: the spotlight's interior is
                // meant to stay clickable so the user can keep
                // drawing annotations INSIDE a framed region. The
                // pickable zone is a band on either side of the
                // rectangle's boundary — wide enough to grab
                // without precise aim, narrow enough that a click
                // a few pixels in still lands on whatever tool is
                // active. Matches the non-filled-rectangle hit
                // semantics in `rectangle.rs`.
                let Some(rect) = self.bounds() else {
                    return false;
                };
                // Spotlight has no rendered stroke; pick a band
                // wide enough to feel like a real edge target. The
                // selection halo (`halo_in_image_units`) draws at a
                // similar width, so this also aligns visually with
                // the glow ring that appears on selection.
                const EDGE_BAND: f32 = 8.0;
                let half = EDGE_BAND + tolerance;
                if !rect.inflated(half).contains(point) {
                    return false;
                }
                let inner_w = rect.size.x - 2.0 * half;
                let inner_h = rect.size.y - 2.0 * half;
                // Tiny rect with no hollow interior — the band fills
                // the whole footprint, so any point in the outer
                // counts.
                if inner_w <= 0.0 || inner_h <= 0.0 {
                    return true;
                }
                let inner = Rect {
                    pos: Vec2D::new(rect.pos.x + half, rect.pos.y + half),
                    size: Vec2D::new(inner_w, inner_h),
                };
                !inner.contains(point)
            }
            SpotlightKind::Freehand(s) => {
                let Some(first) = s.data.points.first().copied() else {
                    return false;
                };
                if s.data.points.len() < 2 {
                    return false;
                }
                let stroke = Self::freehand_thickness(&s.style);
                let pick = stroke / 2.0 + tolerance;
                let mut prev = first;
                for p in s.data.points.iter().skip(1) {
                    let cur = first + *p;
                    if point_to_segment_distance(point, prev, cur) <= pick {
                        return true;
                    }
                    prev = cur;
                }
                false
            }
        }
    }

    fn translate(&mut self, delta: Vec2D) {
        match self {
            SpotlightKind::Block(s) => s.data.top_left += delta,
            SpotlightKind::Freehand(s) => {
                if let Some(first) = s.data.points.first_mut() {
                    *first += delta;
                }
            }
        }
    }

    fn handles(&self) -> Vec<Handle> {
        match self {
            SpotlightKind::Block(_) => self.bounds().map(bbox_handles).unwrap_or_default(),
            SpotlightKind::Freehand(_) => Vec::new(),
        }
    }

    fn move_handle(&mut self, handle: HandleId, to: Vec2D) {
        let SpotlightKind::Block(s) = self else { return };
        let Some(size) = s.data.size else { return };
        let cur = Rect::new(s.data.top_left, size);
        let new = bbox_resize(cur, handle, to);
        s.data.top_left = new.pos;
        s.data.size = Some(new.size);
    }

    fn set_style(&mut self, style: Style) {
        match self {
            SpotlightKind::Block(s) => s.style = style,
            SpotlightKind::Freehand(s) => s.style = style,
        }
    }

    fn style(&self) -> Option<Style> {
        Some(match self {
            SpotlightKind::Block(s) => s.style,
            SpotlightKind::Freehand(s) => s.style,
        })
    }

    fn tool_type(&self) -> Option<Tools> {
        Some(Tools::Spotlight)
    }

    fn render_glow(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: femtovg::FontId,
        _bounds: (Vec2D, Vec2D),
        device_pixel_ratio: f32,
    ) -> anyhow::Result<()> {
        let halo = halo_in_image_units(canvas, device_pixel_ratio);
        if let SpotlightKind::Block(_) = self
            && let Some(rect) = self.bounds()
        {
            let inflate = halo / 2.0;
            canvas.save();
            let mut path = Path::new();
            path.rounded_rect(
                rect.pos.x - inflate,
                rect.pos.y - inflate,
                rect.size.x + inflate * 2.0,
                rect.size.y + inflate * 2.0,
                APP_CONFIG.read().corner_roundness() + inflate,
            );
            let mut paint = Paint::color(GLOW_COLOR);
            paint.set_line_width(halo);
            paint.set_line_join(femtovg::LineJoin::Round);
            canvas.stroke_path(&path, &paint);
            canvas.restore();
            return Ok(());
        }
        let Some(b) = self.bounds() else {
            return Ok(());
        };
        let inflate = halo / 2.0;
        canvas.save();
        let mut path = Path::new();
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

#[derive(Default, Clone, Debug)]
pub struct SpotlightTool {
    spotlight: Option<SpotlightKind>,
    style: Style,
    input_enabled: bool,
    sender: Option<Sender<SketchBoardInput>>,
}

impl Tool for SpotlightTool {
    fn input_enabled(&self) -> bool {
        self.input_enabled
    }

    fn set_input_enabled(&mut self, value: bool) {
        self.input_enabled = value;
    }

    fn get_tool_type(&self) -> super::Tools {
        Tools::Spotlight
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        let shift_pressed = event.modifier.intersects(ModifierType::SHIFT_MASK);
        let ctrl_pressed = event.modifier.intersects(ModifierType::CONTROL_MASK);
        // Reuse the user's primary highlighter preference for the
        // freehand-vs-block default — same gesture, same semantics, no
        // need for a separate config knob. CTRL still flips it.
        let primary_block = matches!(
            APP_CONFIG.read().primary_highlighter(),
            super::highlight::Highlighters::Block
        );
        match event.type_ {
            MouseEventType::BeginDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }
                let want_block = primary_block ^ ctrl_pressed;
                if want_block {
                    self.spotlight = Some(SpotlightKind::Block(Spotlighter {
                        data: BlockSpotlight {
                            top_left: event.pos,
                            size: None,
                        },
                        style: self.style,
                    }));
                } else {
                    self.spotlight = Some(SpotlightKind::Freehand(Spotlighter {
                        data: FreehandSpotlight {
                            points: vec![event.pos],
                            shift_pressed,
                        },
                        style: self.style,
                    }));
                }
                ToolUpdateResult::Redraw
            }
            MouseEventType::UpdateDrag | MouseEventType::EndDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }
                if self.spotlight.is_none() {
                    return ToolUpdateResult::Unmodified;
                }
                let mut kind = self.spotlight.as_mut().unwrap();
                let update: ToolUpdateResult = match &mut kind {
                    SpotlightKind::Block(s) => {
                        if shift_pressed {
                            let max_size = event.pos.x.abs().max(event.pos.y.abs());
                            s.data.size = Some(Vec2D {
                                x: max_size * event.pos.x.signum(),
                                y: max_size * event.pos.y.signum(),
                            });
                        } else {
                            s.data.size = Some(event.pos);
                        };
                        ToolUpdateResult::Redraw
                    }
                    SpotlightKind::Freehand(s) => {
                        if event.pos == Vec2D::zero() {
                            return ToolUpdateResult::Unmodified;
                        };
                        if shift_pressed {
                            if s.data.shift_pressed && s.data.points.len() >= 2 {
                                s.data
                                    .points
                                    .pop()
                                    .expect("at least 2 points in spotlight path.");
                            };
                            let last = if s.data.points.len() == 1 {
                                Vec2D::zero()
                            } else {
                                *s.data.points.last_mut().expect("at least one point")
                            };
                            let snapped_pos = event.pos.sub(last).snapped_vector_15deg().add(last);
                            s.data.points.push(snapped_pos);
                        } else {
                            s.data.points.push(event.pos);
                        }
                        s.data.shift_pressed = shift_pressed;
                        ToolUpdateResult::Redraw
                    }
                };
                if event.type_ == MouseEventType::UpdateDrag {
                    return update;
                };
                let result = kind.clone_box();
                self.spotlight = None;
                ToolUpdateResult::Commit(result)
            }
            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn handle_key_event(&mut self, event: crate::sketch_board::KeyEventMsg) -> ToolUpdateResult {
        if event.key == Key::Escape && self.spotlight.is_some() {
            self.spotlight = None;
            return ToolUpdateResult::Redraw;
        }
        ToolUpdateResult::Unmodified
    }

    fn handle_key_release_event(
        &mut self,
        event: crate::sketch_board::KeyEventMsg,
    ) -> ToolUpdateResult {
        if (event.key == Key::Shift_L || event.key == Key::Shift_R)
            && let Some(SpotlightKind::Freehand(s)) = &mut self.spotlight
        {
            let points = &mut s.data.points;
            let last = points
                .last()
                .expect("freehand spotlight must have at least one point");
            if points.len() >= 2 {
                if *last == points[points.len() - 2] {
                    points.pop();
                } else {
                    points.push(*last);
                }
                return ToolUpdateResult::Redraw;
            };
        };
        ToolUpdateResult::Unmodified
    }

    fn handle_style_event(&mut self, style: Style) -> ToolUpdateResult {
        self.style = style;
        // Slider changes update the global darkness — every existing
        // spotlight needs to repaint so the overlay follows the slider
        // in real time.
        ToolUpdateResult::Redraw
    }

    fn get_drawable(&self) -> Option<&dyn Drawable> {
        match &self.spotlight {
            Some(d) => Some(d),
            None => None,
        }
    }

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>) {
        self.sender = Some(sender);
    }
}
