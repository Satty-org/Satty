use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use femtovg::{Color, ImageId, Paint, Path, RenderTarget};

use relm4::{Sender, gtk::gdk::Key};

use crate::{
    configuration::APP_CONFIG,
    math::{self, Vec2D},
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    style::Style,
    tools::{DrawableClone, RenderingMode, SelectionType, SpotlightData},
};

use super::{
    Drawable, Tool, ToolUpdateResult, Tools,
    drag_box::{DragBox, draw_center_marker},
};

// Freehand is implemented by the BrushTool, HighlightTool handles the others

/// Lower limit of the spotlight opacity in percent, so the spotlight stays visible.
const SPOTLIGHT_MIN_ALPHA: f32 = 25.0;
/// Lower limit of the block highlighter opacity in percent.
const BLOCK_HIGHLIGHTER_MIN_ALPHA: f32 = 20.0;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Highlighters {
    #[default]
    Block,
    Spotlight,
    /// covers the area with the style color, black by default
    Blackout,
}

#[derive(Clone, Debug)]
struct BlockHighlight {
    mode: Highlighters,
    shared_spotlight_alpha: Option<Arc<Mutex<f32>>>,
    origin: Vec2D,
    top_left: Vec2D,
    size: Vec2D,
    centered: bool,
    editing: bool,
    cached_layer: Rc<RefCell<Option<(u32, u32, ImageId)>>>,
    style: Style,
}

impl BlockHighlight {
    // For an ellipse, the dragged box is the rectangle whose corners lie on the
    // ellipse, like for the Ellipse tool. Otherwise it is the box itself.
    fn dragged_box(&self) -> (Vec2D, Vec2D) {
        if self.style.fill {
            return (self.top_left, self.size);
        }
        let (tl, br) = math::rectangle_in_ellipse(self.top_left, self.top_left + self.size);
        (tl, br - tl)
    }

    fn set_dragged_box(&mut self, top_left: Vec2D, size: Vec2D) {
        let (tl, br) = if self.style.fill {
            (top_left, top_left + size)
        } else {
            math::ellipse_around_rectangle(top_left, top_left + size)
        };
        self.top_left = tl;
        self.size = br - tl;
    }

    fn calculate_shape(&mut self, sender: &Sender<SketchBoardInput>, event: &MouseEventMsg) {
        let (_, dragged_size) = self.dragged_box();
        let drag_box = DragBox::from_delta(self.origin, dragged_size, event.pos, event.modifier);
        self.centered = drag_box.centered;
        self.set_dragged_box(drag_box.top_left, drag_box.size);
        sender
            .send(SketchBoardInput::ShapeDimensionsUpdate(self.size))
            .ok();
    }
}

#[derive(Default, Clone, Debug)]
pub struct HighlightTool {
    mode: Highlighters,
    highlighter: Option<BlockHighlight>,
    style: Style,
    input_enabled: bool,
    sender: Option<Sender<SketchBoardInput>>,
    shared_spotlight_alpha: Option<Arc<Mutex<f32>>>,
}

impl Drawable for BlockHighlight {
    fn get_rendering_mode(&self) -> super::RenderingMode {
        if self.mode == Highlighters::Spotlight {
            RenderingMode::SpotlightHighlight
        } else {
            RenderingMode::Default
        }
    }

    fn anchors(&self) -> Option<(Vec2D, Vec2D)> {
        Some(math::ensure_bounding_box(
            self.top_left,
            self.top_left + self.size,
        ))
    }

    fn get_selection_type(&self) -> SelectionType {
        match self.style.fill {
            false => SelectionType::Ellipse,
            true => SelectionType::Rectangle,
        }
    }

    fn hit_test(&self, pos: Vec2D, tolerance: f32) -> bool {
        // a blackout covers its whole area, so it is hit anywhere inside
        let filled = self.mode == Highlighters::Blackout;
        if matches!(self.mode, Highlighters::Spotlight | Highlighters::Blackout) && !self.style.fill
        {
            return super::hit_test_ellipse(pos, self.top_left, self.size, tolerance, filled);
        }

        super::hit_test_rectangle(pos, self.top_left, self.size, tolerance, filled)
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

    fn set_style(&mut self, style: Style) {
        self.style = style;
        if self.mode == Highlighters::Spotlight
            && let Some(alpha) = &self.shared_spotlight_alpha
        {
            *alpha.lock().unwrap() = self.style.size_as_alpha(SPOTLIGHT_MIN_ALPHA);
        }
    }

    fn draw_spotlight(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        bounds: (Vec2D, Vec2D),
        boxes: &Vec<SpotlightData>,
        spotlight_preview: bool,
        _background_image_id: femtovg::ImageId,
        output_target: RenderTarget,
    ) {
        assert!(
            self.mode == Highlighters::Spotlight,
            "draw_spotlight called on non-spotlight highlight"
        );
        let alpha: f32 = *self
            .shared_spotlight_alpha
            .as_ref()
            .expect("Shared spotlight alpha should be set for spotlight highlights")
            .lock()
            .unwrap();

        let alpha = if spotlight_preview { 0.4 } else { alpha };
        let canvas_tl = bounds.0;
        let canvas_size = bounds.1 - bounds.0;
        super::spotlight::draw_spotlight_layer(
            canvas,
            boxes,
            self.style.effective_corner_radius(),
            output_target,
            &self.cached_layer,
            |canvas| {
                let mut dim = Path::new();
                dim.rect(canvas_tl.x, canvas_tl.y, canvas_size.x, canvas_size.y);
                let mut color = Color::black();
                color.set_alphaf(alpha);
                canvas.fill_path(&dim, &Paint::color(color));
            },
        );
    }

    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: femtovg::FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        let (tl, size) = math::rect_ensure_positive_size(self.top_left, self.size);

        let mut shadow_path = Path::new();
        if self.style.fill {
            shadow_path.rounded_rect(
                tl.x,
                tl.y,
                size.x,
                size.y,
                self.style.effective_corner_radius(),
            );
        } else {
            let (center, radii) = (tl + size / 2.0, size / 2.0);
            shadow_path.ellipse(center.x, center.y, radii.x, radii.y);
        }

        let shadow_paint = Paint::color(match self.mode {
            Highlighters::Blackout => self.style.paint_color(),
            _ => {
                let mut color = self.style.paint_color();
                color.a *= self
                    .style
                    .size_as_alpha(if self.mode == Highlighters::Spotlight {
                        SPOTLIGHT_MIN_ALPHA
                    } else {
                        BLOCK_HIGHLIGHTER_MIN_ALPHA
                    });
                color
            }
        });

        canvas.fill_path(&shadow_path, &shadow_paint);

        if self.editing && self.centered {
            draw_center_marker(canvas, self.origin);
        }

        Ok(())
    }

    fn get_centered(&self) -> bool {
        self.centered
    }

    fn set_centered(&mut self, centered: bool) {
        self.centered = centered;
        self.origin = self.top_left + self.size / 2.0;
    }

    fn set_editing(&mut self, editing: bool) {
        self.editing = editing;
    }
}

impl HighlightTool {
    pub fn with_mode(mode: Highlighters) -> Self {
        Self {
            mode,
            shared_spotlight_alpha: (mode == Highlighters::Spotlight).then(|| {
                Arc::new(Mutex::new(
                    APP_CONFIG
                        .read()
                        .style_for(Tools::HighlightSpotlight)
                        .size_as_alpha(SPOTLIGHT_MIN_ALPHA),
                ))
            }),
            ..Default::default()
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
        match self.mode {
            Highlighters::Block => Tools::BlockHighlighter,
            Highlighters::Spotlight => Tools::HighlightSpotlight,
            Highlighters::Blackout => Tools::Blackout,
        }
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        match event.type_ {
            MouseEventType::BeginDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                //set origin
                self.highlighter = Some(BlockHighlight {
                    mode: self.mode,
                    shared_spotlight_alpha: self.shared_spotlight_alpha.clone(),
                    origin: event.pos,
                    top_left: event.pos,
                    size: Vec2D::zero(),
                    centered: false,
                    editing: true,
                    cached_layer: Rc::new(RefCell::new(None)),
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
            let (_, size) = hb.dragged_box();
            hb.set_dragged_box(hb.origin - size, size * 2.0);

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
            let (_, size) = hb.dragged_box();
            hb.set_dragged_box(hb.origin, size / 2.0);

            return ToolUpdateResult::Redraw;
        }
        ToolUpdateResult::Unmodified
    }

    fn handle_style_event(&mut self, style: Style) -> ToolUpdateResult {
        self.style = style;
        if let Some(ref shared_spotlight_alpha) = self.shared_spotlight_alpha {
            *shared_spotlight_alpha.lock().unwrap() = style.size_as_alpha(SPOTLIGHT_MIN_ALPHA);
        }
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
