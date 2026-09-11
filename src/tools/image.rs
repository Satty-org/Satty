use std::cell::Cell;

use anyhow::Result;
use femtovg::{ImageId, Paint, Path};
use relm4::gtk::gdk_pixbuf::Pixbuf;
use relm4::gtk::prelude::*;
use relm4::{RelmWidgetExt, Sender, gtk};

use crate::{
    configuration::APP_CONFIG,
    femtovg_area::create_image_from_pixbuf,
    image_loading,
    math::{self, Vec2D},
    notification::log_result,
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType, SketchBoardInput},
    tools::hit_test_rectangle,
};

use super::{Drawable, InputContext, Tool, ToolUpdateResult, Tools};

/// Where an inserted image goes, in image coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ImagePlacement {
    /// Centered on a point, scaled down to cover at most a fraction of the
    /// screenshot.
    Center(Vec2D),
}

#[derive(Clone, Debug)]
pub struct Image {
    pixbuf: Pixbuf,
    top_left: Vec2D,
    size: Vec2D,
    // the canvas owns the uploaded texture, so it is only known once drawn
    cached_image_id: Cell<Option<ImageId>>,
}

impl Image {
    // maximum fraction of the background image an inserted image covers initially
    const INITIAL_SIZE_FRACTION: f32 = 0.5;

    fn new(pixbuf: Pixbuf, background_size: Vec2D, placement: Option<ImagePlacement>) -> Self {
        let natural_size = Vec2D::new(pixbuf.width() as f32, pixbuf.height() as f32);
        let (top_left, size) = Self::layout(natural_size, background_size, placement);

        Self {
            pixbuf,
            top_left,
            size,
            cached_image_id: Cell::new(None),
        }
    }

    // top left corner and size of an image of the given natural size; without
    // a placement it is centered on the screenshot
    fn layout(
        natural_size: Vec2D,
        background_size: Vec2D,
        placement: Option<ImagePlacement>,
    ) -> (Vec2D, Vec2D) {
        match placement.unwrap_or(ImagePlacement::Center(background_size * 0.5)) {
            ImagePlacement::Center(center) => {
                // shrink to fit, but never blow a small image up
                let scale = (background_size.x * Self::INITIAL_SIZE_FRACTION / natural_size.x)
                    .min(background_size.y * Self::INITIAL_SIZE_FRACTION / natural_size.y)
                    .min(1.0);
                let size = natural_size * scale;
                (center - size * 0.5, size)
            }
        }
    }
}

impl Drawable for Image {
    fn bounds(&self) -> Option<(Vec2D, Vec2D)> {
        Some(math::ensure_bounding_box(
            self.top_left,
            self.top_left + self.size,
        ))
    }

    fn hit_test(&self, pos: Vec2D, tolerance: f32) -> bool {
        hit_test_rectangle(pos, self.top_left, Some(self.size), tolerance, true)
    }

    fn translate(&mut self, delta: Vec2D) {
        self.top_left += delta;
    }

    fn resize_bounds(&mut self, tl: Vec2D, br: Vec2D) {
        let (tl, br) = math::ensure_bounding_box(tl, br);
        self.top_left = tl;
        self.size = br - tl;
    }

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

        let mut path = Path::new();
        path.rect(self.top_left.x, self.top_left.y, self.size.x, self.size.y);
        canvas.fill_path(
            &path,
            &Paint::image(
                image_id,
                self.top_left.x,
                self.top_left.y,
                self.size.x,
                self.size.y,
                0f32,
                1f32,
            ),
        );

        Ok(())
    }
}

#[derive(Default)]
pub struct ImageTool {
    input_enabled: bool,
    input_context: Option<InputContext>,
    sender: Option<Sender<SketchBoardInput>>,
    // gtk does not keep the native dialog alive, dropping it closes the
    // dialog and crashes gtk internals, so hold on to it until the response
    dialog: Option<gtk::FileChooserNative>,
    // where the primary button went down, in image coordinates
    drag_origin: Option<Vec2D>,
}

impl ImageTool {
    fn open_file_dialog(&mut self, placement: ImagePlacement) {
        let Some(sender) = self.sender.clone() else {
            return;
        };
        // a second dialog would take over the field below and leave the first
        // one without an owner, which gtk does not survive
        if self.dialog.as_ref().is_some_and(|d| d.is_visible()) {
            return;
        }
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
                    Ok(pixbuf) => sender.emit(SketchBoardInput::ImagePlaced(pixbuf, placement)),
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

    fn get_drawable(&self) -> Option<&dyn Drawable> {
        // an inserted image is committed right away and from then on moved and
        // resized like any other annotation, so the tool holds no drawable
        None
    }

    fn handle_image_selected(
        &mut self,
        pixbuf: Pixbuf,
        background_size: Vec2D,
        placement: Option<ImagePlacement>,
    ) -> ToolUpdateResult {
        ToolUpdateResult::Commit(Box::new(Image::new(pixbuf, background_size, placement)))
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        if event.button != MouseButton::Primary {
            return ToolUpdateResult::Unmodified;
        }
        // a click arrives on press, before it is known whether the press
        // becomes a drag, so the dialog waits for the release of the drag
        // gesture that every press also starts
        match event.type_ {
            MouseEventType::BeginDrag => {
                self.drag_origin = Some(event.pos);
            }
            MouseEventType::EndDrag => {
                if let Some(origin) = self.drag_origin.take() {
                    self.open_file_dialog(ImagePlacement::Center(origin));
                }
            }
            _ => {}
        }
        ToolUpdateResult::Unmodified
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(natural: (f32, f32), placement: Option<ImagePlacement>) -> (Vec2D, Vec2D) {
        Image::layout(
            Vec2D::new(natural.0, natural.1),
            Vec2D::new(800.0, 600.0),
            placement,
        )
    }

    #[test]
    fn without_placement_centers_on_the_screenshot() {
        assert_eq!(
            layout((100.0, 50.0), None),
            (Vec2D::new(350.0, 275.0), Vec2D::new(100.0, 50.0))
        );
    }

    #[test]
    fn center_placement_centers_on_the_point() {
        assert_eq!(
            layout(
                (100.0, 50.0),
                Some(ImagePlacement::Center(Vec2D::new(100.0, 100.0)))
            ),
            (Vec2D::new(50.0, 75.0), Vec2D::new(100.0, 50.0))
        );
    }

    #[test]
    fn center_placement_shrinks_to_half_the_screenshot() {
        // the height is the tighter limit: 300 / 1200 = 0.25
        let (_, size) = layout(
            (1600.0, 1200.0),
            Some(ImagePlacement::Center(Vec2D::new(0.0, 0.0))),
        );
        assert_eq!(size, Vec2D::new(400.0, 300.0));
    }
}
