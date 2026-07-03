use std::cell::Cell;
use std::f32::consts::FRAC_PI_2;

use anyhow::Result;
use femtovg::{Color, ImageId, Paint, Path};
use relm4::gtk::gdk::{Key, ModifierType};
use relm4::gtk::gdk_pixbuf::Pixbuf;
use relm4::gtk::prelude::*;
use relm4::{RelmWidgetExt, Sender, gtk};

use crate::{
    configuration::APP_CONFIG,
    femtovg_area::create_image_from_pixbuf,
    image_loading,
    math::{Angle, Vec2D},
    notification::log_result,
    sketch_board::{KeyEventMsg, MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
};

use super::{
    Drawable, InputContext, Tool, ToolUpdateResult, Tools,
    drag_box::{HANDLE_BORDER, HANDLE_RADIUS, draw_handle},
};

#[derive(Clone, Copy, Debug, PartialEq)]
enum ImageHandle {
    Rotate,
    // a corner or edge midpoint, encoded by its sign vector; a zero
    // component means the resize leaves that axis unchanged
    Resize(Vec2D),
    Inside,
}

#[derive(Clone, Debug)]
pub struct Image {
    pixbuf: Pixbuf,
    center: Vec2D,
    // signed size: a negative component means the image is mirrored on that axis
    size: Vec2D,
    rotation: Angle,
    editing: bool,
    hover: Option<ImageHandle>,
    cached_image_id: Cell<Option<ImageId>>,
}

impl Image {
    const ROTATE_HANDLE_OFFSET: f32 = 30.0;
    // maximum fraction of the background image an inserted image covers initially
    const INITIAL_SIZE_FRACTION: f32 = 0.5;

    fn new(pixbuf: Pixbuf, background_size: Vec2D) -> Self {
        let natural_size = Vec2D::new(pixbuf.width() as f32, pixbuf.height() as f32);
        let scale = (background_size.x * Self::INITIAL_SIZE_FRACTION / natural_size.x)
            .min(background_size.y * Self::INITIAL_SIZE_FRACTION / natural_size.y)
            .min(1.0);

        Self {
            pixbuf,
            center: background_size * 0.5,
            size: natural_size * scale,
            rotation: Angle::default(),
            editing: true,
            hover: None,
            cached_image_id: Cell::new(None),
        }
    }

    /// Transforms an image coordinate into the rotation-only frame around the
    /// center. In this frame the corners are at `corner_offset(±1, ±1)`,
    /// regardless of mirroring, because the size components are signed.
    fn to_local(&self, pos: Vec2D) -> Vec2D {
        (pos - self.center).rotate(self.rotation * -1.0)
    }

    fn to_image_coords(&self, local: Vec2D) -> Vec2D {
        self.center + local.rotate(self.rotation)
    }

    fn handle_offset(&self, handle: Vec2D) -> Vec2D {
        Vec2D::new(handle.x * self.size.x / 2.0, handle.y * self.size.y / 2.0)
    }

    fn rotate_handle_offset(&self) -> Vec2D {
        Vec2D::new(0.0, -self.size.y.abs() / 2.0 - Self::ROTATE_HANDLE_OFFSET)
    }

    fn resize_handles() -> [Vec2D; 8] {
        [
            Vec2D::new(-1.0, -1.0),
            Vec2D::new(0.0, -1.0),
            Vec2D::new(1.0, -1.0),
            Vec2D::new(1.0, 0.0),
            Vec2D::new(1.0, 1.0),
            Vec2D::new(0.0, 1.0),
            Vec2D::new(-1.0, 1.0),
            Vec2D::new(-1.0, 0.0),
        ]
    }

    fn hit_test(&self, pos: Vec2D) -> Option<ImageHandle> {
        const HANDLE_SIZE: f32 = HANDLE_RADIUS + HANDLE_BORDER;
        const HANDLE_MARGIN_2: f32 = 15.0 * 15.0;
        let allowed_distance2 = HANDLE_SIZE * HANDLE_SIZE + HANDLE_MARGIN_2;

        let local = self.to_local(pos);
        if (self.rotate_handle_offset() - local).norm2() < HANDLE_MARGIN_2 {
            return Some(ImageHandle::Rotate);
        }

        let closest_handle = Self::resize_handles()
            .into_iter()
            .map(|handle| (handle, (self.handle_offset(handle) - local).norm2()))
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
            .filter(|(_, distance2)| *distance2 < allowed_distance2)
            .map(|(handle, _)| handle);
        if let Some(handle) = closest_handle {
            return Some(ImageHandle::Resize(handle));
        }

        if local.x.abs() <= self.size.x.abs() / 2.0 && local.y.abs() <= self.size.y.abs() / 2.0 {
            return Some(ImageHandle::Inside);
        }
        None
    }

    fn draw_decorations(&self, canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>) {
        let scale = canvas.transform().average_scale();
        let (width, height) = (self.size.x.abs(), self.size.y.abs());

        let border_color = if self.hover == Some(ImageHandle::Inside) {
            Color::rgbf(0.9, 0.9, 0.9)
        } else {
            Color::rgbf(0.1, 0.1, 0.1)
        };
        let border_paint = Paint::color(border_color).with_line_width(2.0);
        let mut border_path = Path::new();
        border_path.rect(-width / 2.0, -height / 2.0, width, height);

        let rotate_handle = self.rotate_handle_offset();
        let rotate_paint = Paint::color(Color::rgbf(0.1, 0.1, 0.1)).with_line_width(2.0);
        let mut rotate_path = Path::new();
        rotate_path.move_to(0.0, -height / 2.0);
        rotate_path.line_to(rotate_handle.x, rotate_handle.y);

        canvas.stroke_path(&border_path, &border_paint);
        canvas.stroke_path(&rotate_path, &rotate_paint);

        for handle in Self::resize_handles() {
            draw_handle(
                canvas,
                self.handle_offset(handle),
                scale,
                self.hover == Some(ImageHandle::Resize(handle)),
            );
        }
        draw_handle(
            canvas,
            rotate_handle,
            scale,
            self.hover == Some(ImageHandle::Rotate),
        );
    }
}

impl Drawable for Image {
    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: femtovg::FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        let image_id = match self.cached_image_id.get() {
            Some(id) => id,
            None => {
                let id = create_image_from_pixbuf(canvas, &self.pixbuf)?;
                self.cached_image_id.set(Some(id));
                id
            }
        };

        let (width, height) = (self.size.x.abs(), self.size.y.abs());

        canvas.save();
        canvas.translate(self.center.x, self.center.y);
        canvas.rotate(self.rotation.radians);

        canvas.save();
        canvas.scale(self.size.x.signum(), self.size.y.signum());
        let mut path = Path::new();
        path.rect(-width / 2.0, -height / 2.0, width, height);
        canvas.fill_path(
            &path,
            &Paint::image(
                image_id,
                -width / 2.0,
                -height / 2.0,
                width,
                height,
                0f32,
                1f32,
            ),
        );
        // undo the mirroring so the decorations are drawn upright
        canvas.restore();

        if self.editing {
            self.draw_decorations(canvas);
        }

        canvas.restore();
        Ok(())
    }
}

enum ImageToolAction {
    Move {
        start_center: Vec2D,
    },
    Resize {
        handle: Vec2D,
        anchor: Vec2D,
        start_size: Vec2D,
    },
    Rotate,
}

#[derive(Default)]
pub struct ImageTool {
    image: Option<Image>,
    action: Option<ImageToolAction>,
    drag_start: Vec2D,
    input_enabled: bool,
    input_context: Option<InputContext>,
    sender: Option<Sender<SketchBoardInput>>,
    // gtk does not keep the native dialog alive, dropping it closes the
    // dialog and crashes gtk internals, so hold on to it until the response
    dialog: Option<gtk::FileChooserNative>,
}

impl ImageTool {
    fn open_file_dialog(&mut self) {
        let Some(sender) = self.sender.clone() else {
            return;
        };
        let window = self
            .input_context
            .as_ref()
            .and_then(|context| context.widget.toplevel_window());

        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Images"));
        filter.add_pixbuf_formats();
        for mime_type in image_loading::FALLBACK_MIME_TYPES {
            filter.add_mime_type(mime_type);
        }

        let builder = gtk::FileChooserNative::builder()
            .modal(true)
            .title("Add Image")
            .action(gtk::FileChooserAction::Open)
            .accept_label("Open")
            .cancel_label("Cancel");

        let dialog = match window {
            Some(w) => builder.transient_for(&w),
            None => builder,
        }
        .build();
        dialog.add_filter(&filter);

        dialog.connect_response(move |dialog, response| {
            if response == gtk::ResponseType::Accept
                && let Some(path) = dialog.file().and_then(|file| file.path())
            {
                match image_loading::pixbuf_from_file(&path) {
                    Ok(pixbuf) => sender.emit(SketchBoardInput::ImageSelected(pixbuf)),
                    Err(e) => log_result(
                        &format!("Error loading image: {e}"),
                        !APP_CONFIG.read().disable_notifications(),
                    ),
                }
            }
            dialog.destroy();
        });

        dialog.show();
        self.dialog = Some(dialog);
    }

    fn commit_image(&mut self) -> ToolUpdateResult {
        self.action = None;
        match self.image.take() {
            Some(mut image) => {
                image.editing = false;
                ToolUpdateResult::Commit(Box::new(image))
            }
            None => ToolUpdateResult::Unmodified,
        }
    }

    fn begin_drag(&mut self, pos: Vec2D) -> ToolUpdateResult {
        let Some(image) = &self.image else {
            return ToolUpdateResult::Unmodified;
        };

        self.drag_start = pos;
        self.action = match image.hit_test(pos) {
            Some(ImageHandle::Rotate) => Some(ImageToolAction::Rotate),
            Some(ImageHandle::Resize(handle)) => Some(ImageToolAction::Resize {
                handle,
                anchor: image.to_image_coords(image.handle_offset(handle * -1.0)),
                start_size: image.size,
            }),
            Some(ImageHandle::Inside) => Some(ImageToolAction::Move {
                start_center: image.center,
            }),
            None => None,
        };

        match self.action {
            Some(_) => ToolUpdateResult::Redraw,
            None => ToolUpdateResult::Unmodified,
        }
    }

    fn update_hover(&mut self, pos: Vec2D) -> ToolUpdateResult {
        let Some(image) = &mut self.image else {
            return ToolUpdateResult::Unmodified;
        };
        // keep the active handle highlighted while dragging
        if self.action.is_some() {
            return ToolUpdateResult::Unmodified;
        }

        let hover = image.hit_test(pos);
        if hover != image.hover {
            image.hover = hover;
            ToolUpdateResult::Redraw
        } else {
            ToolUpdateResult::Unmodified
        }
    }

    fn update_drag(&mut self, delta: Vec2D, modifier: ModifierType) -> ToolUpdateResult {
        let (Some(image), Some(action)) = (&mut self.image, &self.action) else {
            return ToolUpdateResult::Unmodified;
        };

        let mouse = self.drag_start + delta;
        match action {
            ImageToolAction::Move { start_center } => {
                image.center = *start_center + delta;
            }
            ImageToolAction::Rotate => {
                let direction = mouse - image.center;
                if direction.is_zero() {
                    return ToolUpdateResult::Unmodified;
                }
                // the rotate handle points upwards, hence the quarter turn
                let mut radians = direction.angle().radians + FRAC_PI_2;
                if modifier.contains(ModifierType::SHIFT_MASK) {
                    let step = Angle::from_degrees(15.0).radians;
                    radians = (radians / step).round() * step;
                }
                image.rotation = Angle::from_radians(radians);
            }
            ImageToolAction::Resize {
                handle,
                anchor,
                start_size,
            } => {
                // vector from the fixed anchor handle to the mouse, in the
                // rotation-only frame; crossing the anchor mirrors the image
                let mut span = (mouse - *anchor).rotate(image.rotation * -1.0);
                // corner handles keep the aspect ratio, edge handles stretch
                // the image along a single axis
                if handle.x != 0.0 && handle.y != 0.0 {
                    let reference =
                        Vec2D::new(start_size.x.abs().max(1.0), start_size.y.abs().max(1.0));
                    let uniform_scale =
                        (span.x.abs() / reference.x).max(span.y.abs() / reference.y);
                    span = Vec2D::new(
                        span.x.signum() * reference.x * uniform_scale,
                        span.y.signum() * reference.y * uniform_scale,
                    );
                }

                // a zero handle component leaves that axis unchanged
                let mut size = *start_size;
                let mut center_offset = Vec2D::zero();
                if handle.x != 0.0 {
                    size.x = span.x * handle.x;
                    center_offset.x = span.x / 2.0;
                }
                if handle.y != 0.0 {
                    size.y = span.y * handle.y;
                    center_offset.y = span.y / 2.0;
                }
                image.size = size;
                image.center = *anchor + center_offset.rotate(image.rotation);
            }
        }
        ToolUpdateResult::Redraw
    }
}

impl Tool for ImageTool {
    fn get_tool_type(&self) -> Tools {
        Tools::Image
    }

    fn input_enabled(&self) -> bool {
        self.input_enabled
    }

    fn set_input_enabled(&mut self, value: bool) {
        self.input_enabled = value;
    }

    fn set_im_context(&mut self, context: Option<InputContext>) {
        self.input_context = context;
    }

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>) {
        self.sender = Some(sender);
    }

    fn active(&self) -> bool {
        self.image.is_some()
    }

    fn get_drawable(&self) -> Option<&dyn Drawable> {
        match &self.image {
            Some(d) => Some(d),
            None => None,
        }
    }

    fn handle_activated(&mut self) -> ToolUpdateResult {
        if self.image.is_none() {
            self.open_file_dialog();
        }
        ToolUpdateResult::Unmodified
    }

    fn handle_deactivated(&mut self) -> ToolUpdateResult {
        self.commit_image()
    }

    fn handle_image_selected(
        &mut self,
        pixbuf: Pixbuf,
        background_size: Vec2D,
    ) -> ToolUpdateResult {
        match self.image.replace(Image::new(pixbuf, background_size)) {
            Some(mut previous) => {
                previous.editing = false;
                ToolUpdateResult::Commit(Box::new(previous))
            }
            None => ToolUpdateResult::Redraw,
        }
    }

    fn handle_key_event(&mut self, event: KeyEventMsg) -> ToolUpdateResult {
        if self.image.is_none() {
            return ToolUpdateResult::Unmodified;
        }
        match event.key {
            Key::Escape => {
                self.image = None;
                self.action = None;
                ToolUpdateResult::Redraw
            }
            Key::Return | Key::KP_Enter => self.commit_image(),
            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        if event.button != MouseButton::Primary {
            return ToolUpdateResult::Unmodified;
        }
        match event.type_ {
            MouseEventType::Click => {
                if self.image.is_none() {
                    self.open_file_dialog();
                }
                ToolUpdateResult::Unmodified
            }
            MouseEventType::BeginDrag => self.begin_drag(event.pos),
            MouseEventType::UpdateDrag => self.update_drag(event.pos, event.modifier),
            MouseEventType::EndDrag => {
                let result = self.update_drag(event.pos, event.modifier);
                self.action = None;
                result
            }
            MouseEventType::PointerPos => self.update_hover(event.pos),
            _ => ToolUpdateResult::Unmodified,
        }
    }
}
