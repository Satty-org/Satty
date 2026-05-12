//! Per-user persistent UI state — survives across launches, separate
//! from the read-only user config in `configuration.rs`. Lives in the
//! XDG state dir (`~/.local/state/satty/state.toml` on Linux).

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use hex_color::HexColor;
use serde::{Deserialize, Serialize};
use xdg::BaseDirectories;

use crate::style::{Color, Size};
use crate::tools::{ArrowStyle, BlurStyle, TextBackground, Tools};

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PersistedState {
    pub last_color: Option<HexColor>,
    #[serde(default)]
    pub saved_custom_colors: Vec<HexColor>,
    /// Spotlight overlay darkness (0.10–0.90). None = use the
    /// 50% default (detent value).
    #[serde(default)]
    pub spotlight_darkness: Option<f32>,
    /// Highlighter stroke opacity (0.10–1.00). None = use the
    /// 40% default.
    #[serde(default)]
    pub highlighter_opacity: Option<f32>,
    /// Annotation size factor (multiplier applied to all Size-based
    /// metrics — text size, line width, etc.). `None` triggers the
    /// first-run welcome dialog so the user picks a value matching
    /// their display scale before they can use the app. Once saved,
    /// the dialog never reappears unless the user clears their state.
    #[serde(default)]
    pub annotation_size_factor: Option<f32>,
    /// Crop tool's "Snap to edges" preference (the bottom-left
    /// checkbox while cropping). `None` means "use default" — true.
    #[serde(default)]
    pub snap_to_edges: Option<bool>,
    /// Saved-default size per tool. Keyed by `Tools` (serializes to
    /// the lowercase tool name); `None` for a missing entry means
    /// "use the global Size::Medium default". Updated only by the
    /// size slider's right-click → "Save as default" — the slider's
    /// live value isn't persisted on every drag.
    #[serde(default)]
    pub size_per_tool: HashMap<Tools, Size>,
    /// Last-chosen arrow geometry (Standard / Fancy / Curved / Double).
    /// Auto-saved on every selection so re-opening the Arrow tool
    /// picks up where the user left off.
    #[serde(default)]
    pub arrow_style: Option<ArrowStyle>,
    /// Last-chosen blur algorithm (Gaussian / Pixelate). Same
    /// auto-save semantics as `arrow_style`.
    #[serde(default)]
    pub blur_style: Option<BlurStyle>,
    /// Last-chosen text background style (Plain / Rounded). Same
    /// auto-save semantics as `arrow_style` — re-opening the Text
    /// tool restores the user's last choice.
    #[serde(default)]
    pub text_background: Option<TextBackground>,
}

fn state_path() -> Option<PathBuf> {
    let dirs = BaseDirectories::with_prefix(env!("CARGO_PKG_NAME"));
    dirs.place_state_file("state.toml").ok()
}

pub fn load() -> PersistedState {
    let Some(path) = state_path() else {
        return PersistedState::default();
    };
    let Ok(content) = fs::read_to_string(&path) else {
        return PersistedState::default();
    };
    toml::from_str(&content).unwrap_or_default()
}

fn save(state: &PersistedState) {
    let Some(path) = state_path() else { return };
    let Ok(s) = toml::to_string(state) else { return };
    let _ = fs::write(path, s);
}

pub fn save_last_color(color: Color) {
    let mut state = load();
    state.last_color = Some(HexColor::rgba(color.r, color.g, color.b, color.a));
    save(&state);
}

pub fn load_last_color() -> Option<Color> {
    load().last_color.map(Color::from)
}

/// Resolve the startup annotation color. Persisted last-color wins;
/// otherwise red. Shared between the toolbar (swatch preview) and
/// sketch_board (drawing style) so the very first stroke after launch
/// matches the swatch the user sees — `Style::default()` would
/// otherwise resolve color to whatever the palette's first entry is,
/// which is independent of (and can disagree with) the user's
/// previously-chosen color.
pub fn initial_color() -> Color {
    load_last_color().unwrap_or_else(Color::red)
}

pub fn load_custom_colors() -> Vec<Color> {
    load()
        .saved_custom_colors
        .into_iter()
        .map(Color::from)
        .collect()
}

/// Append `color` to the persisted saved-custom list. Returns the new
/// list so callers can update their in-memory mirror without a separate
/// re-load. Duplicates are *not* deduplicated — saving twice produces
/// two adjacent slots, matching what most users expect ("I clicked save
/// twice, I should see two swatches"); callers that want dedup should
/// filter the input first.
pub fn append_custom_color(color: Color) -> Vec<Color> {
    let mut state = load();
    state
        .saved_custom_colors
        .push(HexColor::rgba(color.r, color.g, color.b, color.a));
    save(&state);
    state
        .saved_custom_colors
        .into_iter()
        .map(Color::from)
        .collect()
}

pub fn load_spotlight_darkness() -> Option<f32> {
    load().spotlight_darkness
}

pub fn save_spotlight_darkness(value: f32) {
    let mut state = load();
    state.spotlight_darkness = Some(value);
    save(&state);
}

pub fn load_highlighter_opacity() -> Option<f32> {
    load().highlighter_opacity
}

pub fn save_highlighter_opacity(value: f32) {
    let mut state = load();
    state.highlighter_opacity = Some(value);
    save(&state);
}

/// Persisted annotation size factor. `None` means "never been set" —
/// triggers the welcome dialog at next launch.
pub fn load_annotation_size_factor() -> Option<f32> {
    load().annotation_size_factor
}

pub fn save_annotation_size_factor(value: f32) {
    let mut state = load();
    state.annotation_size_factor = Some(value);
    save(&state);
}

/// "Snap to edges" toggle for the crop tool. `None` falls back to
/// the default (true) — callers handle the unwrap to keep the
/// reader honest about the missing-state case.
pub fn load_snap_to_edges() -> Option<bool> {
    load().snap_to_edges
}

pub fn save_snap_to_edges(value: bool) {
    let mut state = load();
    state.snap_to_edges = Some(value);
    save(&state);
}

/// Read this tool's saved-default size, if the user has explicitly
/// saved one via the size slider's right-click → "Save as default".
pub fn load_size_for_tool(tool: Tools) -> Option<Size> {
    load().size_per_tool.get(&tool).copied()
}

/// Persist `size` as the default for `tool`. Future launches and
/// future tool switches into `tool` will start at this size.
pub fn save_size_for_tool(tool: Tools, size: Size) {
    let mut state = load();
    state.size_per_tool.insert(tool, size);
    save(&state);
}

pub fn load_arrow_style() -> Option<ArrowStyle> {
    load().arrow_style
}

pub fn save_arrow_style(style: ArrowStyle) {
    let mut state = load();
    state.arrow_style = Some(style);
    save(&state);
}

pub fn load_blur_style() -> Option<BlurStyle> {
    load().blur_style
}

pub fn save_blur_style(style: BlurStyle) {
    let mut state = load();
    state.blur_style = Some(style);
    save(&state);
}

pub fn load_text_background() -> Option<TextBackground> {
    load().text_background
}

pub fn save_text_background(bg: TextBackground) {
    let mut state = load();
    state.text_background = Some(bg);
    save(&state);
}

/// Replace the persisted saved-custom list wholesale. Used by
/// reorder + delete flows where the caller has already computed the
/// final list in memory.
pub fn save_custom_colors(colors: &[Color]) {
    let mut state = load();
    state.saved_custom_colors = colors
        .iter()
        .map(|c| HexColor::rgba(c.r, c.g, c.b, c.a))
        .collect();
    save(&state);
}
