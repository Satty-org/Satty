use std::cell::RefCell;
use std::rc::Rc;

use femtovg::{
    Canvas, Color, CompositeOperation, ImageFlags, ImageId, Paint, Path, RenderTarget,
    renderer::OpenGl,
};

use crate::{
    math::{self, Vec2D},
    tools::{Drawable, RenderingMode, SpotlightData, Tool, drag_box::draw_center_marker},
};

pub fn render_spotlights(
    canvas: &mut Canvas<OpenGl>,
    active_tool: &Rc<RefCell<dyn Tool>>,
    drawables: &[Box<dyn Drawable>],
    hidden_drawable_index: Option<usize>,
    bounds: (Vec2D, Vec2D),
    background_image_id: ImageId,
    output_target: RenderTarget,
) {
    // for collecting spotlight boxes
    let mut spotlight_blur_boxes: Vec<SpotlightData> = Vec::new();
    let mut spotlight_highlight_boxes: Vec<SpotlightData> = Vec::new();
    let mut spotlight_preview = false;
    let mut preview_blur_spotlight: Option<Box<dyn Drawable>> = None;
    let mut preview_highlight_spotlight: Option<Box<dyn Drawable>> = None;

    // check if the active tool is a spotlight preview and collect its anchors
    if let Some(preview) = active_tool.borrow().get_drawable()
        && matches!(
            preview.get_rendering_mode(),
            RenderingMode::SpotlightBlur | RenderingMode::SpotlightHighlight
        )
        && let Some(preview_anchors) = preview.anchors()
    {
        if preview.get_rendering_mode() == RenderingMode::SpotlightBlur {
            preview_blur_spotlight = Some(preview.clone_box());
            spotlight_blur_boxes.push(SpotlightData {
                tl: preview_anchors.0,
                br: preview_anchors.1,
                filled: preview.get_style().is_some_and(|style| style.fill),
                editing: true,
                centered: preview.get_centered(),
            });
        } else {
            preview_highlight_spotlight = Some(preview.clone_box());
            spotlight_highlight_boxes.push(SpotlightData {
                tl: preview_anchors.0,
                br: preview_anchors.1,
                filled: preview.get_style().is_some_and(|style| style.fill),
                editing: true,
                centered: preview.get_centered(),
            });
        }
        spotlight_preview = true;
    }

    // iterate over all drawables to collect spotlight boxes and find the first spotlight of each kind
    let mut first_blur_spotlight: Option<&dyn Drawable> = None;
    let mut first_highlight_spotlight: Option<&dyn Drawable> = None;
    for (index, drawable) in drawables.iter().enumerate() {
        if hidden_drawable_index == Some(index) {
            continue;
        }
        if matches!(
            drawable.get_rendering_mode(),
            RenderingMode::SpotlightBlur | RenderingMode::SpotlightHighlight
        ) && let Some(drawable_anchors) = drawable.anchors()
        {
            let data = SpotlightData {
                tl: drawable_anchors.0,
                br: drawable_anchors.1,
                filled: drawable.get_style().is_some_and(|style| style.fill),
                editing: false,
                centered: false,
            };
            match drawable.get_rendering_mode() {
                RenderingMode::SpotlightBlur => spotlight_blur_boxes.push(data),
                RenderingMode::SpotlightHighlight => spotlight_highlight_boxes.push(data),
                _ => {}
            }
        }

        // find set first spotlight of each kind
        match drawable.get_rendering_mode() {
            RenderingMode::SpotlightBlur if first_blur_spotlight.is_none() => {
                first_blur_spotlight = Some(drawable.as_ref());
            }
            RenderingMode::SpotlightHighlight if first_highlight_spotlight.is_none() => {
                first_highlight_spotlight = Some(drawable.as_ref());
            }
            _ => {}
        }
    }

    // draw spotlights
    let blur_spotlight_drawable = first_blur_spotlight.or(preview_blur_spotlight.as_deref());
    if let Some(drawable) = blur_spotlight_drawable {
        drawable.draw_spotlight(
            canvas,
            bounds,
            &spotlight_blur_boxes,
            spotlight_preview,
            background_image_id,
            output_target,
        );
    }

    let highlight_spotlight_drawable =
        first_highlight_spotlight.or(preview_highlight_spotlight.as_deref());
    if let Some(drawable) = highlight_spotlight_drawable {
        drawable.draw_spotlight(
            canvas,
            bounds,
            &spotlight_highlight_boxes,
            spotlight_preview,
            background_image_id,
            output_target,
        );
    }
}

pub(super) fn draw_spotlight_layer(
    canvas: &mut Canvas<OpenGl>,
    boxes: &[SpotlightData],
    corner_radius: f32,
    output_target: RenderTarget,
    // width, size, image_id - as they depend on render target
    cached_layer: &RefCell<Option<(u32, u32, ImageId)>>,
    draw_content: impl FnOnce(&mut Canvas<OpenGl>),
) {
    let (width, height) = (canvas.width(), canvas.height());

    let layer = {
        let mut cached_layer = cached_layer.borrow_mut();
        if let Some((cached_width, cached_height, layer)) = *cached_layer
            && cached_width == width
            && cached_height == height
        {
            layer
        } else {
            if let Some((_, _, old_layer)) = cached_layer.take() {
                canvas.delete_image(old_layer);
            }
            let layer = canvas
                .create_image_empty(
                    width as usize,
                    height as usize,
                    femtovg::PixelFormat::Rgba8,
                    ImageFlags::FLIP_Y,
                )
                .unwrap();
            *cached_layer = Some((width, height, layer));
            layer
        }
    };

    canvas.save();
    canvas.set_render_target(RenderTarget::Image(layer));
    canvas.clear_rect(0, 0, width, height, Color::rgbaf(0.0, 0.0, 0.0, 0.0));

    draw_content(canvas);

    canvas.global_composite_operation(CompositeOperation::DestinationOut);
    let mut holes = Path::new();
    for SpotlightData { tl, br, filled, .. } in boxes {
        let (tl, size) = math::rect_ensure_positive_size(*tl, *br - *tl);
        if *filled {
            holes.rounded_rect(tl.x, tl.y, size.x, size.y, corner_radius);
        } else {
            let (center, radii) = (tl + size / 2.0, size / 2.0);
            holes.ellipse(center.x, center.y, radii.x, radii.y);
        }
    }
    canvas.fill_path(&holes, &Paint::color(Color::black()));
    canvas.global_composite_operation(CompositeOperation::SourceOver);

    canvas.set_render_target(output_target);
    canvas.reset_transform();
    let mut layer_path = Path::new();
    layer_path.rect(0.0, 0.0, width as f32, height as f32);
    canvas.fill_path(
        &layer_path,
        &Paint::image(layer, 0.0, 0.0, width as f32, height as f32, 0.0, 1.0),
    );
    canvas.restore();

    if let Some(spotlight_data) = boxes.first()
        && spotlight_data.editing
        && spotlight_data.centered
    {
        let center = (spotlight_data.tl + spotlight_data.br) / 2.0;
        draw_center_marker(canvas, center);
    }
}
