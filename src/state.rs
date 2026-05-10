//! Per-user persistent UI state — survives across launches, separate
//! from the read-only user config in `configuration.rs`. Lives in the
//! XDG state dir (`~/.local/state/satty/state.toml` on Linux).

use std::fs;
use std::path::PathBuf;

use hex_color::HexColor;
use serde::{Deserialize, Serialize};
use xdg::BaseDirectories;

use crate::style::Color;

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PersistedState {
    pub last_color: Option<HexColor>,
    #[serde(default)]
    pub saved_custom_colors: Vec<HexColor>,
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
