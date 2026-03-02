use std::borrow::Cow;

use femtovg::Paint;
use hex_color::HexColor;
use relm4::gtk::gdk::RGBA;
use relm4::gtk::gdk_pixbuf::{
    glib::{Variant, VariantTy},
    prelude::{StaticVariantType, ToVariant},
};
use relm4::gtk::glib::variant::FromVariant;

use crate::configuration::APP_CONFIG;

#[derive(Clone, Copy, Debug)]
pub struct Style {
    pub color: Color,
    pub size: Size,
    pub fill: bool,
    pub annotation_size_factor: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Default)]
pub enum Size {
    Small = 0,
    #[default]
    Medium = 1,
    Large = 2,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            color: Color::default(),
            size: Size::default(),
            fill: APP_CONFIG.read().default_fill_shapes(),
            annotation_size_factor: APP_CONFIG.read().annotation_size_factor(),
        }
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
            .with_font_size(value.size.to_text_size(value.annotation_size_factor) as f32)
            .with_color(value.color.into())
            .with_line_width(value.size.to_line_width(value.annotation_size_factor))
    }
}

impl StaticVariantType for Size {
    fn static_variant_type() -> Cow<'static, VariantTy> {
        Cow::Borrowed(VariantTy::UINT32)
    }
}

impl ToVariant for Size {
    fn to_variant(&self) -> Variant {
        Variant::from(*self as u32)
    }
}

impl FromVariant for Size {
    fn from_variant(variant: &Variant) -> Option<Self> {
        variant.get::<u32>().and_then(|v| match v {
            0 => Some(Size::Small),
            1 => Some(Size::Medium),
            2 => Some(Size::Large),
            _ => None,
        })
    }
}

impl Size {
    pub fn to_text_size(self, size_factor: f32) -> i32 {
        match self {
            Size::Small => (36.0 * size_factor) as i32,
            Size::Medium => (54.0 * size_factor) as i32,
            Size::Large => (96.0 * size_factor) as i32,
        }
    }

    pub fn to_line_width(self, size_factor: f32) -> f32 {
        match self {
            Size::Small => 3.0 * size_factor,
            Size::Medium => 5.0 * size_factor,
            Size::Large => 7.0 * size_factor,
        }
    }

    pub fn to_arrow_tail_width(self, size_factor: f32) -> f32 {
        match self {
            Size::Small => 3.0 * size_factor,
            Size::Medium => 10.0 * size_factor,
            Size::Large => 25.0 * size_factor,
        }
    }

    pub fn to_arrow_head_length(self, size_factor: f32) -> f32 {
        match self {
            Size::Small => 15.0 * size_factor,
            Size::Medium => 30.0 * size_factor,
            Size::Large => 60.0 * size_factor,
        }
    }

    pub fn to_blur_factor(self, size_factor: f32) -> f32 {
        match self {
            Size::Small => 10.0 * size_factor,
            Size::Medium => 20.0 * size_factor,
            Size::Large => 30.0 * size_factor,
        }
    }

    pub fn to_highlight_width(self, size_factor: f32) -> f32 {
        match self {
            Size::Small => 15.0 * size_factor,
            Size::Medium => 30.0 * size_factor,
            Size::Large => 45.0 * size_factor,
        }
    }
}
