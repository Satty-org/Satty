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
    XSmall = 0,
    Small = 1,
    #[default]
    Medium = 2,
    Large = 3,
    XLarge = 4,
    XXLarge = 5,
}

impl Size {
    pub fn display_name(self) -> &'static str {
        match self {
            Size::XSmall => "X-Small",
            Size::Small => "Small",
            Size::Medium => "Medium",
            Size::Large => "Large",
            Size::XLarge => "X-Large",
            Size::XXLarge => "XX-Large",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Size::XSmall => "XS",
            Size::Small => "S",
            Size::Medium => "M",
            Size::Large => "L",
            Size::XLarge => "XL",
            Size::XXLarge => "XXL",
        }
    }
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

    // curated palette additions — black/white anchor each end of the
    // 10-slot row, plus yellow/purple/royal-blue/teal fill the gaps that
    // the older 6-color helper set was missing.
    pub fn black() -> Self {
        Self::new(0, 0, 0, 255)
    }
    pub fn white() -> Self {
        Self::new(255, 255, 255, 255)
    }
    pub fn yellow() -> Self {
        Self::new(240, 211, 47, 255)
    }
    pub fn teal() -> Self {
        Self::new(34, 166, 179, 255)
    }
    pub fn royal_blue() -> Self {
        Self::new(64, 128, 224, 255)
    }
    pub fn purple() -> Self {
        Self::new(96, 72, 205, 255)
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
            0 => Some(Size::XSmall),
            1 => Some(Size::Small),
            2 => Some(Size::Medium),
            3 => Some(Size::Large),
            4 => Some(Size::XLarge),
            5 => Some(Size::XXLarge),
            _ => None,
        })
    }
}

impl Size {
    pub fn to_text_size(self, size_factor: f32) -> i32 {
        match self {
            Size::XSmall => (24.0 * size_factor) as i32,
            Size::Small => (36.0 * size_factor) as i32,
            Size::Medium => (54.0 * size_factor) as i32,
            Size::Large => (84.0 * size_factor) as i32,
            Size::XLarge => (120.0 * size_factor) as i32,
            Size::XXLarge => (168.0 * size_factor) as i32,
        }
    }

    pub fn to_line_width(self, size_factor: f32) -> f32 {
        match self {
            Size::XSmall => 1.5 * size_factor,
            Size::Small => 3.0 * size_factor,
            Size::Medium => 5.0 * size_factor,
            Size::Large => 7.0 * size_factor,
            Size::XLarge => 11.0 * size_factor,
            Size::XXLarge => 16.0 * size_factor,
        }
    }

    /// Visible body width where it meets the back of the arrowhead, in
    /// logical pixels at size_factor=1.0.
    pub fn to_arrow_tail_width(self, size_factor: f32) -> f32 {
        match self {
            Size::XSmall => 5.5 * size_factor,
            Size::Small => 7.0 * size_factor,
            Size::Medium => 11.5 * size_factor,
            Size::Large => 14.5 * size_factor,
            Size::XLarge => 19.5 * size_factor,
            Size::XXLarge => 29.5 * size_factor,
        }
    }

    /// Visible body width at the head intersection for the Fancy arrow style.
    /// Matches a reference exactly
    /// (3-run-zone middle-run widths halved from 2× DPR).
    pub fn to_arrow_fancy_tail_width(self, size_factor: f32) -> f32 {
        match self {
            Size::XSmall => 8.0 * size_factor,
            Size::Small => 10.0 * size_factor,
            Size::Medium => 16.0 * size_factor,
            Size::Large => 20.0 * size_factor,
            Size::XLarge => 27.0 * size_factor,
            Size::XXLarge => 41.0 * size_factor,
        }
    }

    /// Flat-edge thickness of the Fancy arrow's tail back, in 1× logical px.
    /// the standard fancy arrows do not taper to a perfect needle — there is
    /// a thin finite back edge that scales with size. Calibrated against the
    /// reference using a screen pixel ruler; the smaller sizes were a touch
    /// too thin in the first pass (target ruler readings: 2.5, 2.5, 3.5, 4,
    /// 4.5, 6.5 at 2× DPR), so XS/S/M were nudged up to match.
    pub fn to_arrow_fancy_tail_back_width(self, size_factor: f32) -> f32 {
        match self {
            Size::XSmall => 1.5 * size_factor,
            Size::Small => 1.5 * size_factor,
            Size::Medium => 1.75 * size_factor,
            Size::Large => 2.0 * size_factor,
            Size::XLarge => 2.5 * size_factor,
            Size::XXLarge => 3.5 * size_factor,
        }
    }

    /// Diameter of the arrow tail's rounded back cap (visible). Sized so
    /// the body taper rate scales with the arrow size — larger sizes get
    /// a steeper taper, smaller sizes stay nearly parallel.
    pub fn to_arrow_tail_back_width(self, size_factor: f32) -> f32 {
        match self {
            Size::XSmall => 1.5 * size_factor,
            Size::Small => 2.5 * size_factor,
            Size::Medium => 3.5 * size_factor,
            Size::Large => 4.5 * size_factor,
            Size::XLarge => 6.0 * size_factor,
            Size::XXLarge => 8.5 * size_factor,
        }
    }

    /// Length of the arrowhead along the shaft, from tip to back of the head
    /// triangle. Larger sizes (L/XL/XXL) shave 1–2 px off a strict
    /// geometric scaling to compensate for the rounded line-join's
    /// natural backward bulge at the head's outer corner.
    pub fn to_arrow_head_length(self, size_factor: f32) -> f32 {
        match self {
            Size::XSmall => 15.0 * size_factor,
            Size::Small => 20.0 * size_factor,
            Size::Medium => 31.5 * size_factor,
            Size::Large => 38.0 * size_factor,
            Size::XLarge => 52.0 * size_factor,
            Size::XXLarge => 78.5 * size_factor,
        }
    }

    /// Full perpendicular head height (path-space, before rounded outline
    /// stroke widens it). Reference visible heights (1× DPR): 15.5, 21, 32.5,
    /// 40.5, 55, 83.5; subtract `to_arrow_tail_back_width` (the rounded stroke
    /// adds that to the visible height). REF heads are slightly *squatter*
    /// than the body length suggests — full apex ≈ 49°, not 53° — so we
    /// store these per-size like Fancy does, not derive from a single angle.
    pub fn to_arrow_head_full_height(self, size_factor: f32) -> f32 {
        match self {
            Size::XSmall => 14.0 * size_factor,
            Size::Small => 18.5 * size_factor,
            Size::Medium => 29.0 * size_factor,
            Size::Large => 36.0 * size_factor,
            Size::XLarge => 49.0 * size_factor,
            Size::XXLarge => 75.0 * size_factor,
        }
    }

    /// Length of each side of the open V-tip used by Curved/Double arrows
    /// (path-space, before stroke cap widens it). The visible side length
    /// is this value plus `to_line_width / 2` — the round cap extends the
    /// corner outward by stroke_radius along the V direction. XSmall and
    /// Small share the same value: below that there's a minimum head size
    /// the V doesn't shrink past regardless of line width.
    pub fn to_arrow_curved_head_side(self, size_factor: f32) -> f32 {
        match self {
            Size::XSmall => 9.0 * size_factor,
            Size::Small => 9.0 * size_factor,
            Size::Medium => 15.0 * size_factor,
            Size::Large => 19.0 * size_factor,
            Size::XLarge => 26.0 * size_factor,
            Size::XXLarge => 40.0 * size_factor,
        }
    }

    /// Stroke width for the Curved/Double arrow shaft and V-tip head.
    /// Curved/Double shafts are noticeably thicker than the global
    /// `to_line_width` defaults (especially at XSmall and XXLarge). Kept
    /// arrow-specific so we don't fatten lines, rectangles, etc., used by
    /// other tools.
    pub fn to_arrow_curved_shaft_width(self, size_factor: f32) -> f32 {
        match self {
            Size::XSmall => 3.0 * size_factor,
            Size::Small => 3.0 * size_factor,
            Size::Medium => 6.0 * size_factor,
            Size::Large => 7.5 * size_factor,
            Size::XLarge => 11.5 * size_factor,
            Size::XXLarge => 18.5 * size_factor,
        }
    }

    /// Head length (along the shaft) for Fancy arrows. Exact the standard
    /// reference *head triangle* widths (standard 2026-05-09 at
    /// 11.45.11@2x.png), halved from 2× DPR (31, 44, 69, 86, 118, 179).
    /// Excludes the reference's swept-back ear, which is a separate feature
    /// we don't model.
    pub fn to_arrow_fancy_head_length(self, size_factor: f32) -> f32 {
        match self {
            Size::XSmall => 15.5 * size_factor,
            Size::Small => 22.0 * size_factor,
            Size::Medium => 34.5 * size_factor,
            Size::Large => 43.0 * size_factor,
            Size::XLarge => 59.0 * size_factor,
            Size::XXLarge => 89.5 * size_factor,
        }
    }

    /// Full perpendicular head height for Fancy. Stored independently of
    /// `to_arrow_fancy_head_length` because standard uses a per-size apex:
    /// nearly 1:1 (apex ≈ 53°) at XSmall/Small, squatter (apex ≈ 51°) at
    /// larger sizes. Exact reference values halved from 2× DPR (31, 44, 66,
    /// 83, 113, 171).
    pub fn to_arrow_fancy_head_full_height(self, size_factor: f32) -> f32 {
        match self {
            Size::XSmall => 15.5 * size_factor,
            Size::Small => 22.0 * size_factor,
            Size::Medium => 33.0 * size_factor,
            Size::Large => 41.5 * size_factor,
            Size::XLarge => 56.5 * size_factor,
            Size::XXLarge => 85.5 * size_factor,
        }
    }

    pub fn to_blur_factor(self, size_factor: f32) -> f32 {
        match self {
            Size::XSmall => 5.0 * size_factor,
            Size::Small => 10.0 * size_factor,
            Size::Medium => 20.0 * size_factor,
            Size::Large => 30.0 * size_factor,
            Size::XLarge => 45.0 * size_factor,
            Size::XXLarge => 65.0 * size_factor,
        }
    }

    pub fn to_highlight_width(self, size_factor: f32) -> f32 {
        match self {
            Size::XSmall => 8.0 * size_factor,
            Size::Small => 15.0 * size_factor,
            Size::Medium => 30.0 * size_factor,
            Size::Large => 45.0 * size_factor,
            Size::XLarge => 65.0 * size_factor,
            Size::XXLarge => 90.0 * size_factor,
        }
    }
}
