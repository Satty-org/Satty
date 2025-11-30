use anyhow::Result;
use femtovg::{FontId, Paint, Path};
use relm4::gtk::prelude::IMContextExt;
use relm4::gtk::{
    gdk::{Key, ModifierType, Rectangle},
    TextBuffer,
};
use std::{borrow::Cow, ops::Range};

use relm4::gtk::prelude::*;

use crate::{
    femtovg_area,
    ime::preedit::{Preedit, UnderlineKind},
    math::Vec2D,
    sketch_board::{KeyEventMsg, MouseButton, MouseEventMsg, MouseEventType, TextEventMsg},
    style::Style,
};

use super::{Drawable, DrawableClone, InputContext, Tool, ToolUpdateResult, Tools};

#[derive(Clone, Debug)]
pub struct Text {
    pos: Vec2D,
    editing: bool,
    text_buffer: TextBuffer,
    style: Style,
    preedit: Option<Preedit>,
    im_context: Option<InputContext>,
    font_ids: Vec<FontId>,
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
            font_ids: femtovg_area::font_stack().to_vec(),
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

        if self.font_ids.is_empty() {
            base_paint.set_font(&[font]);
        } else {
            base_paint.set_font(&self.font_ids);
        }

        let transform = canvas.transform();
        let canva_scale = transform.average_scale();
        let canvas_offset_x = transform[4];
        let canvas_width = canvas.width() as f32;

        let width = canvas_width / canva_scale - self.pos.x - canvas_offset_x;

        let lines = canvas.break_text_vec(width, text, &base_paint)?;

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

        let mut draw_baseline = self.pos.y;
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

        let line_start = line.range.start;
        let line_end = line.range.end;
        let overlap_start = range.start.max(line_start).min(line_end);
        let overlap_end = range.end.max(line_start).min(line_end);
        if overlap_start >= overlap_end {
            return Vec::new();
        }

        let line_text = &context.text[line.range.clone()];
        let start_offset = overlap_start.saturating_sub(line_start);
        let end_offset = overlap_end.saturating_sub(line_start);

        let prefix = &line_text[..start_offset];
        let selected = &line_text[start_offset..end_offset];

        let start_x = self.pos.x + Self::text_width(canvas, context.paint, prefix);
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
    ) {
        let (cursor_x, cursor_top) = self.caret_top_left(canvas, context, cursor_byte_pos, cursor);
        let caret_height = cursor.height;

        let mut caret_paint: Paint = self.style.into();
        caret_paint.set_font(&[font]);
        let extra_height = caret_height * 0.05;
        let mut path = Path::new();
        path.move_to(cursor_x, cursor_top - extra_height);
        path.line_to(cursor_x, cursor_top + caret_height + extra_height * 2.0);
        canvas.fill_path(&path, &caret_paint);

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
        if let Some(t) = &mut self.text {
            if event.key == Key::Return {
                if event.modifier == ModifierType::SHIFT_MASK {
                    t.text_buffer.insert_at_cursor("\n");
                    return ToolUpdateResult::Redraw;
                } else {
                    t.preedit = None;
                    t.editing = false;
                    t.im_context = None;
                    let result = t.clone_box();
                    self.text = None;
                    self.input_enabled = false;
                    return ToolUpdateResult::Commit(result);
                }
            } else if event.key == Key::Escape {
                return self.handle_deactivated();
            } else if event.key == Key::BackSpace {
                if event.modifier == ModifierType::CONTROL_MASK {
                    return Self::handle_text_buffer_action(
                        &mut t.text_buffer,
                        Action::Delete,
                        ActionScope::BackwardWord,
                    );
                } else {
                    return Self::handle_text_buffer_action(
                        &mut t.text_buffer,
                        Action::Delete,
                        ActionScope::BackwardChar,
                    );
                }
            } else if event.key == Key::Delete {
                if event.modifier == ModifierType::CONTROL_MASK {
                    return Self::handle_text_buffer_action(
                        &mut t.text_buffer,
                        Action::Delete,
                        ActionScope::ForwardWord,
                    );
                } else {
                    return Self::handle_text_buffer_action(
                        &mut t.text_buffer,
                        Action::Delete,
                        ActionScope::ForwardChar,
                    );
                }
            } else if event.key == Key::Left {
                if event.modifier == ModifierType::CONTROL_MASK {
                    return Self::handle_text_buffer_action(
                        &mut t.text_buffer,
                        Action::MoveCursor,
                        ActionScope::BackwardWord,
                    );
                } else {
                    return Self::handle_text_buffer_action(
                        &mut t.text_buffer,
                        Action::MoveCursor,
                        ActionScope::BackwardChar,
                    );
                }
            } else if event.key == Key::Right {
                if event.modifier == ModifierType::CONTROL_MASK {
                    return Self::handle_text_buffer_action(
                        &mut t.text_buffer,
                        Action::MoveCursor,
                        ActionScope::ForwardWord,
                    );
                } else {
                    return Self::handle_text_buffer_action(
                        &mut t.text_buffer,
                        Action::MoveCursor,
                        ActionScope::ForwardChar,
                    );
                }
            } else if event.key == Key::Home {
                if event.modifier == ModifierType::CONTROL_MASK {
                    return Self::handle_text_buffer_action(
                        &mut t.text_buffer,
                        Action::MoveCursor,
                        ActionScope::BufferStart,
                    );
                } else {
                    return Self::handle_text_buffer_action(
                        &mut t.text_buffer,
                        Action::MoveCursor,
                        ActionScope::BackwardLine,
                    );
                }
            } else if event.key == Key::End {
                if event.modifier == ModifierType::CONTROL_MASK {
                    return Self::handle_text_buffer_action(
                        &mut t.text_buffer,
                        Action::MoveCursor,
                        ActionScope::BufferEnd,
                    );
                } else {
                    return Self::handle_text_buffer_action(
                        &mut t.text_buffer,
                        Action::MoveCursor,
                        ActionScope::ForwardLine,
                    );
                }
            }
        };
        ToolUpdateResult::Unmodified
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        match event.type_ {
            MouseEventType::Click => {
                if event.button == MouseButton::Primary {
                    // create commit message if necessary
                    let return_value = match &mut self.text {
                        Some(l) => {
                            l.preedit = None;
                            l.editing = false;
                            l.im_context = None;
                            ToolUpdateResult::Commit(l.clone_box())
                        }
                        None => ToolUpdateResult::Redraw,
                    };

                    // create a new Text
                    self.text = Some(Text::new(event.pos, self.style, self.im_context.clone()));

                    self.set_input_enabled(true);

                    return_value
                } else {
                    self.set_input_enabled(false);
                    ToolUpdateResult::Unmodified
                }
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
}
enum ActionScope {
    ForwardChar,
    BackwardChar,
    ForwardLine,
    BackwardLine,
    ForwardWord,
    BackwardWord,
    BufferStart,
    BufferEnd,
}

enum Action {
    Delete,
    MoveCursor,
}

impl TextTool {
    fn handle_text_buffer_action(
        text_buffer: &mut TextBuffer,
        action: Action,
        action_scope: ActionScope,
    ) -> ToolUpdateResult {
        let mut start_cursor_itr = text_buffer.iter_at_mark(&text_buffer.get_insert());

        match action {
            Action::Delete => {
                let mut end_cursor_itr = start_cursor_itr;

                match action_scope {
                    ActionScope::ForwardChar => end_cursor_itr.forward_char(),
                    ActionScope::BackwardChar => end_cursor_itr.backward_char(),
                    ActionScope::ForwardWord => end_cursor_itr.forward_word_end(),
                    ActionScope::BackwardWord => end_cursor_itr.backward_word_start(),
                    _ => false, // should normally be whether movement was possible, but it's not used anyway
                };

                if text_buffer.delete_interactive(&mut start_cursor_itr, &mut end_cursor_itr, true)
                {
                    ToolUpdateResult::Redraw
                } else {
                    ToolUpdateResult::Unmodified
                }
            }
            Action::MoveCursor => {
                let mut cursor_itr = start_cursor_itr;
                match action_scope {
                    ActionScope::ForwardChar => cursor_itr.forward_char(),
                    ActionScope::BackwardChar => cursor_itr.backward_char(),
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
                };

                text_buffer.place_cursor(&cursor_itr);
                let new_cursor_itr = text_buffer.iter_at_mark(&text_buffer.get_insert());

                if new_cursor_itr != start_cursor_itr {
                    ToolUpdateResult::Redraw
                } else {
                    ToolUpdateResult::Unmodified
                }
            }
        }
    }
}
