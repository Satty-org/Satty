use relm4::gtk::glib::translate::FromGlib;
use relm4::gtk::pango::{AttrColor, AttrInt, AttrList, AttrType, Color as PangoColor, Underline};

use crate::style::Color;

use super::preedit::{PreeditSpan, UnderlineKind};

#[allow(clippy::cast_possible_truncation)]
fn to_style_color(color: PangoColor, alpha: Option<u16>) -> Color {
    let to_u8 = |value: u16| -> u8 { (value / 257) as u8 };
    let alpha = alpha.unwrap_or(u16::MAX);
    Color::new(
        to_u8(color.red()),
        to_u8(color.green()),
        to_u8(color.blue()),
        to_u8(alpha),
    )
}

fn clamp_index(index: i32, len: usize) -> usize {
    if index < 0 {
        0
    } else {
        (index as usize).min(len)
    }
}

fn underline_from_pango(value: i32) -> UnderlineKind {
    match unsafe { Underline::from_glib(value) } {
        Underline::None => UnderlineKind::None,
        Underline::Single => UnderlineKind::Single,
        Underline::Double | Underline::DoubleLine => UnderlineKind::Double,
        Underline::Low => UnderlineKind::Low,
        Underline::Error => UnderlineKind::Error,
        _ => UnderlineKind::Single,
    }
}

/// Convert a Pango attribute list into neutral preedit spans understood by our renderer.
pub fn spans_from_pango_attrs(text: &str, attrs: Option<AttrList>) -> Vec<PreeditSpan> {
    let mut spans = Vec::new();
    let text_len = text.len();

    let Some(attr_list) = attrs else {
        if !text.is_empty() {
            spans.push(PreeditSpan {
                range: 0..text_len,
                selected: false,
                underline: UnderlineKind::Single,
                ..Default::default()
            });
        }
        return spans;
    };

    let mut iterator = attr_list.iterator();
    loop {
        let (start, end) = iterator.range();
        let span_start = clamp_index(start, text_len);
        let span_end = clamp_index(end, text_len);
        if span_start < span_end {
            let mut span = PreeditSpan {
                range: span_start..span_end,
                ..Default::default()
            };

            let mut fg_color: Option<PangoColor> = None;
            let mut bg_color: Option<PangoColor> = None;
            let mut underline_color: Option<PangoColor> = None;
            let mut underline_kind = UnderlineKind::None;
            let mut fg_alpha: Option<u16> = None;
            let mut bg_alpha: Option<u16> = None;

            for attr in iterator.attrs() {
                match attr.attr_class().type_() {
                    AttrType::Foreground => {
                        if let Some(color_attr) = attr.downcast_ref::<AttrColor>() {
                            fg_color = Some(color_attr.color());
                        }
                    }
                    AttrType::Background => {
                        if let Some(color_attr) = attr.downcast_ref::<AttrColor>() {
                            bg_color = Some(color_attr.color());
                        }
                    }
                    AttrType::Underline => {
                        if let Some(value_attr) = attr.downcast_ref::<AttrInt>() {
                            underline_kind = underline_from_pango(value_attr.value());
                        }
                    }
                    AttrType::UnderlineColor => {
                        if let Some(color_attr) = attr.downcast_ref::<AttrColor>() {
                            underline_color = Some(color_attr.color());
                        }
                    }
                    AttrType::ForegroundAlpha => {
                        if let Some(alpha_attr) = attr.downcast_ref::<AttrInt>() {
                            fg_alpha = Some(alpha_attr.value().clamp(0, u16::MAX as i32) as u16);
                        }
                    }
                    AttrType::BackgroundAlpha => {
                        if let Some(alpha_attr) = attr.downcast_ref::<AttrInt>() {
                            bg_alpha = Some(alpha_attr.value().clamp(0, u16::MAX as i32) as u16);
                        }
                    }
                    _ => {}
                }
            }

            if let Some(color) = fg_color {
                span.foreground = Some(to_style_color(color, fg_alpha));
            }
            if let Some(color) = bg_color {
                span.background = Some(to_style_color(color, bg_alpha));
            }
            if let Some(color) = underline_color {
                span.underline_color = Some(to_style_color(color, None));
            }

            span.underline = underline_kind;
            span.selected = span.background.is_some()
                || matches!(span.underline, UnderlineKind::Double | UnderlineKind::Error);

            spans.push(span);
        }

        if !iterator.next_style_change() {
            break;
        }
    }

    if spans.is_empty() && !text.is_empty() {
        spans.push(PreeditSpan {
            range: 0..text_len,
            selected: false,
            underline: UnderlineKind::Single,
            ..Default::default()
        });
    }

    spans
}
