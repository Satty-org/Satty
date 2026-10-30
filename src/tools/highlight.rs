use anyhow::Result;
use femtovg::{Paint, Path};

use relm4::{Sender, gtk::gdk::Key};

use crate::{
    math::{self, Vec2D},
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    style::Style,
    tools::{DrawableClone, SelectionType},
};

use super::{
    Drawable, Tool, ToolUpdateResult, Tools,
    drag_box::{DragBox, draw_center_marker},
};

const HIGHLIGHT_OPACITY: f64 = 0.4;

#[derive(Clone, Debug)]
struct BlockHighlight {
    origin: Vec2D,
    top_left: Vec2D,
    size: Vec2D,
    centered: bool,
    editing: bool,
    style: Style,
}

impl BlockHighlight {
    fn calculate_shape(&mut self, sender: &Sender<SketchBoardInput>, event: &MouseEventMsg) {
        let drag_box = DragBox::from_origin_delta(self.origin, self.size, event, sender);
        self.centered = drag_box.centered;
        self.top_left = drag_box.top_left;
        self.size = drag_box.size;
    }
}

#[derive(Default, Clone, Debug)]
pub struct HighlightTool {
    highlighter: Option<BlockHighlight>,
    style: Style,
    input_enabled: bool,
    sender: Option<Sender<SketchBoardInput>>,
}

impl Drawable for BlockHighlight {
    fn anchors(&self) -> Option<(Vec2D, Vec2D)> {
        Some(math::ensure_bounding_box(
            self.top_left,
            self.top_left + self.size,
        ))
    }

    fn get_selection_type(&self) -> SelectionType {
        SelectionType::Rectangle
    }

    fn hit_test(&self, pos: Vec2D, tolerance: f32) -> bool {
        let (tl, br) = math::ensure_bounding_box(self.top_left, self.top_left + self.size);
        super::hit_test_rectangle(pos, tl, br - tl, tolerance, true)
    }

    fn translate(&mut self, delta: Vec2D) {
        self.top_left += delta;
    }

    fn resize(&mut self, tl: Vec2D, br: Vec2D, _keep_aspect: bool) {
        let (tl, br) = math::ensure_bounding_box(tl, br);
        self.top_left = tl;
        self.size = br - tl;
    }

    fn get_style(&self) -> Option<&Style> {
        Some(&self.style)
    }

    fn get_style_mut(&mut self) -> Option<&mut Style> {
        Some(&mut self.style)
    }

    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: femtovg::FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        let (pos, size) = math::rect_ensure_positive_size(self.top_left, self.size);

        let mut shadow_path = Path::new();
        shadow_path.rounded_rect(pos.x, pos.y, size.x, size.y, self.style.corner_radius());

        let shadow_paint = Paint::color(femtovg::Color::rgba(
            self.style.color.r,
            self.style.color.g,
            self.style.color.b,
            (255.0 * HIGHLIGHT_OPACITY) as u8,
        ));

        canvas.fill_path(&shadow_path, &shadow_paint);

        if self.editing && self.centered {
            draw_center_marker(canvas, self.origin);
        }

        Ok(())
    }

    fn set_centered(&mut self, centered: bool) {
        self.centered = centered;
        self.origin = self.top_left + self.size / 2.0;
    }

    fn set_editing(&mut self, editing: bool) {
        self.editing = editing;
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
        Tools::BlockHighlighter
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        match event.type_ {
            MouseEventType::BeginDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                //set origin
                self.highlighter = Some(BlockHighlight {
                    origin: event.pos,
                    top_left: event.pos,
                    size: Vec2D::zero(),
                    centered: false,
                    editing: true,
                    style: self.style,
                });
                ToolUpdateResult::Redraw
            }
            MouseEventType::UpdateDrag | MouseEventType::EndDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                let Some(highlighter) = &mut self.highlighter else {
                    return ToolUpdateResult::Unmodified;
                };
                highlighter.calculate_shape(self.sender.as_ref().unwrap(), &event);

                if event.type_ == MouseEventType::UpdateDrag {
                    return ToolUpdateResult::Redraw;
                }

                highlighter.editing = false;
                let result = highlighter.clone_box();
                self.highlighter = None;
                ToolUpdateResult::Commit(result)
            }

            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn handle_key_event(&mut self, event: crate::sketch_board::KeyEventMsg) -> ToolUpdateResult {
        if matches!(event.key, Key::Alt_L | Key::Alt_R)
            && let Some(hb) = &mut self.highlighter
        {
            hb.centered = true;
            hb.top_left = hb.origin - hb.size;
            hb.size *= 2.0;

            return ToolUpdateResult::Redraw;
        }
        ToolUpdateResult::Unmodified
    }

    fn handle_key_release_event(
        &mut self,
        event: crate::sketch_board::KeyEventMsg,
    ) -> ToolUpdateResult {
        if matches!(event.key, Key::Alt_L | Key::Alt_R)
            && let Some(hb) = &mut self.highlighter
        {
            hb.centered = false;
            hb.top_left = hb.origin;
            hb.size /= 2.0;

            return ToolUpdateResult::Redraw;
        }
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
