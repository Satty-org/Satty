use std::cell::Cell;

use anyhow::Result;
use femtovg::{Color, ImageId, Paint, Path};
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

use super::{
    Drawable, InputContext, Tool, ToolUpdateResult, Tools,
    drag_box::{DragBox, draw_center_marker},
};

/// Where an inserted image goes, in image coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ImagePlacement {
    /// Centered on a point, scaled down to cover at most a fraction of the
    /// screenshot.
    Center(Vec2D),
    /// Scaled with its aspect ratio kept to fit into a box, and centered in it.
    Fit { top_left: Vec2D, size: Vec2D },
}

impl ImagePlacement {
    // a box narrower than this many screen pixels on either side has no room
    // for an image, and a click is such a box
    const MIN_BOX_SIZE: f32 = 8.0;

    // the placement a drag from `origin` asks for, given its end event
    fn from_drag(origin: Vec2D, event: &MouseEventMsg, sender: &Sender<SketchBoardInput>) -> Self {
        let drag_box = DragBox::from_origin_delta(origin, Vec2D::zero(), event, sender);
        // the same box on screen, so that what counts as a click does not
        // change with the zoom
        let screen_box =
        // FIXME was using screen_pos in old version - now we pass event and dragbox used pos
            DragBox::from_origin_delta(Vec2D::zero(), Vec2D::zero(), event, sender);

        if screen_box.size.x < Self::MIN_BOX_SIZE || screen_box.size.y < Self::MIN_BOX_SIZE {
            Self::Center(drag_box.middle())
        } else {
            Self::Fit {
                top_left: drag_box.top_left,
                size: drag_box.size,
            }
        }
    }
}

// the box being dragged out with the image tool
#[derive(Clone, Copy, Debug)]
struct DragPreview {
    origin: Vec2D,
    drag_box: DragBox,
}

impl Drawable for DragPreview {
    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: femtovg::FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        if self.drag_box.centered {
            draw_center_marker(canvas, self.origin);
        }

        let DragBox { top_left, size, .. } = self.drag_box;
        let mut path = Path::new();
        path.rect(top_left.x, top_left.y, size.x, size.y);
        // a light line under a dark one, so the box shows on any screenshot
        canvas.stroke_path(
            &path,
            &Paint::color(Color::rgbf(1.0, 1.0, 1.0)).with_line_width(3.0),
        );
        canvas.stroke_path(
            &path,
            &Paint::color(Color::rgbf(0.1, 0.1, 0.1)).with_line_width(1.0),
        );
        Ok(())
    }
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
                // the whole image stays on the screenshot, even for a drop
                // over a toolbar or a click beside a zoomed out screenshot
                let top_left = center - size * 0.5;
                let top_left = Vec2D::new(
                    top_left.x.min(background_size.x - size.x).max(0.0),
                    top_left.y.min(background_size.y - size.y).max(0.0),
                );
                (top_left, size)
            }
            ImagePlacement::Fit {
                top_left,
                size: box_size,
            } => {
                // a drawn box is a size the user asked for, so unlike above a
                // small image is enlarged to it
                let scale = (box_size.x / natural_size.x).min(box_size.y / natural_size.y);
                let size = natural_size * scale;
                (top_left + (box_size - size) * 0.5, size)
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
        hit_test_rectangle(pos, self.top_left, self.size, tolerance, true)
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
    // the box dragged out with the primary button, in image coordinates
    drag: Option<DragPreview>,
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
        // resized like any other annotation, so the tool only draws the box
        // being dragged out
        self.drag.as_ref().map(|drag| drag as &dyn Drawable)
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
                self.drag = Some(DragPreview {
                    origin: event.pos,
                    drag_box: DragBox::from_origin_delta(
                        event.pos,
                        Vec2D::zero(),
                        &event,
                        self.sender.as_ref().unwrap(),
                    ),
                });
                ToolUpdateResult::Unmodified
            }
            MouseEventType::UpdateDrag => {
                let Some(drag) = &mut self.drag else {
                    return ToolUpdateResult::Unmodified;
                };
                drag.drag_box = DragBox::from_origin_delta(
                    drag.origin,
                    Vec2D::zero(),
                    &event,
                    self.sender.as_ref().unwrap(),
                );
                ToolUpdateResult::Redraw
            }
            MouseEventType::EndDrag => {
                let Some(drag) = self.drag.take() else {
                    return ToolUpdateResult::Unmodified;
                };
                self.open_file_dialog(ImagePlacement::from_drag(
                    drag.origin,
                    &event,
                    self.sender.as_ref().unwrap(),
                ));
                ToolUpdateResult::Redraw
            }
            _ => ToolUpdateResult::Unmodified,
        }
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

    #[test]
    fn center_placement_keeps_the_whole_image_on_the_screenshot() {
        let at = |x, y| {
            layout(
                (100.0, 50.0),
                Some(ImagePlacement::Center(Vec2D::new(x, y))),
            )
            .0
        };
        assert_eq!(at(-30.0, 700.0), Vec2D::new(0.0, 550.0));
        assert_eq!(at(900.0, -5.0), Vec2D::new(700.0, 0.0));
        assert_eq!(at(100.0, 200.0), Vec2D::new(50.0, 175.0));
    }

    #[test]
    fn fit_placement_may_leave_the_screenshot() {
        // a box is wherever the user drew it, as with the other box tools
        assert_eq!(
            layout(
                (10.0, 10.0),
                Some(ImagePlacement::Fit {
                    top_left: Vec2D::new(-50.0, -50.0),
                    size: Vec2D::new(100.0, 100.0),
                })
            ),
            (Vec2D::new(-50.0, -50.0), Vec2D::new(100.0, 100.0))
        );
    }

    #[test]
    fn fit_placement_keeps_the_aspect_ratio_and_centers_in_the_box() {
        // the height is the tighter limit: 100 / 50 = 2
        assert_eq!(
            layout(
                (100.0, 50.0),
                Some(ImagePlacement::Fit {
                    top_left: Vec2D::new(10.0, 10.0),
                    size: Vec2D::new(400.0, 100.0),
                })
            ),
            (Vec2D::new(110.0, 10.0), Vec2D::new(200.0, 100.0))
        );
    }

    #[test]
    fn fit_placement_enlarges_a_small_image() {
        assert_eq!(
            layout(
                (10.0, 10.0),
                Some(ImagePlacement::Fit {
                    top_left: Vec2D::new(0.0, 0.0),
                    size: Vec2D::new(100.0, 200.0),
                })
            ),
            (Vec2D::new(0.0, 50.0), Vec2D::new(100.0, 100.0))
        );
    }

    // FIXME need to create a Sender stub for the from_drag tests

    // // the end of a drag: `delta` in image coordinates, `screen_delta` in
    // // screen pixels, which differ by the zoom
    // fn end_drag(delta: Vec2D, screen_delta: Vec2D, modifier: ModifierType) -> MouseEventMsg {
    //     MouseEventMsg {
    //         type_: MouseEventType::EndDrag,
    //         button: MouseButton::Primary,
    //         modifier,
    //         screen_pos: screen_delta,
    //         is_touchpad: false,
    //         pos: delta,
    //         n_pressed: 1,
    //         release: false,
    //     }
    // }

    // fn from_drag(
    //     delta: (f32, f32),
    //     screen_delta: (f32, f32),
    //     sender: &Sender<SketchBoardInput>,
    // ) -> ImagePlacement {
    //     ImagePlacement::from_drag(
    //         Vec2D::new(50.0, 50.0),
    //         &end_drag(
    //             Vec2D::new(delta.0, delta.1),
    //             Vec2D::new(screen_delta.0, screen_delta.1),
    //             ModifierType::empty(),
    //         ),
    //         sender,
    //     )
    // }

    // #[test]
    // fn a_drag_fits_into_the_dragged_box() {
    //     assert_eq!(
    //         from_drag((100.0, -40.0), (100.0, -40.0), sender),
    //         ImagePlacement::Fit {
    //             top_left: Vec2D::new(50.0, 10.0),
    //             size: Vec2D::new(100.0, 40.0),
    //         }
    //     );
    // }

    // #[test]
    // fn a_press_that_barely_moved_centers_on_the_press() {
    //     assert_eq!(
    //         from_drag((0.0, 0.0), (0.0, 0.0)),
    //         ImagePlacement::Center(Vec2D::new(50.0, 50.0))
    //     );
    //     assert_eq!(
    //         from_drag((2.0, 2.0), (2.0, 2.0)),
    //         ImagePlacement::Center(Vec2D::new(51.0, 51.0))
    //     );
    // }

    // #[test]
    // fn a_box_too_thin_for_an_image_centers_on_its_middle() {
    //     assert_eq!(
    //         from_drag((100.0, 2.0), (100.0, 2.0)),
    //         ImagePlacement::Center(Vec2D::new(100.0, 51.0))
    //     );
    // }

    // #[test]
    // fn what_counts_as_a_click_does_not_depend_on_the_zoom() {
    //     // zoomed in: a few image pixels are a real drag on screen
    //     assert!(matches!(
    //         from_drag((5.0, 5.0), (20.0, 20.0)),
    //         ImagePlacement::Fit { .. }
    //     ));
    //     // zoomed out: many image pixels are still a click on screen
    //     assert!(matches!(
    //         from_drag((20.0, 20.0), (5.0, 5.0)),
    //         ImagePlacement::Center(_)
    //     ));
    // }

    // #[test]
    // fn alt_centers_the_box_on_the_press() {
    //     let placement = ImagePlacement::from_drag(
    //         Vec2D::new(50.0, 50.0),
    //         &end_drag(
    //             Vec2D::new(40.0, 20.0),
    //             Vec2D::new(40.0, 20.0),
    //             ModifierType::ALT_MASK,
    //         ),
    //     );
    //     assert_eq!(
    //         placement,
    //         ImagePlacement::Fit {
    //             top_left: Vec2D::new(30.0, 40.0),
    //             size: Vec2D::new(40.0, 20.0),
    //         }
    //     );
    // }
}
