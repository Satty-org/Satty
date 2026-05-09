use std::ops::{Add, Sub};

use anyhow::Result;
use femtovg::{Paint, Path};

use relm4::{
    Sender,
    gtk::gdk::{Key, ModifierType},
};
use serde_derive::Deserialize;

use crate::{
    configuration::APP_CONFIG,
    math::{self, Rect, Vec2D, point_to_segment_distance},
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    style::Style,
    tools::DrawableClone,
};

use satty_cli::command_line;

use super::{
    Drawable, GLOW_COLOR, GLOW_STROKE_WIDTH, Handle, HandleId, Tool, ToolUpdateResult, Tools,
    bbox_handles, bbox_resize,
};

const HIGHLIGHT_OPACITY: f64 = 0.4;

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Highlighters {
    Block = 0,
    Freehand = 1,
}

impl From<command_line::Highlighters> for Highlighters {
    fn from(tool: command_line::Highlighters) -> Self {
        match tool {
            command_line::Highlighters::Block => Self::Block,
            command_line::Highlighters::Freehand => Self::Freehand,
        }
    }
}

#[derive(Clone, Debug)]
struct BlockHighlight {
    top_left: Vec2D,
    size: Option<Vec2D>,
}

#[derive(Clone, Debug)]
struct FreehandHighlight {
    points: Vec<Vec2D>,
    shift_pressed: bool,
}

#[derive(Clone, Debug)]
struct Highlighter<T> {
    data: T,
    style: Style,
}

trait Highlight {
    fn highlight(&self, canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>) -> Result<()>;
}

impl Highlight for Highlighter<FreehandHighlight> {
    fn highlight(&self, canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>) -> Result<()> {
        canvas.save();

        let mut path = Path::new();
        let first = self
            .data
            .points
            .first()
            .expect("should exist at least one point in highlight instance.");

        path.move_to(first.x, first.y);
        for p in self.data.points.iter().skip(1) {
            path.line_to(first.x + p.x, first.y + p.y);
        }

        let mut paint = Paint::color(femtovg::Color::rgba(
            self.style.color.r,
            self.style.color.g,
            self.style.color.b,
            (255.0 * HIGHLIGHT_OPACITY) as u8,
        ));
        paint.set_line_width(
            self.style
                .size
                .to_highlight_width(self.style.annotation_size_factor),
        );
        paint.set_line_join(femtovg::LineJoin::Round);
        paint.set_line_cap(femtovg::LineCap::Square);

        canvas.stroke_path(&path, &paint);
        canvas.restore();
        Ok(())
    }
}

impl Highlight for Highlighter<BlockHighlight> {
    fn highlight(&self, canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>) -> Result<()> {
        let size = match self.data.size {
            Some(s) => s,
            None => return Ok(()), // early exit if size is none
        };

        let (pos, size) = math::rect_ensure_positive_size(self.data.top_left, size);

        let mut shadow_path = Path::new();
        shadow_path.rounded_rect(
            pos.x,
            pos.y,
            size.x,
            size.y,
            APP_CONFIG.read().corner_roundness(),
        );

        let shadow_paint = Paint::color(femtovg::Color::rgba(
            self.style.color.r,
            self.style.color.g,
            self.style.color.b,
            (255.0 * HIGHLIGHT_OPACITY) as u8,
        ));

        canvas.fill_path(&shadow_path, &shadow_paint);
        Ok(())
    }
}

#[derive(Clone, Debug)]
enum HighlightKind {
    Block(Highlighter<BlockHighlight>),
    Freehand(Highlighter<FreehandHighlight>),
}

#[derive(Default, Clone, Debug)]
pub struct HighlightTool {
    highlighter: Option<HighlightKind>,
    style: Style,
    input_enabled: bool,
    sender: Option<Sender<SketchBoardInput>>,
}

impl Drawable for HighlightKind {
    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: femtovg::FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        match self {
            HighlightKind::Block(highlighter) => highlighter.highlight(canvas),
            HighlightKind::Freehand(highlighter) => highlighter.highlight(canvas),
        }
    }

    fn bounds(&self) -> Option<Rect> {
        match self {
            HighlightKind::Block(h) => h.data.size.map(|s| Rect::new(h.data.top_left, s)),
            HighlightKind::Freehand(h) => {
                if h.data.points.len() < 2 {
                    return None;
                }
                let first = *h.data.points.first()?;
                let mut min = first;
                let mut max = first;
                for p in h.data.points.iter().skip(1) {
                    let abs = first + *p;
                    min.x = min.x.min(abs.x);
                    min.y = min.y.min(abs.y);
                    max.x = max.x.max(abs.x);
                    max.y = max.y.max(abs.y);
                }
                let stroke = h.style.size.to_highlight_width(h.style.annotation_size_factor);
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
            HighlightKind::Block(_) => self
                .bounds()
                .map(|b| b.inflated(tolerance).contains(point))
                .unwrap_or(false),
            HighlightKind::Freehand(h) => {
                let Some(first) = h.data.points.first().copied() else {
                    return false;
                };
                if h.data.points.len() < 2 {
                    return false;
                }
                let stroke = h.style.size.to_highlight_width(h.style.annotation_size_factor);
                let pick = stroke / 2.0 + tolerance;
                let mut prev = first;
                for p in h.data.points.iter().skip(1) {
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
            HighlightKind::Block(h) => h.data.top_left += delta,
            HighlightKind::Freehand(h) => {
                if let Some(first) = h.data.points.first_mut() {
                    *first += delta;
                }
            }
        }
    }

    fn handles(&self) -> Vec<Handle> {
        match self {
            HighlightKind::Block(_) => self.bounds().map(bbox_handles).unwrap_or_default(),
            HighlightKind::Freehand(_) => Vec::new(),
        }
    }

    fn move_handle(&mut self, handle: HandleId, to: Vec2D) {
        let HighlightKind::Block(h) = self else { return };
        let Some(size) = h.data.size else { return };
        let cur = Rect::new(h.data.top_left, size);
        let new = bbox_resize(cur, handle, to);
        h.data.top_left = new.pos;
        h.data.size = Some(new.size);
    }

    fn set_style(&mut self, style: Style) {
        match self {
            HighlightKind::Block(h) => h.style = style,
            HighlightKind::Freehand(h) => h.style = style,
        }
    }

    fn render_glow(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: femtovg::FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> anyhow::Result<()> {
        // Block-highlight is always filled — use the offset-rect glow so the
        // halo sits entirely outside the highlight rectangle.
        if let HighlightKind::Block(_) = self
            && let Some(rect) = self.bounds()
        {
            let inflate = GLOW_STROKE_WIDTH / 2.0;
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
            paint.set_line_width(GLOW_STROKE_WIDTH);
            paint.set_line_join(femtovg::LineJoin::Round);
            canvas.stroke_path(&path, &paint);
            canvas.restore();
            return Ok(());
        }
        // Freehand falls back to the trait default (rounded-rect bbox glow).
        // Render that here since we're overriding the trait method.
        let Some(b) = self.bounds() else {
            return Ok(());
        };
        let pad = 6.0;
        canvas.save();
        let mut path = Path::new();
        path.rounded_rect(
            b.pos.x - pad,
            b.pos.y - pad,
            b.size.x + pad * 2.0,
            b.size.y + pad * 2.0,
            6.0,
        );
        let mut paint = Paint::color(GLOW_COLOR);
        paint.set_line_width(GLOW_STROKE_WIDTH);
        canvas.stroke_path(&path, &paint);
        canvas.restore();
        Ok(())
    }
}

impl Tool for HighlightTool {
    fn input_enabled(&self) -> bool {
        self.input_enabled
    }

    fn set_input_enabled(&mut self, value: bool) {
        self.input_enabled = value;
    }

    fn get_tool_type(&self) -> super::Tools {
        Tools::Highlight
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        let shift_pressed = event.modifier.intersects(ModifierType::SHIFT_MASK);
        let ctrl_pressed = event.modifier.intersects(ModifierType::CONTROL_MASK);
        let primary_highlighter = APP_CONFIG.read().primary_highlighter();
        match event.type_ {
            MouseEventType::BeginDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }
                // There exists two types of highlighting modes currently: freehand, block
                // A user may set a primary highlighter mode, with the other being accessible
                // by clicking CTRL when starting a highlight (doesn't need to be held).
                match (primary_highlighter, ctrl_pressed) {
                    // This matches when CTRL is not pressed and the primary highlighting mode
                    // is block, along with its inverse, CTRL pressed with the freehand mode
                    // being their primary highlighting mode.
                    (Highlighters::Block, false) | (Highlighters::Freehand, true) => {
                        self.highlighter =
                            Some(HighlightKind::Block(Highlighter::<BlockHighlight> {
                                data: BlockHighlight {
                                    top_left: event.pos,
                                    size: None,
                                },
                                style: self.style,
                            }))
                    }
                    // This matches the remaining two cases, which is when the user has the
                    // freehand mode as the primary mode and CTRL is not pressed, and conversely,
                    // when CTRL is pressed and the users primary mode is block.
                    (Highlighters::Freehand, false) | (Highlighters::Block, true) => {
                        self.highlighter =
                            Some(HighlightKind::Freehand(Highlighter::<FreehandHighlight> {
                                data: FreehandHighlight {
                                    points: vec![event.pos],
                                    shift_pressed,
                                },
                                style: self.style,
                            }))
                    }
                }

                ToolUpdateResult::Redraw
            }
            MouseEventType::UpdateDrag | MouseEventType::EndDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                if self.highlighter.is_none() {
                    return ToolUpdateResult::Unmodified;
                }
                let mut highlighter_kind = self.highlighter.as_mut().unwrap();
                let update: ToolUpdateResult = match &mut highlighter_kind {
                    HighlightKind::Block(highlighter) => {
                        // When shift is pressed when using the block highlighter, it transforms
                        // the area into a perfect square (in the direction they intended).
                        if shift_pressed {
                            let max_size = event.pos.x.abs().max(event.pos.y.abs());
                            highlighter.data.size = Some(Vec2D {
                                x: max_size * event.pos.x.signum(),
                                y: max_size * event.pos.y.signum(),
                            });
                        } else {
                            highlighter.data.size = Some(event.pos);
                        };
                        ToolUpdateResult::Redraw
                    }
                    HighlightKind::Freehand(highlighter) => {
                        if event.pos == Vec2D::zero() {
                            return ToolUpdateResult::Unmodified;
                        };

                        // The freehand highlighter has a more complex shift model:
                        // when pressing shift it begins a straight line, which is aligned
                        // from the point after shift was pressed, to any 15*n degree rotation.
                        //
                        // After releasing shift, it creates an extra point, this is useful since
                        // it means that users do not need to move their mouse to achieve perfectly
                        // aligned turns, since they can release, then hold shift again to continue
                        // another aligned line.
                        // This extra point can be removed by releasing shift again (if the cursor
                        // hasn't moved)
                        if shift_pressed {
                            // if shift was pressed before we remove an extra point which would
                            // have been the previous aligned point. However ignore if there is
                            // only one point which means the highlight has just started.
                            if highlighter.data.shift_pressed && highlighter.data.points.len() >= 2
                            {
                                highlighter
                                    .data
                                    .points
                                    .pop()
                                    .expect("at least 2 points in highlight path.");
                            };
                            // use the last point to position the snapping guide, or 0 if the point
                            // is the first one.
                            let last = if highlighter.data.points.len() == 1 {
                                Vec2D::zero()
                            } else {
                                *highlighter
                                    .data
                                    .points
                                    .last_mut()
                                    .expect("at least one point")
                            };
                            let snapped_pos = event.pos.sub(last).snapped_vector_15deg().add(last);
                            highlighter.data.points.push(snapped_pos);
                        } else {
                            highlighter.data.points.push(event.pos);
                        }

                        highlighter.data.shift_pressed = shift_pressed;
                        ToolUpdateResult::Redraw
                    }
                };
                if event.type_ == MouseEventType::UpdateDrag {
                    return update;
                };
                let result = highlighter_kind.clone_box();
                self.highlighter = None;
                ToolUpdateResult::Commit(result)
            }

            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn handle_key_event(&mut self, event: crate::sketch_board::KeyEventMsg) -> ToolUpdateResult {
        if event.key == Key::Escape && self.highlighter.is_some() {
            self.highlighter = None;
            return ToolUpdateResult::Redraw;
        }
        ToolUpdateResult::Unmodified
    }

    fn handle_key_release_event(
        &mut self,
        event: crate::sketch_board::KeyEventMsg,
    ) -> ToolUpdateResult {
        // Adds an extra point when shift is released in the freehand mode, this
        // allows for users to make sharper turns. Release shift a second time
        // to remove the added point (only if the cursor has not moved).
        if (event.key == Key::Shift_L || event.key == Key::Shift_R)
            && let Some(HighlightKind::Freehand(highlighter)) = &mut self.highlighter
        {
            let points = &mut highlighter.data.points;
            let last = points
                .last()
                .expect("line highlight must have at least one point");
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
        ToolUpdateResult::Unmodified
    }

    fn get_drawable(&self) -> Option<&dyn Drawable> {
        match &self.highlighter {
            Some(d) => Some(d),
            None => None,
        }
    }

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>) {
        self.sender = Some(sender);
    }
}
