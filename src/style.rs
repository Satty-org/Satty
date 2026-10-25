use std::borrow::Cow;

use femtovg::{LineCap, Paint};
use hex_color::HexColor;
use relm4::gtk::gdk::RGBA;
use relm4::gtk::gdk_pixbuf::{
    glib::{Variant, VariantTy},
    prelude::{StaticVariantType, ToVariant},
};
use relm4::gtk::glib::variant::FromVariant;

use crate::configuration::APP_CONFIG;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    pub color: Color,
    pub size: Size,
    pub fill: bool,
    pub round_caps: bool,
    /// radius of rounded corners in pixels
    pub corner_radius: f32,
    /// opacity in percent, multiplied with the alpha of the color
    pub opacity: f32,
    /// sizes for small, medium and large, see [`SizePreset`]
    pub presets: [Size; 3],
}

/// A part of the style that a tool can have itself or share with the others
/// through the general style. The size includes the size presets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StyleField {
    Color,
    Size,
    Fill,
    RoundCaps,
    CornerRadius,
    Opacity,
}

impl StyleField {
    pub fn name(self) -> &'static str {
        match self {
            StyleField::Color => "color",
            StyleField::Size => "size",
            StyleField::Fill => "fill",
            StyleField::RoundCaps => "round caps",
            StyleField::CornerRadius => "corner radius",
            StyleField::Opacity => "opacity",
        }
    }
}

/// The fields a tool has itself, the others come from the general style.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StyleFields(u8);

impl StyleFields {
    pub fn contains(self, field: StyleField) -> bool {
        self.0 & (1 << field as u8) != 0
    }

    pub fn set(&mut self, field: StyleField, on: bool) {
        if on {
            self.0 |= 1 << field as u8;
        } else {
            self.0 &= !(1 << field as u8);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

/// Size of an annotation, its meaning depends on the tool, e.g. line width,
/// font size, block size or opacity in percent.
#[derive(Debug, PartialEq, PartialOrd, Clone, Copy)]
pub struct Size(f32);

#[derive(Debug, PartialEq, Eq, Clone, Copy, Default)]
pub enum SizePreset {
    Small = 0,
    #[default]
    Medium = 1,
    Large = 2,
}

impl Default for Style {
    fn default() -> Self {
        APP_CONFIG.read().style()
    }
}

impl Default for Color {
    fn default() -> Self {
        APP_CONFIG
            .read()
            .color_palette()
            .palette()
            .first()
            .copied()
            .unwrap_or(Color::red())
    }
}

impl StaticVariantType for Color {
    fn static_variant_type() -> Cow<'static, VariantTy> {
        Cow::Borrowed(VariantTy::TUPLE)
    }
}
impl ToVariant for Color {
    fn to_variant(&self) -> Variant {
        (self.r, self.g, self.b, self.a).to_variant()
    }
}

impl FromVariant for Color {
    fn from_variant(variant: &Variant) -> Option<Self> {
        <(u8, u8, u8, u8)>::from_variant(variant).map(|(r, g, b, a)| Self { r, g, b, a })
    }
}

impl Color {
    pub fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub fn from_gdk(rgba: RGBA) -> Self {
        Self::new(
            (rgba.red() * 255.0) as u8,
            (rgba.green() * 255.0) as u8,
            (rgba.blue() * 255.0) as u8,
            (rgba.alpha() * 255.0) as u8,
        )
    }

    pub fn orange() -> Self {
        Self::new(240, 147, 43, 255)
    }
    pub fn red() -> Self {
        Self::new(235, 77, 75, 255)
    }
    pub fn green() -> Self {
        Self::new(106, 176, 76, 255)
    }
    pub fn blue() -> Self {
        Self::new(34, 166, 179, 255)
    }
    pub fn cove() -> Self {
        Self::new(19, 15, 64, 255)
    }

    pub fn pink() -> Self {
        Self::new(200, 37, 184, 255)
    }

    /// Returns the color with its RGB channels inverted, preserving alpha.
    pub fn inverted(self) -> Self {
        Self::new(255 - self.r, 255 - self.g, 255 - self.b, self.a)
    }

    /// Returns the color with its alpha channel replaced.
    pub fn with_alpha(self, a: u8) -> Self {
        Self::new(self.r, self.g, self.b, a)
    }

    /// Returns black or white, whichever contrasts better with this color,
    /// preserving alpha. Uses WCAG relative luminance (channels gamma-decoded to
    /// linear light, then weighted); black and white are equal at L = 0.179.
    pub fn contrast(self) -> Self {
        fn linearize(channel: u8) -> f32 {
            let c = channel as f32 / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }

        let luminance =
            0.2126 * linearize(self.r) + 0.7152 * linearize(self.g) + 0.0722 * linearize(self.b);
        if luminance > 0.179 {
            Self::new(0, 0, 0, self.a)
        } else {
            Self::new(255, 255, 255, self.a)
        }
    }

    pub fn to_rgba_f64(self) -> (f64, f64, f64, f64) {
        (
            (self.r as f64) / 255.0,
            (self.g as f64) / 255.0,
            (self.b as f64) / 255.0,
            (self.a as f64) / 255.0,
        )
    }
    pub fn to_rgba_u32(self) -> u32 {
        ((self.r as u32) << 24) | ((self.g as u32) << 16) | ((self.b as u32) << 8) | (self.a as u32)
    }
}

impl From<RGBA> for Color {
    fn from(value: RGBA) -> Self {
        Self::new(
            (value.red() * 255.0) as u8,
            (value.green() * 255.0) as u8,
            (value.blue() * 255.0) as u8,
            (value.alpha() * 255.0) as u8,
        )
    }
}

impl From<Color> for RGBA {
    fn from(color: Color) -> Self {
        Self::new(
            color.r as f32 / 255.0,
            color.g as f32 / 255.0,
            color.b as f32 / 255.0,
            color.a as f32 / 255.0,
        )
    }
}

impl From<Color> for femtovg::Color {
    fn from(value: Color) -> Self {
        femtovg::Color {
            r: value.r as f32 / 255.0,
            g: value.g as f32 / 255.0,
            b: value.b as f32 / 255.0,
            a: value.a as f32 / 255.0,
        }
    }
}

impl From<HexColor> for Color {
    fn from(value: HexColor) -> Self {
        Self::new(value.r, value.g, value.b, value.a)
    }
}

impl From<Style> for Paint {
    fn from(value: Style) -> Self {
        Paint::default()
            .with_anti_alias(true)
            .with_font_size(value.size.at_least(1.0))
            .with_color(value.paint_color())
            .with_line_cap(if value.round_caps {
                LineCap::Round
            } else {
                LineCap::Butt
            })
            .with_line_width(value.size.at_least(0.5))
    }
}

impl Size {
    pub const MAX: f32 = 999.0;

    /// Clamps to 0..=MAX and rounds to one decimal place.
    pub fn new(value: f32) -> Self {
        Self((value.clamp(0.0, Self::MAX) * 10.0).round() / 10.0)
    }

    pub fn value(self) -> f32 {
        self.0
    }

    /// The size, but at least `min`, for tools that need a lower limit.
    pub fn at_least(self, min: f32) -> f32 {
        self.0.max(min)
    }
}

impl SizePreset {
    pub const ALL: [SizePreset; 3] = [SizePreset::Small, SizePreset::Medium, SizePreset::Large];

    /// The preset to switch to with cycle-size.
    pub fn cycled(self) -> Self {
        match self {
            SizePreset::Small => SizePreset::Large,
            SizePreset::Medium => SizePreset::Small,
            SizePreset::Large => SizePreset::Medium,
        }
    }
}

impl Style {
    /// The corner radius, or 0 without round caps.
    pub fn effective_corner_radius(&self) -> f32 {
        if self.round_caps {
            self.corner_radius
        } else {
            0.0
        }
    }

    /// Copies `field` from `other`.
    pub fn copy_field(&mut self, field: StyleField, other: &Style) {
        match field {
            StyleField::Color => self.color = other.color,
            StyleField::Size => {
                self.size = other.size;
                self.presets = other.presets;
            }
            StyleField::Fill => self.fill = other.fill,
            StyleField::RoundCaps => self.round_caps = other.round_caps,
            StyleField::CornerRadius => self.corner_radius = other.corner_radius,
            StyleField::Opacity => self.opacity = other.opacity,
        }
    }

    /// This style for the fields in `own`, `general` for the others.
    pub fn merged_with(&self, general: &Style, own: StyleFields) -> Style {
        let mut style = *general;
        for field in [
            StyleField::Color,
            StyleField::Size,
            StyleField::Fill,
            StyleField::RoundCaps,
            StyleField::CornerRadius,
            StyleField::Opacity,
        ] {
            if own.contains(field) {
                style.copy_field(field, self);
            }
        }
        style
    }

    /// The color with the opacity applied.
    pub fn paint_color(&self) -> femtovg::Color {
        let mut color: femtovg::Color = self.color.into();
        color.a *= self.opacity.clamp(0.0, 100.0) / 100.0;
        color
    }

    pub fn preset(&self, preset: SizePreset) -> Size {
        self.presets[preset as usize]
    }

    /// The preset whose size equals the current size, if any.
    pub fn matching_preset(&self) -> Option<SizePreset> {
        SizePreset::ALL
            .into_iter()
            .find(|&p| self.preset(p) == self.size)
    }

    pub fn line_width(&self) -> f32 {
        self.size.at_least(0.5)
    }

    pub fn font_size(&self) -> f32 {
        self.size.at_least(4.0)
    }

    /// Opacity from the size taken as percent, at least `min_percent`.
    pub fn size_as_alpha(&self, min_percent: f32) -> f32 {
        self.size.at_least(min_percent).min(100.0) / 100.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(color: Color, size: f32) -> Style {
        Style {
            color,
            size: Size::new(size),
            fill: false,
            round_caps: true,
            corner_radius: 12.0,
            opacity: 100.0,
            presets: [Size::new(size); 3],
        }
    }

    #[test]
    fn merged_with_takes_own_fields_from_tool() {
        let general = style(Color::red(), 5.0);
        let tool = style(Color::blue(), 54.0);
        let mut own = StyleFields::default();
        assert_eq!(tool.merged_with(&general, own), general);

        own.set(StyleField::Size, true);
        let merged = tool.merged_with(&general, own);
        assert_eq!(merged.color, Color::red());
        assert_eq!(merged.size, Size::new(54.0));
        assert_eq!(merged.presets, [Size::new(54.0); 3]);

        own.set(StyleField::Size, false);
        assert!(!own.contains(StyleField::Size));
    }
}
