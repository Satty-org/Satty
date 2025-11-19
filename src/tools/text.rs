use anyhow::Result;
use femtovg::{Color, FontId, Paint, Path};
use relm4::gtk::prelude::IMContextExt;
use relm4::gtk::{
    gdk::{Key, ModifierType, Rectangle},
    TextBuffer,
};
use std::{borrow::Cow, ops::Range};

use relm4::gtk::prelude::*;

use crate::{
    ime::preedit::{Preedit, UnderlineKind},
    math::Vec2D,
    sketch_board::{KeyEventMsg, MouseButton, MouseEventMsg, MouseEventType, TextEventMsg},
    style::Style,
};

use super::{Drawable, DrawableClone, InputContext, Tool, ToolUpdateResult, Tools};
use crate::sketch_board::SketchBoardInput;
use relm4::gtk::gdk::DisplayManager;
use relm4::Sender;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone, Debug)]
pub struct Text {
    pos: Vec2D,
    editing: bool,
    text_buffer: TextBuffer,
    style: Style,
    preedit: Option<Preedit>,
    im_context: Option<InputContext>,
    rect: RefCell<Rectangle>,
    glyphs: RefCell<Vec<Vec<Rectangle>>>,
    line_ranges: RefCell<Vec<Range<usize>>>,
    cursor_visible: RefCell<bool>,
    draw_rect: RefCell<bool>,
}

struct DisplayContent<'a> {
    text: Cow<'a, str>,
    cursor_byte_pos: usize,
    preedit_range: Option<Range<usize>>,
}

struct LineLayout {
    range: Range<usize>,
    baseline: f32,
}

struct TextDrawingContext<'a> {
    paint: &'a Paint,
    text: &'a str,
    lines: &'a [LineLayout],
}

#[derive(Clone, Copy)]
struct CursorMetrics {
    top_offset: f32,
    height: f32,
    line_height: f32,
}

impl Text {
    fn new(pos: Vec2D, style: Style, im_context: Option<InputContext>) -> Self {
        let text_buffer = TextBuffer::new(None);
        text_buffer.set_enable_undo(true);

        Self {
            pos,
            text_buffer,
            editing: true,
            style,
            preedit: None,
            im_context,
            rect: RefCell::new(Rectangle::new(0, 0, 0, 0)),
            glyphs: RefCell::new(Vec::new()),
            line_ranges: RefCell::new(Vec::new()),
            cursor_visible: RefCell::new(true),
            draw_rect: RefCell::new(true),
        }
    }

    fn byte_index_from_char_index(text: &str, char_index: usize) -> usize {
        text.char_indices()
            .nth(char_index)
            .map(|(idx, _)| idx)
            .unwrap_or_else(|| text.len())
    }

    fn display_text<'a>(&self, base_text: &'a str) -> DisplayContent<'a> {
        let cursor_char_index = self.text_buffer.cursor_position() as usize;
        let base_cursor_byte = Self::byte_index_from_char_index(base_text, cursor_char_index);

        if self.editing {
            if let Some(preedit) = &self.preedit {
                if preedit.text.is_empty() {
                    return DisplayContent {
                        text: Cow::Borrowed(base_text),
                        cursor_byte_pos: base_cursor_byte,
                        preedit_range: None,
                    };
                }

                let mut composed = String::with_capacity(base_text.len() + preedit.text.len());
                composed.push_str(&base_text[..base_cursor_byte]);
                composed.push_str(&preedit.text);
                composed.push_str(&base_text[base_cursor_byte..]);

                let preedit_char_len = preedit.text.chars().count();
                let cursor_chars = preedit
                    .cursor_chars
                    .map(|value| value.min(preedit_char_len))
                    .unwrap_or(preedit_char_len);
                let preedit_cursor_byte =
                    Self::byte_index_from_char_index(&preedit.text, cursor_chars);
                let composed_cursor_byte = base_cursor_byte + preedit_cursor_byte;

                DisplayContent {
                    text: Cow::Owned(composed),
                    cursor_byte_pos: composed_cursor_byte,
                    preedit_range: Some(base_cursor_byte..base_cursor_byte + preedit.text.len()),
                }
            } else {
                DisplayContent {
                    text: Cow::Borrowed(base_text),
                    cursor_byte_pos: base_cursor_byte,
                    preedit_range: None,
                }
            }
        } else {
            DisplayContent {
                text: Cow::Borrowed(base_text),
                cursor_byte_pos: base_cursor_byte,
                preedit_range: None,
            }
        }
    }
}

impl Drawable for Text {
    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        font: FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        let gtext = self.text_buffer.text(
            &self.text_buffer.start_iter(),
            &self.text_buffer.end_iter(),
            false,
        );
        let base_text = gtext.as_str();
        let display = self.display_text(base_text);
        let text = display.text.as_ref();

        let mut base_paint: Paint = self.style.into();
        base_paint.set_font(&[font]);

        let transform = canvas.transform();
        let canva_scale = transform.average_scale();

        let width = _bounds.1.x - self.pos.x;

        let lines = canvas.break_text_vec(width, text, &base_paint)?;
        self.line_ranges.replace(lines.clone());

        let font_metrics = canvas.measure_font(&base_paint)?;
        let measured_cursor = canvas
            .measure_text(self.pos.x, self.pos.y, "|", &base_paint)
            .ok();

        let mut line_height = measured_cursor
            .as_ref()
            .map(|metrics| metrics.height())
            .unwrap_or(0.0);
        if line_height <= 0.0 {
            let ascender_plus_descender = font_metrics.ascender() + font_metrics.descender();
            if ascender_plus_descender.abs() > f32::EPSILON {
                line_height = ascender_plus_descender.abs() / canva_scale;
            }
        }
        if line_height <= 0.0 {
            line_height = font_metrics.height() / canva_scale;
        }

        let cursor_top_offset = -line_height;
        let cursor_height = if line_height.abs() > f32::EPSILON {
            line_height.abs()
        } else {
            (font_metrics.height() / canva_scale).abs()
        };

        let mut line_layouts: Vec<LineLayout> = Vec::with_capacity(lines.len());
        let mut baseline = self.pos.y;
        for line_range in &lines {
            line_layouts.push(LineLayout {
                range: line_range.clone(),
                baseline,
            });
            baseline += line_height;
        }

        let cursor_metrics = CursorMetrics {
            top_offset: cursor_top_offset,
            height: cursor_height,
            line_height,
        };

        let layout_context = TextDrawingContext {
            paint: &base_paint,
            text,
            lines: &line_layouts,
        };

        if self.editing {
            if let (Some(preedit), Some(preedit_range)) = (&self.preedit, &display.preedit_range) {
                self.draw_preedit_background(
                    canvas,
                    &layout_context,
                    preedit,
                    preedit_range,
                    cursor_metrics,
                );
            }
        }

        let mut cursor_visible = self.cursor_visible.borrow_mut();
        //draw selection
        if let Some((sel_start_iter, sel_end_iter)) = self.text_buffer.selection_bounds() {
            let sel_start = sel_start_iter.offset() as usize;
            let sel_end = sel_end_iter.offset() as usize;

            for line in &line_layouts {
                let start_index = text[..line.range.start].chars().count();
                let end_index = text[..line.range.end].chars().count();

                let overlap_start = sel_start.max(start_index);
                let overlap_end = sel_end.min(end_index);
                if overlap_start >= overlap_end {
                    continue;
                }

                let segments = self.segments_for_line_span(
                    canvas,
                    &layout_context,
                    line,
                    overlap_start..overlap_end,
                );
                for (start_x, end_x) in segments {
                    let mut path = Path::new();

                    let offset_y = cursor_metrics.height * 0.1;
                    let y = line.baseline + cursor_metrics.top_offset + offset_y;
                    let h = cursor_metrics.height;
                    let x = start_x;
                    let w = end_x - start_x;

                    path.rect(x, y, w, h);
                    let mut paint = Paint::color(Color::rgbaf(0.3, 0.5, 1.0, 0.3)); // transparent blue
                    paint.set_anti_alias(true);
                    canvas.fill_path(&path, &paint);
                }
            }

            *cursor_visible = false;
        } else {
            *cursor_visible = true;
        }

        //calculate rect and glyphs
        let mut draw_baseline = self.pos.y;
        let mut rect = self.rect.borrow_mut();
        let mut glyphs = self.glyphs.borrow_mut();

        glyphs.clear();
        {
            let mut top = 0;
            let mut left = 0;
            let mut width = 0;
            let mut height = 0;

            for line in &line_layouts {
                let mut line_glyphs = Vec::new();

                let start = text[..line.range.start].chars().count();
                let end = text[..line.range.end].chars().count();

                for i in start..end {
                    let segments =
                        self.segments_for_line_span(canvas, &layout_context, line, i..i + 1);

                    for (start_x, end_x) in segments {
                        let offset_y = cursor_metrics.height * 0.1;
                        let y = (line.baseline + cursor_metrics.top_offset + offset_y) as i32;
                        let h = cursor_metrics.height as i32;
                        let x = start_x as i32;
                        let w = (end_x - start_x) as i32;
                        line_glyphs.push(Rectangle::new(x, y, w, h));

                        if top == 0 {
                            top = y;
                        }

                        if left == 0 {
                            left = x;
                        }

                        width = (end_x as i32 - left).max(width);
                        height = y + h - top;
                    }
                }

                glyphs.push(line_glyphs);

                rect.set_height(height);
                rect.set_width(width);
                rect.set_x(left);
                rect.set_y(top);
            }
        }

        //draw rect
        if *self.draw_rect.borrow() {
            let mut rect_paint = Path::new();
            rect_paint.move_to(self.pos.x, self.pos.y);
            let y = rect.y() as f32;
            let h = rect.height() as f32;
            let x = rect.x() as f32;
            let w = rect.width() as f32;

            rect_paint.rect(x, y, w, h);
            let mut paint = Paint::color(Color::rgbaf(1.0, 0.5, 0.3, 0.3)); // transparent orange
            paint.set_anti_alias(true);
            paint.set_line_width(2.0);
            canvas.stroke_path(&rect_paint, &paint);
        }

        for line_range in &lines {
            canvas.fill_text(
                self.pos.x,
                draw_baseline,
                &text[line_range.clone()],
                &base_paint,
            )?;
            draw_baseline += line_height;
        }

        if self.editing {
            if let (Some(preedit), Some(preedit_range)) = (&self.preedit, &display.preedit_range) {
                self.draw_preedit_overlays(
                    canvas,
                    font,
                    &layout_context,
                    preedit,
                    preedit_range,
                    cursor_metrics,
                )?;
            }
        }

        if self.editing {
            self.draw_cursor_and_update_ime(
                canvas,
                font,
                &layout_context,
                cursor_metrics,
                display.cursor_byte_pos,
                *cursor_visible,
            );
        }

        Ok(())
    }
}

impl Text {
    fn draw_preedit_background(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        context: &TextDrawingContext<'_>,
        preedit: &Preedit,
        preedit_range: &Range<usize>,
        cursor: CursorMetrics,
    ) {
        for span in &preedit.spans {
            let Some(background_color) = span.background else {
                continue;
            };
            let global_start = preedit_range.start + span.range.start;
            let global_end = preedit_range.start + span.range.end;

            for line in context.lines {
                let overlap_start = global_start.max(line.range.start);
                let overlap_end = global_end.min(line.range.end);
                if overlap_start >= overlap_end {
                    continue;
                }
                let segments =
                    self.segments_for_line_span(canvas, context, line, overlap_start..overlap_end);
                for (start_x, end_x) in segments {
                    let width = (end_x - start_x).max(0.0);
                    if width <= f32::EPSILON {
                        continue;
                    }
                    let mut path = Path::new();
                    let top = line.baseline + cursor.top_offset;
                    path.rect(start_x, top, width, cursor.height);
                    let mut fill_paint = Paint::color(background_color.into());
                    fill_paint.set_anti_alias(true);
                    canvas.fill_path(&path, &fill_paint);
                }
            }
        }
    }

    fn draw_preedit_overlays(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        font: FontId,
        context: &TextDrawingContext<'_>,
        preedit: &Preedit,
        preedit_range: &Range<usize>,
        cursor: CursorMetrics,
    ) -> Result<()> {
        for span in &preedit.spans {
            let global_start = preedit_range.start + span.range.start;
            let global_end = preedit_range.start + span.range.end;

            for line in context.lines {
                let overlap_start = global_start.max(line.range.start);
                let overlap_end = global_end.min(line.range.end);
                if overlap_start >= overlap_end {
                    continue;
                }
                let segments =
                    self.segments_for_line_span(canvas, context, line, overlap_start..overlap_end);
                if segments.is_empty() {
                    continue;
                }

                if let Some(color) = span.foreground {
                    let mut overlay_paint: Paint = self.style.into();
                    overlay_paint.set_font(&[font]);
                    overlay_paint.set_color(color.into());
                    for (start_x, end_x) in &segments {
                        let width = (*end_x - *start_x).max(0.0);
                        if width <= f32::EPSILON {
                            continue;
                        }
                        canvas.save();
                        canvas.scissor(
                            (*start_x - 1.0).floor(),
                            (line.baseline + cursor.top_offset - 1.0).floor(),
                            (width + 2.0).ceil(),
                            (cursor.height + 2.0).ceil(),
                        );
                        canvas.fill_text(
                            self.pos.x,
                            line.baseline,
                            &context.text[line.range.clone()],
                            &overlay_paint,
                        )?;
                        canvas.restore();
                    }
                }

                if span.underline != UnderlineKind::None {
                    let color = span
                        .underline_color
                        .or(span.foreground)
                        .unwrap_or(self.style.color);
                    self.draw_underline_segments(
                        canvas,
                        &segments,
                        line.baseline + cursor.top_offset,
                        cursor.height,
                        span.underline,
                        color,
                    );
                }
            }
        }

        Ok(())
    }

    fn draw_underline_segments(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        segments: &[(f32, f32)],
        line_top: f32,
        cursor_height: f32,
        underline: UnderlineKind,
        color: crate::style::Color,
    ) {
        if segments.is_empty() {
            return;
        }
        let mut paint = Paint::color(color.into());
        let thickness = (cursor_height * 0.08).clamp(1.0, cursor_height / 2.0);
        paint.set_line_width(thickness);
        paint.set_anti_alias(true);

        let base_y = line_top + cursor_height - thickness * 0.5;

        for &(start_x, end_x) in segments {
            if end_x - start_x <= f32::EPSILON {
                continue;
            }
            match underline {
                UnderlineKind::Double => {
                    let mut first = Path::new();
                    first.move_to(start_x, base_y - thickness);
                    first.line_to(end_x, base_y - thickness);
                    canvas.stroke_path(&first, &paint);

                    let mut second = Path::new();
                    second.move_to(start_x, base_y + thickness * 0.5);
                    second.line_to(end_x, base_y + thickness * 0.5);
                    canvas.stroke_path(&second, &paint);
                }
                UnderlineKind::None => {}
                _ => {
                    let mut path = Path::new();
                    path.move_to(start_x, base_y);
                    path.line_to(end_x, base_y);
                    canvas.stroke_path(&path, &paint);
                }
            }
        }
    }

    fn segments_for_line_span(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        context: &TextDrawingContext<'_>,
        line: &LineLayout,
        range: Range<usize>,
    ) -> Vec<(f32, f32)> {
        if range.start >= range.end {
            return Vec::new();
        }

        let chars_without_newline: Vec<(usize, char)> = context.text.char_indices().collect();

        let range_start_byte = chars_without_newline
            .get(range.start)
            .map(|(i, _)| *i)
            .unwrap_or(context.text.len());

        let range_end_byte = chars_without_newline
            .get(range.end)
            .map(|(i, _)| *i)
            .unwrap_or(context.text.len());

        let line_start = line.range.start;
        let line_end = line.range.end;
        let overlap_start = range_start_byte.max(line_start).min(line_end);
        let overlap_end = range_end_byte.max(line_start).min(line_end);

        if overlap_start >= overlap_end {
            return Vec::new();
        }

        let line_text = &context.text[line.range.clone()];

        let start_byte = overlap_start.saturating_sub(line_start);
        let end_byte = overlap_end.saturating_sub(line_start);

        let prefix = &line_text[..start_byte];
        let selected = &line_text[start_byte..end_byte].replace("\n", "");

        let start_x: f32 = self.pos.x + Self::text_width(canvas, context.paint, prefix);
        let width = Self::text_width(canvas, context.paint, selected);

        vec![(start_x, start_x + width.max(0.0))]
    }

    fn caret_top_left(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        context: &TextDrawingContext<'_>,
        cursor_byte_pos: usize,
        cursor: CursorMetrics,
    ) -> (f32, f32) {
        if context.lines.is_empty() {
            return (self.pos.x, self.pos.y + cursor.top_offset);
        }

        let mut newline_pending_baseline: Option<f32> = None;

        for line in context.lines {
            let line_text = &context.text[line.range.clone()];

            if cursor_byte_pos < line.range.end {
                let prefix_len = cursor_byte_pos
                    .saturating_sub(line.range.start)
                    .min(line_text.len());
                let prefix = &line_text[..prefix_len];
                let offset = Self::text_width(canvas, context.paint, prefix);
                return (self.pos.x + offset, line.baseline + cursor.top_offset);
            }

            if cursor_byte_pos == line.range.end {
                if line_text.ends_with('\n') {
                    // The caret is positioned right after a manual line break,
                    // so place it on the next visual line instead.
                    newline_pending_baseline =
                        Some(line.baseline + cursor.top_offset + cursor.line_height);
                    continue;
                }
                let offset = Self::text_width(canvas, context.paint, line_text);
                return (self.pos.x + offset, line.baseline + cursor.top_offset);
            }
        }

        if let Some(baseline) = newline_pending_baseline {
            return (self.pos.x, baseline);
        }

        if let Some(last_line) = context.lines.last() {
            let line_text = &context.text[last_line.range.clone()];
            let offset = Self::text_width(canvas, context.paint, line_text);
            (
                self.pos.x + offset,
                last_line.baseline + cursor.top_offset + cursor.line_height,
            )
        } else {
            (self.pos.x, self.pos.y + cursor.top_offset)
        }
    }

    fn draw_cursor_and_update_ime(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        font: FontId,
        context: &TextDrawingContext<'_>,
        cursor: CursorMetrics,
        cursor_byte_pos: usize,
        cursor_visible: bool,
    ) {
        let (cursor_x, cursor_top) = self.caret_top_left(canvas, context, cursor_byte_pos, cursor);
        let caret_height = cursor.height;

        let mut caret_paint: Paint = self.style.into();
        caret_paint.set_font(&[font]);

        if cursor_visible {
            let extra_height = caret_height * 0.05;
            let mut path = Path::new();
            path.move_to(cursor_x, cursor_top - extra_height);
            path.line_to(cursor_x, cursor_top + caret_height + extra_height * 2.0);
            canvas.fill_path(&path, &caret_paint);
        }

        if self.editing {
            if let Some(handle) = &self.im_context {
                let transform = canvas.transform();
                let widget_scale = handle.widget.scale_factor().max(1) as f32;
                let (x1, y1) = transform.transform_point(cursor_x, cursor_top);
                let (x2, y2) = transform.transform_point(cursor_x + 1.0, cursor_top + caret_height);
                let logical_x = (x1 / widget_scale).floor() as i32;
                let logical_y = (y1 / widget_scale).floor() as i32;
                let logical_width = ((x2 - x1).abs() / widget_scale).ceil().max(1.0) as i32;
                let logical_height = ((y2 - y1).abs() / widget_scale).ceil().max(1.0) as i32;
                let rect =
                    Rectangle::new(logical_x, logical_y, logical_width, logical_height.max(1));
                handle.im_context.set_cursor_location(&rect);
            }
        }
    }

    fn text_width(
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        paint: &Paint,
        text: &str,
    ) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        canvas
            .measure_text(0.0, 0.0, text, paint)
            .map(|metrics| metrics.width())
            .unwrap_or(0.0)
    }
}

#[derive(Default)]
pub struct TextTool {
    text: Option<Text>,
    style: Style,
    input_enabled: bool,
    im_context: Option<InputContext>,
    sender: Option<Sender<SketchBoardInput>>,
    drag_start_pos: Vec2D,
    dragged: Rc<RefCell<bool>>,
}

impl Tool for TextTool {
    fn get_tool_type(&self) -> super::Tools {
        Tools::Text
    }

    fn input_enabled(&self) -> bool {
        self.input_enabled
    }

    fn set_input_enabled(&mut self, value: bool) {
        self.input_enabled = value;
    }

    fn set_im_context(&mut self, context: Option<InputContext>) {
        self.im_context = context.clone();
        if let Some(text) = &mut self.text {
            text.im_context = context;
        }
    }

    fn get_drawable(&self) -> Option<&dyn Drawable> {
        match &self.text {
            Some(d) => Some(d),
            None => None,
        }
    }

    fn handle_style_event(&mut self, style: Style) -> ToolUpdateResult {
        self.style = style;
        if let Some(t) = &mut self.text {
            t.style = style;
            ToolUpdateResult::Redraw
        } else {
            ToolUpdateResult::Unmodified
        }
    }

    fn handle_text_event(&mut self, event: crate::sketch_board::TextEventMsg) -> ToolUpdateResult {
        if let Some(t) = &mut self.text {
            match event {
                TextEventMsg::Commit(text) => {
                    //delete selection
                    Self::handle_text_buffer_action(t, Action::Delete, ActionScope::None);
                    //update input text
                    t.preedit = None;
                    t.text_buffer.insert_at_cursor(&text);
                    ToolUpdateResult::Redraw
                }
                TextEventMsg::Preedit {
                    text,
                    cursor_chars,
                    spans,
                } => {
                    if text.is_empty() {
                        if t.preedit.take().is_some() {
                            ToolUpdateResult::Redraw
                        } else {
                            ToolUpdateResult::Unmodified
                        }
                    } else {
                        t.preedit = Some(Preedit {
                            text,
                            cursor_chars,
                            spans,
                        });
                        ToolUpdateResult::Redraw
                    }
                }
                TextEventMsg::PreeditEnd => {
                    if t.preedit.take().is_some() {
                        ToolUpdateResult::Redraw
                    } else {
                        ToolUpdateResult::Unmodified
                    }
                }
            }
        } else {
            ToolUpdateResult::Unmodified
        }
    }

    fn handle_key_event(&mut self, event: KeyEventMsg) -> ToolUpdateResult {
        let mut stop_propagation = ToolUpdateResult::StopPropagation;
        if let Some(t) = &mut self.text {
            match event.key {
                Key::Return => match event.modifier {
                    ModifierType::SHIFT_MASK => {
                        //delete selection
                        Self::handle_text_buffer_action(t, Action::Delete, ActionScope::None);
                        t.text_buffer.insert_at_cursor("\n");
                        return ToolUpdateResult::RedrawAndStopPropagation;
                    }
                    _ => {
                        t.preedit = None;
                        t.editing = false;
                        t.im_context = None;
                        t.text_buffer
                            .select_range(&t.text_buffer.start_iter(), &t.text_buffer.start_iter());
                        *t.draw_rect.borrow_mut() = false;
                        let result = t.clone_box();
                        self.text = None;
                        self.input_enabled = false;
                        return ToolUpdateResult::Commit(result);
                    }
                },
                Key::Escape => {
                    return self.handle_deactivated();
                }
                Key::BackSpace | Key::Delete => {
                    let ctrl_mask = match event.key {
                        Key::BackSpace => ActionScope::BackwardWord,
                        Key::Delete => ActionScope::ForwardWord,
                        _ => ActionScope::None,
                    };

                    let other_mask = match event.key {
                        Key::BackSpace => ActionScope::BackwardChar,
                        Key::Delete => ActionScope::ForwardChar,
                        _ => ActionScope::None,
                    };

                    if event.modifier == ModifierType::CONTROL_MASK {
                        return Self::handle_text_buffer_action(t, Action::Delete, ctrl_mask);
                    } else {
                        return Self::handle_text_buffer_action(t, Action::Delete, other_mask);
                    }
                }
                Key::Left | Key::Right | Key::Up | Key::Down => {
                    let ctrl_mask = match event.key {
                        Key::Left => ActionScope::BackwardWord,
                        Key::Right => ActionScope::ForwardWord,
                        Key::Up => ActionScope::BackwardLineAndWord,
                        Key::Down => ActionScope::ForwardLineAndWord,
                        _ => ActionScope::None,
                    };

                    let other_mask = match event.key {
                        Key::Left => ActionScope::BackwardChar,
                        Key::Right => ActionScope::ForwardChar,
                        Key::Up => ActionScope::BackwardLineAndWord,
                        Key::Down => ActionScope::ForwardLineAndWord,
                        _ => ActionScope::None,
                    };

                    let combine_mask = match event.key {
                        Key::Left => ActionScope::BackwardWord,
                        Key::Right => ActionScope::ForwardWord,
                        Key::Up => ActionScope::BackwardLineAndWord,
                        Key::Down => ActionScope::ForwardLineAndWord,
                        _ => ActionScope::None,
                    };

                    match event.modifier {
                        ModifierType::CONTROL_MASK => {
                            return Self::handle_text_buffer_action(
                                t,
                                Action::MoveCursor,
                                ctrl_mask,
                            );
                        }
                        ModifierType::SHIFT_MASK => {
                            return Self::handle_text_buffer_action(t, Action::Select, other_mask);
                        }
                        m if m.contains(ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK) => {
                            return Self::handle_text_buffer_action(
                                t,
                                Action::Select,
                                combine_mask,
                            );
                        }
                        _ => {
                            return Self::handle_text_buffer_action(
                                t,
                                Action::MoveCursor,
                                other_mask,
                            );
                        }
                    }
                }
                Key::Home | Key::End => {
                    let ctrl_mask = match event.key {
                        Key::Home => ActionScope::BufferStart,
                        Key::End => ActionScope::BufferEnd,
                        _ => ActionScope::None,
                    };

                    let other_mask = match event.key {
                        Key::Home => ActionScope::BackwardLine,
                        Key::End => ActionScope::ForwardLine,
                        _ => ActionScope::None,
                    };

                    match event.modifier {
                        ModifierType::CONTROL_MASK => {
                            return Self::handle_text_buffer_action(
                                t,
                                Action::MoveCursor,
                                ctrl_mask,
                            );
                        }
                        ModifierType::SHIFT_MASK => {
                            return Self::handle_text_buffer_action(t, Action::Select, other_mask);
                        }
                        _ => {
                            return Self::handle_text_buffer_action(
                                t,
                                Action::MoveCursor,
                                other_mask,
                            );
                        }
                    }
                }
                Key::a | Key::A => {
                    if event.modifier == ModifierType::CONTROL_MASK {
                        return Self::handle_text_buffer_action(
                            t,
                            Action::Select,
                            ActionScope::SelectAll,
                        );
                    }
                }
                Key::v | Key::V => {
                    let display = DisplayManager::get().default_display();
                    if display.is_none() {
                        eprintln!("Cannot open default display for clipboard.");
                        return ToolUpdateResult::StopPropagation;
                    }
                    let clipboard = display.unwrap().clipboard();
                    let buffer = t.text_buffer.clone();

                    Self::handle_text_buffer_action(t, Action::Delete, ActionScope::None);

                    let sender = self.sender.clone();

                    //async clipboard read
                    glib::MainContext::default().spawn_local(async move {
                        match clipboard.read_text_future().await {
                            Ok(Some(text)) => {
                                buffer.insert_at_cursor(&text);
                                if let Some(sender) = sender {
                                    sender.emit(SketchBoardInput::Refresh);
                                }
                            }
                            Ok(None) => {
                                eprintln!("Clipboard contains no text");
                            }
                            Err(err) => {
                                eprintln!("Clipboard read error: {}", err);
                            }
                        }
                    });
                }
                Key::c | Key::C => {
                    if event.modifier == ModifierType::CONTROL_MASK {
                        if let Some(text) = &self.text {
                            let buffer = text.text_buffer.clone();
                            if let Some((start, end)) = buffer.selection_bounds() {
                                let selected_text = buffer.text(&start, &end, false);

                                let display = DisplayManager::get().default_display();
                                if display.is_none() {
                                    eprintln!("Cannot open default display for clipboard.");
                                    return ToolUpdateResult::StopPropagation;
                                }

                                let clipboard = display.unwrap().clipboard();
                                clipboard.set_text(&selected_text);
                            }
                        }
                    }
                }
                Key::x | Key::X => {
                    if event.modifier == ModifierType::CONTROL_MASK {
                        if let Some(text) = &mut self.text {
                            let buffer = text.text_buffer.clone();
                            if let Some((start, end)) = buffer.selection_bounds() {
                                let selected_text = buffer.text(&start, &end, false);

                                let display = DisplayManager::get().default_display();
                                if display.is_none() {
                                    eprintln!("Cannot open default display for clipboard.");
                                    return ToolUpdateResult::StopPropagation;
                                }

                                let clipboard = display.unwrap().clipboard();
                                clipboard.set_text(&selected_text);

                                Self::handle_text_buffer_action(
                                    text,
                                    Action::Delete,
                                    ActionScope::None,
                                );
                                return ToolUpdateResult::RedrawAndStopPropagation;
                            }
                        }
                    }
                }
                Key::Insert => {
                    if event.modifier == ModifierType::SHIFT_MASK {
                        let display = DisplayManager::get().default_display();
                        if display.is_none() {
                            eprintln!("Cannot open default display for clipboard.");
                            return ToolUpdateResult::StopPropagation;
                        }
                        let selection_clipboard = display.unwrap().primary_clipboard();
                        let buffer = t.text_buffer.clone();

                        Self::handle_text_buffer_action(t, Action::Delete, ActionScope::None);

                        let sender = self.sender.clone();

                        glib::MainContext::default().spawn_local(async move {
                            match selection_clipboard.read_text_future().await {
                                Ok(Some(text)) => {
                                    buffer.insert_at_cursor(&text);
                                    if let Some(sender) = sender {
                                        sender.emit(SketchBoardInput::Refresh);
                                    }
                                }
                                Ok(None) => {
                                    eprintln!("selection_clipboard contains no text");
                                }
                                Err(err) => {
                                    eprintln!("selection_clipboard read error: {}", err);
                                }
                            }
                        });
                    }
                }
                _ => {
                    stop_propagation = ToolUpdateResult::Unmodified;
                }
            }
        };
        stop_propagation
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        match event.type_ {
            MouseEventType::Click => {
                match event.button {
                    MouseButton::Primary => {
                        let pos = event.pos;
                        if let Some(t) = &mut self.text {
                            let rect = t.rect.borrow();
                            if rect.contains_point(pos.x as i32, pos.y as i32) {
                                //calculate text cursor position
                                let mut index = 0;
                                let mut find_index = false;

                                let glyphs = t.glyphs.borrow();
                                for line in 0..glyphs.len() {
                                    let line_rect = glyphs.get(line).unwrap();

                                    for glyph in line_rect.iter() {
                                        if glyph.contains_point(pos.x as i32, pos.y as i32) {
                                            find_index = true;
                                            if pos.x > glyph.x() as f32 + glyph.width() as f32 / 2.0
                                            {
                                                index += 1;
                                            }
                                            break;
                                        }
                                        index += 1;
                                    }

                                    if find_index {
                                        break;
                                    }

                                    let first_ele = line_rect.iter().next().unwrap();
                                    if pos.y <= (first_ele.y() + first_ele.height()) as f32
                                        && line != glyphs.len() - 1
                                    {
                                        index -= 1;
                                        break;
                                    }
                                }

                                let buffer = &t.text_buffer;
                                let mut cursor_iter = buffer.iter_at_mark(&buffer.get_insert());
                                cursor_iter.set_offset(index);
                                t.text_buffer.place_cursor(&cursor_iter);

                                if event.n_pressed == 2 {
                                    let mut start_itr = cursor_iter;
                                    let mut end_itr = start_itr;
                                    start_itr.backward_word_start();
                                    end_itr.forward_word_end();
                                    t.text_buffer.select_range(&start_itr, &end_itr);
                                } else if event.n_pressed == 3 {
                                    let mut start_itr = cursor_iter;
                                    let mut end_itr = start_itr;
                                    while !start_itr.is_start() {
                                        start_itr.backward_line();
                                    }
                                    end_itr.forward_to_end();
                                    t.text_buffer.select_range(&start_itr, &end_itr);
                                }

                                return ToolUpdateResult::RedrawAndStopPropagation;
                            }
                        }

                        // create commit message if necessary
                        let return_value = match &mut self.text {
                            Some(l) => {
                                l.preedit = None;
                                l.editing = false;
                                l.im_context = None;
                                l.text_buffer.select_range(
                                    &l.text_buffer.start_iter(),
                                    &l.text_buffer.start_iter(),
                                );
                                *l.draw_rect.borrow_mut() = false;
                                ToolUpdateResult::Commit(l.clone_box())
                            }
                            None => ToolUpdateResult::Redraw,
                        };

                        // create a new Text
                        self.text = Some(Text::new(event.pos, self.style, self.im_context.clone()));

                        self.set_input_enabled(true);

                        return_value
                    }
                    _ => ToolUpdateResult::Unmodified,
                }
            }
            MouseEventType::Release => match event.button {
                MouseButton::Middle => {
                    if let Some(t) = &mut self.text {
                        let display = DisplayManager::get().default_display();
                        if display.is_none() {
                            eprintln!("Cannot open default display for clipboard.");
                            return ToolUpdateResult::StopPropagation;
                        }
                        let selection_clipboard = display.unwrap().primary_clipboard();
                        let buffer = t.text_buffer.clone();

                        Self::handle_text_buffer_action(t, Action::Delete, ActionScope::None);

                        let sender = self.sender.clone();
                        let dragged = self.dragged.clone();

                        glib::MainContext::default().spawn_local(async move {
                            match selection_clipboard.read_text_future().await {
                                Ok(Some(text)) => {
                                    if !*dragged.borrow() {
                                        buffer.insert_at_cursor(&text);
                                        if let Some(sender) = sender {
                                            sender.emit(SketchBoardInput::Refresh);
                                        }
                                    }
                                }
                                Ok(None) => {
                                    eprintln!("selection_clipboard contains no text");
                                }
                                Err(err) => {
                                    eprintln!("selection_clipboard read error: {}", err);
                                }
                            }
                        });
                    }

                    ToolUpdateResult::StopPropagation
                }
                _ => ToolUpdateResult::Unmodified,
            },
            MouseEventType::BeginDrag => {
                self.drag_start_pos = event.pos;
                if let Some(t) = &mut self.text {
                    let rect = t.rect.borrow();
                    if rect.contains_point(event.pos.x as i32, event.pos.y as i32) {
                        return ToolUpdateResult::StopPropagation;
                    }
                }
                ToolUpdateResult::Unmodified
            }
            MouseEventType::UpdateDrag => {
                self.dragged = Rc::new(RefCell::new(true));
                if event.button == MouseButton::Primary {
                    let global_pos = self.drag_start_pos + event.pos;
                    if let Some(t) = &mut self.text {
                        let rect = t.rect.borrow();
                        if rect.contains_point(global_pos.x as i32, global_pos.y as i32) {
                            //calculate text cursor position
                            let mut index = 0;
                            let mut find_index = false;

                            let glyphs = t.glyphs.borrow();
                            for line in glyphs.iter() {
                                for glyph in line.iter() {
                                    if glyph
                                        .contains_point(global_pos.x as i32, global_pos.y as i32)
                                    {
                                        find_index = true;
                                        if global_pos.x
                                            > glyph.x() as f32 + glyph.width() as f32 / 2.0
                                        {
                                            index += 1;
                                        }
                                        break;
                                    }
                                    index += 1;
                                }

                                let first_ele = line.iter().next().unwrap();
                                if find_index
                                    || global_pos.y <= (first_ele.y() + first_ele.height()) as f32
                                {
                                    break;
                                }
                            }

                            let buffer = &t.text_buffer;
                            let mut cursor_iter = buffer.iter_at_mark(&buffer.get_insert());
                            cursor_iter.set_offset(index);

                            let start_cursor_itr = buffer.iter_at_mark(&buffer.get_insert());
                            buffer.select_range(&start_cursor_itr, &cursor_iter);

                            return ToolUpdateResult::RedrawAndStopPropagation;
                        }
                    }
                    return ToolUpdateResult::StopPropagation;
                }
                ToolUpdateResult::Unmodified
            }
            MouseEventType::EndDrag => {
                self.dragged = Rc::new(RefCell::new(false));
                if let Some(t) = &mut self.text {
                    let rect = t.rect.borrow();
                    if rect.contains_point(event.pos.x as i32, event.pos.y as i32) {
                        return ToolUpdateResult::StopPropagation;
                    }
                }
                ToolUpdateResult::Unmodified
            }
            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn handle_deactivated(&mut self) -> ToolUpdateResult {
        self.input_enabled = false;
        if let Some(t) = &mut self.text {
            t.preedit = None;
            t.editing = false;
            t.im_context = None;
            let result = t.clone_box();
            self.text = None;
            ToolUpdateResult::Commit(result)
        } else {
            ToolUpdateResult::Unmodified
        }
    }

    fn active(&self) -> bool {
        self.text.is_some()
    }

    fn handle_undo(&mut self) -> ToolUpdateResult {
        if let Some(t) = &self.text {
            t.text_buffer.undo();
            ToolUpdateResult::Redraw
        } else {
            ToolUpdateResult::Unmodified
        }
    }

    fn handle_redo(&mut self) -> ToolUpdateResult {
        if let Some(t) = &self.text {
            t.text_buffer.redo();
            ToolUpdateResult::Redraw
        } else {
            ToolUpdateResult::Unmodified
        }
    }

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>) {
        self.sender = Some(sender);
    }
}
enum ActionScope {
    ForwardChar,
    BackwardChar,
    ForwardLine,
    BackwardLine,
    ForwardWord,
    BackwardWord,
    ForwardLineAndWord,
    BackwardLineAndWord,
    SelectAll,
    BufferStart,
    BufferEnd,
    None,
}

enum Action {
    Delete,
    MoveCursor,
    Select,
}

impl TextTool {
    fn handle_text_buffer_action(
        text: &mut Text,
        action: Action,
        action_scope: ActionScope,
    ) -> ToolUpdateResult {
        let text_buffer = &text.text_buffer;
        let mut start_cursor_itr = text_buffer.iter_at_mark(&text_buffer.get_insert());

        match action {
            Action::Delete => {
                let mut end_cursor_itr = start_cursor_itr;

                if let Some((start, end)) = text_buffer.selection_bounds() {
                    start_cursor_itr = start;
                    end_cursor_itr = end;
                } else {
                    match action_scope {
                        ActionScope::ForwardChar => end_cursor_itr.forward_char(),
                        ActionScope::BackwardChar => end_cursor_itr.backward_char(),
                        ActionScope::ForwardWord => end_cursor_itr.forward_word_end(),
                        ActionScope::BackwardWord => end_cursor_itr.backward_word_start(),
                        _ => false, // should normally be whether movement was possible, but it's not used anyway
                    };
                }

                if text_buffer.delete_interactive(&mut start_cursor_itr, &mut end_cursor_itr, true)
                {
                    ToolUpdateResult::RedrawAndStopPropagation
                } else {
                    ToolUpdateResult::StopPropagation
                }
            }
            Action::MoveCursor => {
                let mut cursor_itr = start_cursor_itr;
                let mut start_iter = None;
                let mut end_iter = None;

                let mut has_selection = false;
                if let Some((start, end)) = text_buffer.selection_bounds() {
                    start_iter = Some(start);
                    end_iter = Some(end);
                    has_selection = true;
                }

                match action_scope {
                    ActionScope::ForwardChar => {
                        if has_selection {
                            cursor_itr = end_iter.unwrap();
                            false
                        } else {
                            cursor_itr.forward_char()
                        }
                    }
                    ActionScope::BackwardChar => {
                        if has_selection {
                            cursor_itr = start_iter.unwrap();
                            false
                        } else {
                            cursor_itr.backward_char()
                        }
                    }
                    ActionScope::ForwardLine => cursor_itr.forward_to_line_end(),
                    ActionScope::ForwardWord => cursor_itr.forward_word_end(),
                    ActionScope::BackwardWord => cursor_itr.backward_word_start(),
                    ActionScope::BackwardLine => {
                        if cursor_itr.starts_line() {
                            cursor_itr.backward_line()
                        } else {
                            while !cursor_itr.starts_line() {
                                cursor_itr.backward_char();
                            }
                            false
                        }
                    }
                    ActionScope::BufferEnd => {
                        cursor_itr.forward_to_end();
                        false
                    }
                    ActionScope::BufferStart => {
                        while !cursor_itr.is_start() {
                            cursor_itr.backward_line();
                        }
                        false
                    }
                    ActionScope::ForwardLineAndWord => {
                        if has_selection {
                            cursor_itr = end_iter.unwrap();
                        } else {
                            let content = &text.text_buffer.text(
                                &text.text_buffer.start_iter(),
                                &text.text_buffer.end_iter(),
                                false,
                            );
                            let current_offset = cursor_itr.offset();

                            let mut next_line = 0;
                            let mut offset = 0;

                            let ranges = text.line_ranges.borrow();

                            for i in 0..ranges.len() {
                                let line = ranges.get(i).unwrap();

                                let start = content[..line.start].chars().count();
                                let end = content[..line.end].chars().count();

                                if current_offset >= start as i32 && current_offset <= end as i32 {
                                    offset = if i == ranges.len() - 1 {
                                        (end - start) as i32
                                    } else {
                                        let temp = current_offset - start as i32;
                                        let next_start = content
                                            [..ranges.get(i + 1).unwrap().start]
                                            .chars()
                                            .count();
                                        let next_end = content[..ranges.get(i + 1).unwrap().end]
                                            .chars()
                                            .count();

                                        let limit = (next_end - next_start) as i32;
                                        if temp > limit {
                                            limit
                                        } else {
                                            temp
                                        }
                                    };

                                    next_line = if i == ranges.len() - 1 {
                                        content[..ranges.get(i).unwrap().start].chars().count()
                                            as i32
                                    } else {
                                        content[..ranges.get(i + 1).unwrap().start].chars().count()
                                            as i32
                                    };
                                    break;
                                }
                            }

                            let move_offset = next_line + offset;

                            cursor_itr.set_offset(move_offset);
                        }

                        false
                    }
                    ActionScope::BackwardLineAndWord => {
                        if has_selection {
                            cursor_itr = start_iter.unwrap();
                        } else {
                            let content = &text.text_buffer.text(
                                &text.text_buffer.start_iter(),
                                &text.text_buffer.end_iter(),
                                false,
                            );
                            let current_offset = cursor_itr.offset();

                            let mut last_line = 0;
                            let mut offset = 0;

                            let ranges = text.line_ranges.borrow();

                            for i in 0..ranges.len() {
                                let line = ranges.get(i).unwrap();

                                let start = content[..line.start].chars().count();
                                let end = content[..line.end].chars().count();

                                if current_offset >= start as i32 && current_offset <= end as i32 {
                                    offset = if i == 0 {
                                        0
                                    } else {
                                        let temp = current_offset - start as i32;
                                        let last_start = content
                                            [..ranges.get(i - 1).unwrap().start]
                                            .chars()
                                            .count();
                                        let last_end = content[..ranges.get(i - 1).unwrap().end]
                                            .chars()
                                            .count();

                                        let limit = (last_end - last_start) as i32;
                                        if temp > limit {
                                            limit
                                        } else {
                                            temp
                                        }
                                    };

                                    last_line = if i == 0 {
                                        content[..ranges.get(i).unwrap().start].chars().count()
                                            as i32
                                    } else {
                                        content[..ranges.get(i - 1).unwrap().start].chars().count()
                                            as i32
                                    };
                                    break;
                                }
                            }

                            let move_offset = last_line + offset;

                            cursor_itr.set_offset(move_offset);
                        }
                        false
                    }
                    _ => false, // should normally be whether movement was possible, but it's not used anyway
                };

                text_buffer.select_range(&text_buffer.start_iter(), &text_buffer.start_iter());

                text_buffer.place_cursor(&cursor_itr);
                let new_cursor_itr = text_buffer.iter_at_mark(&text_buffer.get_insert());

                if new_cursor_itr != start_cursor_itr || has_selection {
                    ToolUpdateResult::RedrawAndStopPropagation
                } else {
                    ToolUpdateResult::StopPropagation
                }
            }
            Action::Select => {
                let mut start_cursor_itr_new = start_cursor_itr;
                let mut end_cursor_itr = start_cursor_itr;

                if let Some((start, end)) = text_buffer.selection_bounds() {
                    let insert = text_buffer.get_insert();
                    let insert_iter = text_buffer.iter_at_mark(&insert);

                    if insert_iter == start {
                        start_cursor_itr_new = start;
                        end_cursor_itr = end;
                    } else {
                        start_cursor_itr_new = end;
                        end_cursor_itr = start;
                    }
                }

                match action_scope {
                    ActionScope::ForwardChar => {
                        end_cursor_itr.forward_char();
                    }
                    ActionScope::BackwardChar => {
                        end_cursor_itr.backward_char();
                    }
                    ActionScope::ForwardLine => {
                        end_cursor_itr.forward_to_line_end();
                    }
                    ActionScope::BackwardLine => {
                        if end_cursor_itr.starts_line() {
                            end_cursor_itr.backward_line();
                        } else {
                            while !end_cursor_itr.starts_line() {
                                end_cursor_itr.backward_char();
                            }
                        }
                    }
                    ActionScope::ForwardLineAndWord => {
                        let content = &text.text_buffer.text(
                            &text.text_buffer.start_iter(),
                            &text.text_buffer.end_iter(),
                            false,
                        );
                        let current_offset = end_cursor_itr.offset();

                        let mut next_line = 0;
                        let mut offset = 0;

                        let ranges = text.line_ranges.borrow();

                        for i in 0..ranges.len() {
                            let line = ranges.get(i).unwrap();
                            let start = content[..line.start].chars().count();
                            let end = content[..line.end].chars().count();

                            if current_offset >= start as i32 && current_offset <= end as i32 {
                                offset = if i == ranges.len() - 1 {
                                    (end - start) as i32
                                } else {
                                    let temp = current_offset - start as i32;
                                    // current_offset - start as i32
                                    let next_start =
                                        content[..ranges.get(i + 1).unwrap().start].chars().count();
                                    let next_end =
                                        content[..ranges.get(i + 1).unwrap().end].chars().count();

                                    let limit = (next_end - next_start) as i32;
                                    if temp > limit {
                                        limit
                                    } else {
                                        temp
                                    }
                                };

                                next_line = if i == ranges.len() - 1 {
                                    content[..ranges.get(i).unwrap().start].chars().count() as i32
                                } else {
                                    content[..ranges.get(i + 1).unwrap().start].chars().count()
                                        as i32
                                };
                                break;
                            }
                        }

                        let move_offset = next_line + offset;

                        end_cursor_itr.set_offset(move_offset);
                    }
                    ActionScope::BackwardLineAndWord => {
                        let content = &text.text_buffer.text(
                            &text.text_buffer.start_iter(),
                            &text.text_buffer.end_iter(),
                            false,
                        );
                        let current_offset = end_cursor_itr.offset();

                        let mut last_line = 0;
                        let mut offset = 0;

                        let ranges = text.line_ranges.borrow();

                        for i in 0..ranges.len() {
                            let line = ranges.get(i).unwrap();
                            let start = content[..line.start].chars().count();
                            let end = content[..line.end].chars().count();

                            if current_offset >= start as i32 && current_offset <= end as i32 {
                                offset = if i == 0 {
                                    0
                                } else {
                                    let temp = current_offset - start as i32;
                                    let last_start =
                                        content[..ranges.get(i - 1).unwrap().start].chars().count();
                                    let last_end =
                                        content[..ranges.get(i - 1).unwrap().end].chars().count();

                                    let limit = (last_end - last_start) as i32;
                                    if temp > limit {
                                        limit
                                    } else {
                                        temp
                                    }
                                };

                                last_line = if i == 0 {
                                    content[..ranges.get(i).unwrap().start].chars().count() as i32
                                } else {
                                    content[..ranges.get(i - 1).unwrap().start].chars().count()
                                        as i32
                                };
                                break;
                            }
                        }

                        let move_offset = last_line + offset;

                        end_cursor_itr.set_offset(move_offset);
                    }
                    ActionScope::ForwardWord => {
                        end_cursor_itr.forward_word_end();
                    }
                    ActionScope::BackwardWord => {
                        end_cursor_itr.backward_word_start();
                    }
                    ActionScope::SelectAll => {
                        start_cursor_itr_new = text_buffer.start_iter();
                        end_cursor_itr = text_buffer.end_iter();
                    }
                    _ => {}
                }
                text_buffer.select_range(&start_cursor_itr_new, &end_cursor_itr);

                ToolUpdateResult::RedrawAndStopPropagation
            }
        }
    }
}
