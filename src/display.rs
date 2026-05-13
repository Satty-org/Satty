//! Display / compositor integration helpers.
//!
//! Currently a single concern: discovering the user's display scale so
//! the first-run welcome dialog can pre-fill a sensible
//! `annotation_size_factor`. Hyprland is queried directly via
//! `hyprctl monitors -j` because GTK's integer `scale_factor` doesn't
//! expose fractional scales (1.25×, 1.5×) reliably.

use std::process::Command;

/// Try to read the focused monitor's scale from Hyprland. Returns
/// `None` when not running under Hyprland or when `hyprctl` is missing
/// / failing — callers should fall back to `1.0`.
///
/// We deliberately avoid pulling in `serde_json` for this one-shot
/// lookup; the JSON we care about is structurally trivial and the
/// inline parser below tracks just enough state to match each `"scale"`
/// to its surrounding monitor block's `"focused"` flag.
pub fn detect_hyprland_scale() -> Option<f32> {
    // Cheap pre-check: when this env var is missing we're not running
    // under Hyprland and shelling out would only produce noise.
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let output = Command::new("hyprctl")
        .args(["monitors", "-j"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = std::str::from_utf8(&output.stdout).ok()?;
    parse_scale(text)
}

/// Walk the `hyprctl monitors -j` output and return the focused
/// monitor's `"scale"`. Falls back to the first scale seen if nothing
/// is marked focused (edge states on multi-monitor setups).
///
/// We split on `}` so each chunk is one monitor's key/value lines —
/// hyprctl's pretty-printed output uses `},{` as the inter-monitor
/// boundary, which doesn't match a clean per-line `}` heuristic.
fn parse_scale(text: &str) -> Option<f32> {
    let mut first_scale: Option<f32> = None;

    for chunk in text.split('}') {
        let mut scale: Option<f32> = None;
        let mut focused = false;

        for raw in chunk.lines() {
            let line = raw.trim().trim_end_matches(',');
            if let Some(value) = line.strip_prefix("\"scale\":") {
                scale = value.trim().parse::<f32>().ok();
            } else if let Some(value) = line.strip_prefix("\"focused\":") {
                focused = value.trim() == "true";
            }
        }

        if let Some(s) = scale {
            if first_scale.is_none() {
                first_scale = Some(s);
            }
            if focused {
                return Some(s);
            }
        }
    }
    first_scale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_focused_monitor_scale() {
        // Trimmed shape of the real `hyprctl monitors -j` output —
        // two monitors, second one focused with a different scale.
        let json = r#"[{
    "id": 0,
    "name": "DP-1",
    "scale": 1.00,
    "focused": false
},{
    "id": 1,
    "name": "DP-3",
    "scale": 2.00,
    "focused": true
}]"#;
        assert_eq!(parse_scale(json), Some(2.0));
    }

    #[test]
    fn falls_back_to_first_when_nothing_focused() {
        let json = r#"[{
    "scale": 1.50,
    "focused": false
}]"#;
        assert_eq!(parse_scale(json), Some(1.5));
    }

    #[test]
    fn returns_none_on_unrelated_input() {
        assert_eq!(parse_scale("not json"), None);
    }
}
