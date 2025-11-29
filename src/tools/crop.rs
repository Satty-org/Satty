use std::f32::consts::PI;

use crate::{
    math::{self, Vec2D},
    sketch_board::{KeyEventMsg, MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
};
use anyhow::Result;
use femtovg::{Color, Paint, Path};
use relm4::{gtk::gdk::Key, Sender};

use super::{Drawable, Tool, ToolUpdateResult, Tools};

#[derive(Debug, Clone)]
pub struct Crop {
    pos: Vec2D,
    size: Vec2D,
    active: bool,
}

#[derive(Default)]
pub struct CropTool {
    crop: Option<Crop>,
    action: Option<CropToolAction>,
    input_enabled: bool,
    sender: Option<Sender<SketchBoardInput>>,
}

impl Crop {
    const HANDLE_RADIUS: f32 = 5.0;
    const HANDLE_BORDER: f32 = 2.0;

    fn new(pos: Vec2D) -> Self {
        Self {
            pos,
            size: Vec2D::zero(),
            active: true,
        }
    }

    fn draw_single_handle(
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        center: Vec2D,
        scale: f32,
    ) {
        let mut path = Path::new();
        path.arc(
            center.x,
            center.y,
            Crop::HANDLE_RADIUS / scale,
            0.0,
            2.0 * PI,
            femtovg::Solidity::Solid,
        );

        let border_paint =
            Paint::color(Color::rgbf(0.9, 0.9, 0.9)).with_line_width(Crop::HANDLE_BORDER / scale);
        let fill_paint = Paint::color(Color::rgbaf(0.0, 0.0, 0.0, 0.4));

        canvas.fill_path(&path, &fill_paint);
        canvas.stroke_path(&path, &border_paint);
    }

    pub fn get_rectangle(&self) -> (Vec2D, Vec2D) {
        math::rect_ensure_positive_size(self.pos, self.size)
    }

    fn get_handle_pos(crop_pos: Vec2D, crop_size: Vec2D, handle: CropHandle) -> Vec2D {
        match handle {
            CropHandle::TopLeftCorner => crop_pos,
            CropHandle::TopEdge => crop_pos + Vec2D::new(crop_size.x / 2.0, 0.0),
            CropHandle::TopRightCorner => crop_pos + Vec2D::new(crop_size.x, 0.0),
            CropHandle::RightEdge => crop_pos + Vec2D::new(crop_size.x, crop_size.y / 2.0),
            CropHandle::BottomRightCorner => crop_pos + Vec2D::new(crop_size.x, crop_size.y),
            CropHandle::BottomEdge => crop_pos + Vec2D::new(crop_size.x / 2.0, crop_size.y),
            CropHandle::BottomLeftCorner => crop_pos + Vec2D::new(0.0, crop_size.y),
            CropHandle::LeftEdge => crop_pos + Vec2D::new(0.0, crop_size.y / 2.0),
        }
    }
    fn get_closest_handle(&self, mouse_pos: Vec2D) -> (CropHandle, f32) {
        let mut min_distance_squared = f32::MAX;
        let mut closest_handle = CropHandle::TopLeftCorner;
        for h in CropHandle::all() {
            let handle_pos = Self::get_handle_pos(self.pos, self.size, h);
            let distance_squared = (handle_pos - mouse_pos).norm2();
            if distance_squared < min_distance_squared {
                min_distance_squared = distance_squared;
                closest_handle = h;
            }
        }
        (closest_handle, min_distance_squared)
    }
    fn test_handle_hit(&self, mouse_pos: Vec2D, margin2: f32) -> Option<CropHandle> {
        const HANDLE_SIZE: f32 = Crop::HANDLE_RADIUS + Crop::HANDLE_BORDER;
        const HANDLE_SIZE2: f32 = HANDLE_SIZE * HANDLE_SIZE;
        let allowed_distance2 = HANDLE_SIZE2 + margin2;

        let (handle, distance2) = self.get_closest_handle(mouse_pos);
        if distance2 < allowed_distance2 {
            Some(handle)
        } else {
            None
        }
    }
}

impl Drawable for Crop {
    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: femtovg::FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        let size = self.size;
        let scale = canvas.transform().average_scale();
        let dimensions = Vec2D::new(
            canvas.width() as f32 / scale,
            canvas.height() as f32 / scale,
        );

        let shadow_paint = Paint::color(Color::rgbaf(0.0, 0.0, 0.0, 0.5))
            .with_fill_rule(femtovg::FillRule::EvenOdd);
        let mut shadow_path = Path::new();
        shadow_path.rect(0.0, 0.0, dimensions.x, dimensions.y);
        shadow_path.rect(self.pos.x, self.pos.y, size.x, size.y);

        let border_paint = Paint::color(Color::rgbf(0.1, 0.1, 0.1)).with_line_width(2.0);
        let mut border_path = Path::new();
        border_path.rect(self.pos.x, self.pos.y, size.x, size.y);

        canvas.save();
        canvas.fill_path(&shadow_path, &shadow_paint);
        canvas.stroke_path(&border_path, &border_paint);

        if self.active {
            Self::draw_single_handle(canvas, self.pos, scale);
            Self::draw_single_handle(canvas, self.pos + Vec2D::new(size.x / 2.0, 0.0), scale);
            Self::draw_single_handle(canvas, self.pos + Vec2D::new(size.x, 0.0), scale);
            Self::draw_single_handle(canvas, self.pos + Vec2D::new(0.0, size.y / 2.0), scale);
            Self::draw_single_handle(canvas, self.pos + Vec2D::new(0.0, size.y), scale);
            Self::draw_single_handle(canvas, self.pos + Vec2D::new(size.x / 2.0, size.y), scale);
            Self::draw_single_handle(canvas, self.pos + Vec2D::new(size.x, size.y), scale);
            Self::draw_single_handle(canvas, self.pos + Vec2D::new(size.x, size.y / 2.0), scale);
        }

        canvas.restore();
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum CropHandle {
    TopLeftCorner,
    TopEdge,
    TopRightCorner,
    RightEdge,
    BottomRightCorner,
    BottomEdge,
    BottomLeftCorner,
    LeftEdge,
}

enum CropToolAction {
    NewCrop,
    DragHandle(DragHandleState),
    Move(MoveState),
}

struct DragHandleState {
    handle: CropHandle,
    top_left_start: Vec2D,
    bottom_right_start: Vec2D,
}

struct MoveState {
    start: Vec2D,
}

impl CropTool {
    pub fn get_crop(&self) -> Option<&Crop> {
        match &self.crop {
            Some(c) => Some(c),
            None => None,
        }
    }
}

impl CropHandle {
    fn all() -> [CropHandle; 8] {
        [
            CropHandle::TopLeftCorner,
            CropHandle::TopEdge,
            CropHandle::TopRightCorner,
            CropHandle::RightEdge,
            CropHandle::BottomRightCorner,
            CropHandle::BottomEdge,
            CropHandle::BottomLeftCorner,
            CropHandle::LeftEdge,
        ]
    }
}

impl CropTool {
    const HANDLE_MARGIN_IN_2: f32 = 15.0 * 15.0;
    const HANDLE_MARGIN_OUT: f32 = 40.0;

    fn test_inside_crop(&self, mouse_pos: Vec2D, margin: f32) -> bool {
        let crop = match &self.crop {
            Some(c) => c,
            None => return false,
        };

        let (mut min_x, mut max_x) = (crop.pos.x, crop.pos.x + crop.size.x);
        if min_x > max_x {
            (min_x, max_x) = (max_x, min_x);
        }
        min_x -= margin;
        max_x += margin;

        let (mut min_y, mut max_y) = (crop.pos.y, crop.pos.y + crop.size.y);
        if min_y > max_y {
            (min_y, max_y) = (max_y, min_y);
        }
        min_y -= margin;
        max_y += margin;

        min_x < mouse_pos.x && mouse_pos.x < max_x && min_y < mouse_pos.y && mouse_pos.y < max_y
    }

    fn apply_drag_handle_transformation(
        crop: &mut Crop,
        state: &DragHandleState,
        direction: Vec2D,
    ) {
        let mut tl = state.top_left_start;
        let mut br = state.bottom_right_start;

        // apply transformation
        match state.handle {
            CropHandle::TopLeftCorner => {
                tl += direction;
            }
            CropHandle::TopEdge => {
                tl += Vec2D::new(0.0, direction.y);
            }
            CropHandle::TopRightCorner => {
                tl += Vec2D::new(0.0, direction.y);
                br += Vec2D::new(direction.x, 0.0);
            }
            CropHandle::RightEdge => {
                br += Vec2D::new(direction.x, 0.0);
            }
            CropHandle::BottomRightCorner => {
                br += direction;
            }
            CropHandle::BottomEdge => {
                br += Vec2D::new(0.0, direction.y);
            }
            CropHandle::BottomLeftCorner => {
                tl += Vec2D::new(direction.x, 0.0);
                br += Vec2D::new(0.0, direction.y);
            }
            CropHandle::LeftEdge => {
                tl += Vec2D::new(direction.x, 0.0);
            }
        }

        // convert back and save
        crop.pos = tl;
        crop.size = br - tl;
    }

    fn begin_drag(&mut self, pos: Vec2D) -> ToolUpdateResult {
        match &self.crop {
            None => {
                // No crop exists, create a new one
                self.crop = Some(Crop::new(pos));
                self.action = Some(CropToolAction::NewCrop);
            }
            Some(c) => {
                if let Some(handle) = c.test_handle_hit(pos, CropTool::HANDLE_MARGIN_IN_2) {
                    // Crop exists and we are near a handle, drag it
                    self.action = Some(CropToolAction::DragHandle(DragHandleState {
                        handle,
                        top_left_start: c.pos,
                        bottom_right_start: c.pos + c.size,
                    }));
                } else if self.test_inside_crop(pos, 0.0) {
                    // Crop exists and we are inside it, move it
                    self.action = Some(CropToolAction::Move(MoveState { start: c.pos }));
                } else if self.test_inside_crop(pos, CropTool::HANDLE_MARGIN_OUT) {
                    // Crop exists and we are near the edge, drag from the closest handle
                    let (handle, _) = c.get_closest_handle(pos);
                    self.action = Some(CropToolAction::DragHandle(DragHandleState {
                        handle,
                        top_left_start: c.pos,
                        bottom_right_start: c.pos + c.size,
                    }));
                } else {
                    // Crop exists, but we far outside from it, create a new one
                    self.crop = Some(Crop::new(pos));
                    self.action = Some(CropToolAction::NewCrop);
                }
            }
        }
        ToolUpdateResult::Redraw
    }

    fn update_drag(&mut self, direction: Vec2D) -> ToolUpdateResult {
        let crop = match &mut self.crop {
            Some(c) => c,
            None => return ToolUpdateResult::Unmodified,
        };

        let action = match &self.action {
            Some(a) => a,
            None => return ToolUpdateResult::Unmodified,
        };

        match action {
            CropToolAction::NewCrop => {
                crop.size = direction;
                ToolUpdateResult::Redraw
            }
            CropToolAction::DragHandle(state) => {
                Self::apply_drag_handle_transformation(crop, state, direction);
                ToolUpdateResult::Redraw
            }
            CropToolAction::Move(state) => {
                crop.pos = state.start + direction;
                ToolUpdateResult::Redraw
            }
        }
    }

    fn end_drag(&mut self, direction: Vec2D) -> ToolUpdateResult {
        let Some(crop) = &mut self.crop else {
            return ToolUpdateResult::Unmodified;
        };

        let Some(action) = &self.action else {
            return ToolUpdateResult::Unmodified;
        };

        match action {
            // crop never returns "commit" because nothing gets
            // committed to the drawables stack
            CropToolAction::NewCrop => {
                crop.size = direction;
                self.action = None;
                ToolUpdateResult::Redraw
            }
            CropToolAction::DragHandle(state) => {
                Self::apply_drag_handle_transformation(crop, state, direction);
                self.action = None;
                ToolUpdateResult::Redraw
            }
            CropToolAction::Move(state) => {
                crop.pos = state.start + direction;
                self.action = None;
                ToolUpdateResult::Redraw
            }
        }
    }
}

impl Tool for CropTool {
    fn input_enabled(&self) -> bool {
        self.input_enabled
    }

    fn set_input_enabled(&mut self, value: bool) {
        self.input_enabled = value;
    }

    fn get_tool_type(&self) -> super::Tools {
        Tools::Crop
    }

    fn handle_key_event(&mut self, event: KeyEventMsg) -> ToolUpdateResult {
        if event.key == Key::Escape {
            if let Some(crop) = &self.crop {
                if crop.active {
                    // Crop is active, deactivate it
                    return self.handle_deactivated();
                }
            }
        }
        // No crop exists or crop is inactive - let event bubble to global handler
        ToolUpdateResult::Unmodified
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        match event.type_ {
            MouseEventType::BeginDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }
                self.begin_drag(event.pos)
            }
            MouseEventType::EndDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }
                self.end_drag(event.pos)
            }
            MouseEventType::UpdateDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }
                self.update_drag(event.pos)
            }
            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn handle_activated(&mut self) -> ToolUpdateResult {
        if let Some(c) = &mut self.crop {
            c.active = true;
            return ToolUpdateResult::Redraw;
        }
        ToolUpdateResult::Unmodified
    }

    fn handle_deactivated(&mut self) -> ToolUpdateResult {
        if let Some(c) = &mut self.crop {
            c.active = false;
        }
        self.action = None;
        ToolUpdateResult::Redraw
    }

    fn get_drawable(&self) -> Option<&dyn Drawable> {
        // the reason we always return None is because we dont want this tool
        // to show up with the standard rendering mechanism. Instead it will always
        // be drawn separately by using `get_crop(&self)`
        None
    }

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>) {
        self.sender = Some(sender);
    }
}
