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
    math::{self, Vec2D},
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    style::Style,
    tools::DrawableClone,
};

use satty_cli::command_line;

use super::{
    Drawable, Tool, ToolUpdateResult, Tools,
    drag_box::{DragBox, draw_center_marker},
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
    origin: Vec2D,
    top_left: Vec2D,
    size: Vec2D,
    centered: bool,
    editing: bool,
}

#[derive(Clone, Debug)]
struct FreehandHighlight {
    start_point: Vec2D,
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
        let start_point = self.data.start_point;

        path.move_to(start_point.x, start_point.y);
        for p in self.data.points.iter() {
            path.line_to(start_point.x + p.x, start_point.y + p.y);
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
        let (pos, size) = math::rect_ensure_positive_size(self.data.top_left, self.data.size);

        let mut shadow_path = Path::new();
        shadow_path.rounded_rect(pos.x, pos.y, size.x, size.y, self.style.corner_radius());

        let shadow_paint = Paint::color(femtovg::Color::rgba(
            self.style.color.r,
            self.style.color.g,
            self.style.color.b,
            (255.0 * HIGHLIGHT_OPACITY) as u8,
        ));

        canvas.fill_path(&shadow_path, &shadow_paint);

        if self.data.editing && self.data.centered {
            draw_center_marker(canvas, self.data.origin);
        }

        Ok(())
    }
}

impl BlockHighlight {
    fn calculate_shape(&mut self, sender: &Sender<SketchBoardInput>, event: &MouseEventMsg) {
        let drag_box = DragBox::from_origin_delta(self.origin, self.size, event, sender);
        self.centered = drag_box.centered;
        self.top_left = drag_box.top_left;
        self.size = drag_box.size;
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
    fn bounds(&self) -> Option<(Vec2D, Vec2D)> {
        match self {
            HighlightKind::Block(h) => Some(math::ensure_bounding_box(
                h.data.top_left,
                h.data.top_left + h.data.size,
            )),
            HighlightKind::Freehand(h) => Some(super::calculate_line_bounds(
                h.data.start_point,
                &h.data.points,
            )),
        }
    }

    fn hit_test(&self, pos: Vec2D, tolerance: f32) -> bool {
        match self {
            HighlightKind::Block(_) => {
                let (tl, br) = match self.bounds() {
                    Some(bounds) => bounds,
                    None => return false,
                };
                super::hit_test_rectangle(pos, tl, br - tl, tolerance, true)
            }
            HighlightKind::Freehand(h) => {
                let line_width = h
                    .style
                    .size
                    .to_highlight_width(h.style.annotation_size_factor);
                let tolerance = tolerance + line_width / 2.0;
                super::hit_test_line(pos, h.data.start_point, &h.data.points, tolerance)
            }
        }
    }

    fn translate(&mut self, delta: Vec2D) {
        match self {
            HighlightKind::Block(h) => {
                h.data.top_left += delta;
            }
            HighlightKind::Freehand(h) => {
                h.data.start_point += delta;
            }
        }
    }

    fn resize_bounds(&mut self, tl: Vec2D, br: Vec2D, _delta: Vec2D, _keep_aspect: bool) {
        match self {
            HighlightKind::Block(h) => {
                let (tl, br) = math::ensure_bounding_box(tl, br);
                h.data.top_left = tl;
                h.data.size = br - tl;
            }
            HighlightKind::Freehand(h) => {
                super::resize_line_bounds(tl, br, &mut h.data.start_point, &mut h.data.points);
            }
        }
    }

    fn get_style(&self) -> Option<&Style> {
        match self {
            HighlightKind::Block(highlighter) => Some(&highlighter.style),
            HighlightKind::Freehand(highlighter) => Some(&highlighter.style),
        }
    }

    fn get_style_mut(&mut self) -> Option<&mut Style> {
        match self {
            HighlightKind::Block(highlighter) => Some(&mut highlighter.style),
            HighlightKind::Freehand(highlighter) => Some(&mut highlighter.style),
        }
    }

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

    fn set_centered(&mut self, centered: bool) {
        if let HighlightKind::Block(highlighter) = self {
            highlighter.data.centered = centered;
            highlighter.data.origin = highlighter.data.top_left + highlighter.data.size / 2.0;
        }
    }
    fn set_editing(&mut self, editing: bool) {
        if let HighlightKind::Block(highlighter) = self {
            highlighter.data.editing = editing;
        }
    }
}

impl Tool for HighlightTool {
    fn input_enabled(&self) -> bool {
        self.input_enabled
    }

    fn set_input_enabled(&mut self, value: bool) {
        self.input_enabled = value;
    }

    fn active(&self) -> bool {
        self.highlighter.is_some()
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
                                    origin: event.pos,
                                    top_left: event.pos,
                                    size: Vec2D::zero(),
                                    centered: false,
                                    editing: true,
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
                                    start_point: event.pos,
                                    points: vec![],
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
                        highlighter
                            .data
                            .calculate_shape(self.sender.as_ref().unwrap(), &event);
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
                }

                if let HighlightKind::Block(highlighter) = &mut *highlighter_kind {
                    highlighter.data.editing = false;
                }

                let result = highlighter_kind.clone_box();
                self.highlighter = None;
                ToolUpdateResult::Commit(result)
            }

            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn handle_key_event(&mut self, event: crate::sketch_board::KeyEventMsg) -> ToolUpdateResult {
        if matches!(event.key, Key::Alt_L | Key::Alt_R)
            && let Some(HighlightKind::Block(hb)) = &mut self.highlighter
        {
            hb.data.centered = true;
            hb.data.top_left = hb.data.origin - hb.data.size;
            hb.data.size *= 2.0;

            return ToolUpdateResult::Redraw;
        }
        ToolUpdateResult::Unmodified
    }

    fn handle_key_release_event(
        &mut self,
        event: crate::sketch_board::KeyEventMsg,
    ) -> ToolUpdateResult {
        if matches!(event.key, Key::Alt_L | Key::Alt_R)
            && let Some(HighlightKind::Block(hb)) = &mut self.highlighter
        {
            hb.data.centered = false;
            hb.data.top_left = hb.data.origin;
            hb.data.size /= 2.0;

            return ToolUpdateResult::Redraw;
        }

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
