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
use crate::tools::{ArrowStyle, BlurStyle, HighlighterStyle, TextBackground, Tools};

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
    /// Last-chosen arrow geometry (Standard / Pointy / Curved / Double).
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
    /// Last-chosen highlighter style (TextLocked / Normal). Same
    /// auto-save semantics: cycling via toolbar or double-tap
    /// persists, restoring the user's preference on next launch.
    #[serde(default)]
    pub highlighter_style: Option<HighlighterStyle>,
    /// User-edited keyboard shortcuts from the Preferences dialog.
    /// Map keyed by `Tools`; each value is a single character (the
    /// shortcut). Tools missing from this map fall back to the
    /// defaults in `Keybinds::default()` (still merged by
    /// config.toml first). `None` means "preferences dialog has
    /// never been saved" — leaves the config/default map alone.
    #[serde(default)]
    pub keybinds: Option<HashMap<Tools, String>>,
    /// Flip the sign of every scroll delta the app consumes. `None`
    /// is "use default" (false = no inversion). Toggled from the
    /// Preferences dialog; reversing this flips zoom, pan,
    /// scroll-resize, and the annotation-pill bump together so the
    /// user can pick whichever direction feels right.
    #[serde(default)]
    pub invert_scrolling: Option<bool>,
    /// Whether pressing Esc on the canvas closes satty. `None` means
    /// "use default" (false). Independent of `actions_on_escape` —
    /// this just gates the implicit Exit action so users with
    /// explicit per-Esc-action config keep their behavior either way.
    #[serde(default)]
    pub close_on_esc: Option<bool>,
    /// Whether to hide the default 10-color palette in the color
    /// picker popover. When true, the palette column disappears and
    /// the 1–9, 0 shortcut keys map to the first column of saved
    /// custom colors instead. `None` = default (false, palette shown).
    #[serde(default)]
    pub hide_default_palette: Option<bool>,
    /// Saved-default number of Chaikin post-stroke smoothing passes
    /// for the brush tool. `None` = "never explicitly saved"; callers
    /// fall back to the config / built-in default (2).
    #[serde(default)]
    pub brush_post_smooth_iterations: Option<usize>,
    /// Per-tool saved-default fill state (true = filled, false =
    /// outline). Keyed by `Tools`; only Rectangle / Ellipse currently
    /// honor this. A missing entry means "no saved default — use the
    /// `default-fill-shapes` config value".
    #[serde(default)]
    pub fill_per_tool: HashMap<Tools, bool>,
    /// When true, in-session adjustments to per-tool defaults (size,
    /// fill, highlighter opacity, brush smoothness) stick across tool
    /// switches for the duration of the session — only a fresh app
    /// launch re-applies the saved defaults. When false (default), the
    /// saved defaults snap back every time the user switches into a
    /// tool.
    /// Spotlight darkness is unaffected — it's already global
    /// per-session and lives in `spotlight_darkness`.
    #[serde(default)]
    pub sticky_session_defaults: Option<bool>,
    /// Pixel width of the layer panel (Paned start_child slot). `None`
    /// means "user hasn't dragged the divider yet" — falls back to the
    /// in-code default. Persisted so re-opening the app puts the
    /// divider where the user last left it.
    #[serde(default)]
    pub layer_panel_width: Option<f32>,
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

/// Load saved-custom colors as a sparse slot list — `None` entries
/// represent empty placeholder slots the user has intentionally left
/// (e.g. by dragging a color away from its position). Empty slots are
/// encoded on disk as `HexColor::rgba(0, 0, 0, 0)` (fully transparent
/// black), which is unreachable through the color chooser UI in normal
/// use; if a user does manage to save a fully-transparent black color,
/// it'll round-trip as an empty slot — acceptable since that color
/// would render as nothing visible anyway.
pub fn load_custom_colors() -> Vec<Option<Color>> {
    load()
        .saved_custom_colors
        .into_iter()
        .map(hex_to_slot)
        .collect()
}

/// Add `color` to the persisted saved-custom list. Fills the first
/// `None` slot if one exists (so explicit gaps left by drag-aways get
/// reused first); otherwise appends a new slot at the end. Returns the
/// new list so callers can update their in-memory mirror without a
/// separate re-load.
pub fn append_custom_color(color: Color) -> Vec<Option<Color>> {
    let mut slots = load_custom_colors();
    if let Some(empty_idx) = slots.iter().position(Option::is_none) {
        slots[empty_idx] = Some(color);
    } else {
        slots.push(Some(color));
    }
    save_custom_colors(&slots);
    slots
}

fn hex_to_slot(hc: HexColor) -> Option<Color> {
    if hc.r == 0 && hc.g == 0 && hc.b == 0 && hc.a == 0 {
        None
    } else {
        Some(Color::from(hc))
    }
}

fn slot_to_hex(slot: Option<Color>) -> HexColor {
    match slot {
        Some(c) => HexColor::rgba(c.r, c.g, c.b, c.a),
        None => HexColor::rgba(0, 0, 0, 0),
    }
}

pub fn load_layer_panel_width() -> Option<f32> {
    load().layer_panel_width
}

pub fn save_layer_panel_width(value: f32) {
    let mut state = load();
    state.layer_panel_width = Some(value);
    save(&state);
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

pub fn load_highlighter_style() -> Option<HighlighterStyle> {
    load().highlighter_style
}

pub fn save_highlighter_style(style: HighlighterStyle) {
    let mut state = load();
    state.highlighter_style = Some(style);
    save(&state);
}

/// Persisted user-edited keybinds, if the Preferences dialog has ever
/// been saved. Returned as a `char` map matching the in-memory shape
/// `Keybinds` uses (we store strings on disk so TOML doesn't choke on
/// single-char values).
pub fn load_keybinds() -> Option<HashMap<Tools, char>> {
    load().keybinds.map(|map| {
        map.into_iter()
            .filter_map(|(tool, s)| {
                let mut chars = s.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => Some((tool, c)),
                    _ => None,
                }
            })
            .collect()
    })
}

/// Whether the user has opted into in-app scroll inversion. Defaults
/// to `true` for fresh installs (matches the typical user
/// expectation that wheel-up grows / zooms in and pan follows the
/// natural-scroll direction); existing state.toml values still
/// win, so an explicit `false` stays `false`.
pub fn load_invert_scrolling() -> bool {
    load().invert_scrolling.unwrap_or(true)
}

/// Persist the scroll-inversion preference. Called from the
/// Preferences dialog's CheckButton toggle so the choice survives
/// restarts.
pub fn save_invert_scrolling(value: bool) {
    let mut state = load();
    state.invert_scrolling = Some(value);
    save(&state);
}

/// Whether Esc should close satty (in addition to firing any
/// `actions_on_escape` from config). Defaults to false.
pub fn load_close_on_esc() -> bool {
    load().close_on_esc.unwrap_or(false)
}

pub fn save_close_on_esc(value: bool) {
    let mut state = load();
    state.close_on_esc = Some(value);
    save(&state);
}

/// Whether the color picker popover hides its default 10-color
/// palette column. When true, the 1–9, 0 number-key shortcuts pick
/// from the first column of saved-custom colors instead. Defaults
/// to false.
pub fn load_hide_default_palette() -> bool {
    load().hide_default_palette.unwrap_or(false)
}

pub fn save_hide_default_palette(value: bool) {
    let mut state = load();
    state.hide_default_palette = Some(value);
    save(&state);
}

/// Saved-default number of post-stroke Chaikin smoothing passes for
/// the brush. `None` = use config / built-in default.
pub fn load_brush_post_smooth_iterations() -> Option<usize> {
    load().brush_post_smooth_iterations
}

pub fn save_brush_post_smooth_iterations(value: usize) {
    let mut state = load();
    state.brush_post_smooth_iterations = Some(value);
    save(&state);
}

/// Read this tool's saved-default fill state, if any. Returns `None`
/// when the user has never persisted a fill default for the tool —
/// callers fall back to `APP_CONFIG.default_fill_shapes()`.
pub fn load_fill_for_tool(tool: Tools) -> Option<bool> {
    load().fill_per_tool.get(&tool).copied()
}

/// Persist `fill` as the saved-default fill state for `tool`. Future
/// launches and future entries into `tool` will start at this fill.
pub fn save_fill_for_tool(tool: Tools, fill: bool) {
    let mut state = load();
    state.fill_per_tool.insert(tool, fill);
    save(&state);
}

/// Whether the user has opted into "sticky session defaults" —
/// in-session per-tool adjustments survive tool switches and only
/// reset on a fresh app launch. Defaults to false (snap-back on
/// every tool switch, the original behavior).
pub fn load_sticky_session_defaults() -> bool {
    load().sticky_session_defaults.unwrap_or(false)
}

pub fn save_sticky_session_defaults(value: bool) {
    let mut state = load();
    state.sticky_session_defaults = Some(value);
    save(&state);
}

/// Replace the persisted keybind map wholesale. Called by the
/// Preferences dialog's Save handler with the user's edited set.
pub fn save_keybinds(shortcuts: &HashMap<char, Tools>) {
    let mut state = load();
    let serialized: HashMap<Tools, String> = shortcuts
        .iter()
        .map(|(c, t)| (*t, c.to_string()))
        .collect();
    state.keybinds = Some(serialized);
    save(&state);
}

/// Replace the persisted saved-custom slot list wholesale. Trailing
/// `None`s are trimmed (they carry no information — the user can
/// always grow the list again by dragging or saving a new color), but
/// mid-list `None`s are preserved so explicit gaps survive a restart.
pub fn save_custom_colors(slots: &[Option<Color>]) {
    let mut state = load();
    let mut trimmed: Vec<Option<Color>> = slots.to_vec();
    while matches!(trimmed.last(), Some(None)) {
        trimmed.pop();
    }
    state.saved_custom_colors = trimmed.into_iter().map(slot_to_hex).collect();
    save(&state);
}
