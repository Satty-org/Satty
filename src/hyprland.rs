//! Runtime Hyprland bind override for Super+wheel zoom.
//!
//! Satty uses Super+scroll on its canvas for zoom, but Omarchy and
//! most tiling setups already bind `SUPER, mouse_up` / `mouse_down`
//! to workspace switching at the compositor level. Compositor binds
//! fire before GTK sees the event, so Satty can't read Super+wheel
//! unless we suppress those binds while Satty is focused.
//!
//! Approach: at startup we snapshot the user's current Super+mouse_up
//! and Super+mouse_down binds via `hyprctl binds -j`. On focus-in we
//! unbind them so the wheel events fall through to GTK. On focus-out
//! / window-destroy we restore them with `hyprctl keyword bind ...`
//! reconstructed from the snapshot. The user's `hyprland.conf` is
//! never touched — everything is a runtime overlay that disappears
//! the moment Satty exits (or that the user can clear with
//! `hyprctl reload` if a crash strands Satty mid-focus).
//!
//! Super+keyboard chords (Super+D, etc.) intentionally are NOT
//! hijacked here. In testing the compositor still consumed the
//! modifier before GTK saw the press — even when the bind had been
//! cleared via `hyprctl keyword unbind`. Satty's keyboard shortcuts
//! stick to Ctrl chords, which reach GTK reliably.
//!
//! We deliberately avoid pulling in `serde_json` for this one-shot
//! lookup; the JSON is flat and an inline parser like
//! `display::parse_scale` is plenty.

use std::process::Command;

/// One cached Super+mouse_(up|down) bind snapshotted at startup,
/// to be restored on focus-out. Captures the dispatcher / arg plus
/// the bind-variant flag letters (l/r/e/n) so we re-issue the same
/// directive shape — e.g. `bindl` instead of plain `bind` if the
/// user had marked it locked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindEntry {
    /// `"mouse_up"` or `"mouse_down"`.
    pub key: String,
    pub dispatcher: String,
    pub arg: String,
    pub locked: bool,
    pub release: bool,
    pub repeat: bool,
    pub non_consuming: bool,
}

/// Hyprland's modmask bit for the Super (MOD4 / logo) key on its own.
const MOD_SUPER: u32 = 64;

/// Super-modified keys that Satty reserves for its own use while
/// focused. Any Hyprland bind on a key in this list with modmask =
/// pure Super (no Shift/Ctrl/Alt extras) gets snapshotted at startup,
/// unbound on focus-in, and restored on focus-out / destroy.
///
/// `mouse_up` / `mouse_down` — Super+scroll zoom on the canvas.
/// Keyboard chords like Super+D *would* go here too, but the
/// compositor consumes the Super flag before GTK sees the press
/// even after the unbind — so Satty's keyboard shortcuts stick to
/// Ctrl chords instead.
const KEYS_TO_HIJACK: &[&str] = &["mouse_up", "mouse_down"];

/// Read every Super+key bind in `KEYS_TO_HIJACK` from the default
/// submap. Returns empty on non-Hyprland systems, on `hyprctl`
/// failures, or when no matching binds exist (nothing for us to
/// suppress).
pub fn read_super_mouse_binds() -> Vec<BindEntry> {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        return Vec::new();
    }
    let Ok(output) = Command::new("hyprctl").args(["binds", "-j"]).output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let Ok(text) = std::str::from_utf8(&output.stdout) else {
        return Vec::new();
    };
    parse_binds(text)
}

/// Inline parser. Splits the JSON array on `}` so each chunk is one
/// bind object's contents — the same trick `display::parse_scale`
/// uses. Each chunk is then line-scanned for the four required string
/// fields (`key`, `submap`, `dispatcher`, `arg`), the `modmask`
/// number, and the four flag booleans. Objects missing any required
/// field are skipped (so we don't accidentally treat the trailing
/// `]` chunk as a bind).
fn parse_binds(text: &str) -> Vec<BindEntry> {
    let mut out = Vec::new();
    for chunk in text.split('}') {
        let mut modmask: Option<u32> = None;
        let mut submap: Option<String> = None;
        let mut key: Option<String> = None;
        let mut dispatcher: Option<String> = None;
        let mut arg: Option<String> = None;
        let mut locked = false;
        let mut release = false;
        let mut repeat = false;
        let mut non_consuming = false;
        let mut catch_all = false;

        for raw in chunk.lines() {
            let line = raw.trim().trim_end_matches(',');
            if let Some(v) = line.strip_prefix("\"modmask\":") {
                modmask = v.trim().parse().ok();
            } else if let Some(v) = line.strip_prefix("\"submap\":") {
                submap = unquote(v.trim());
            } else if let Some(v) = line.strip_prefix("\"key\":") {
                key = unquote(v.trim());
            } else if let Some(v) = line.strip_prefix("\"dispatcher\":") {
                dispatcher = unquote(v.trim());
            } else if let Some(v) = line.strip_prefix("\"arg\":") {
                arg = unquote(v.trim());
            } else if let Some(v) = line.strip_prefix("\"locked\":") {
                locked = v.trim() == "true";
            } else if let Some(v) = line.strip_prefix("\"release\":") {
                release = v.trim() == "true";
            } else if let Some(v) = line.strip_prefix("\"repeat\":") {
                repeat = v.trim() == "true";
            } else if let Some(v) = line.strip_prefix("\"non_consuming\":") {
                non_consuming = v.trim() == "true";
            } else if let Some(v) = line.strip_prefix("\"catch_all\":") {
                catch_all = v.trim() == "true";
            }
        }

        let (Some(modmask), Some(submap), Some(key), Some(dispatcher), Some(arg)) =
            (modmask, submap, key, dispatcher, arg)
        else {
            continue;
        };
        // Pure Super (no extra modifiers), default submap, normal
        // (non-catch-all) bind targeting one of the two wheel keys.
        if modmask != MOD_SUPER || !submap.is_empty() || catch_all {
            continue;
        }
        if !KEYS_TO_HIJACK.iter().any(|k| *k == key.as_str()) {
            continue;
        }
        out.push(BindEntry {
            key,
            dispatcher,
            arg,
            locked,
            release,
            repeat,
            non_consuming,
        });
    }
    out
}

fn unquote(s: &str) -> Option<String> {
    let s = s.strip_prefix('"')?;
    let s = s.strip_suffix('"')?;
    Some(s.to_string())
}

/// Unbind every Super+key listed in `KEYS_TO_HIJACK` at the Hyprland
/// level. Idempotent — repeated calls on the same focus cycle are a
/// no-op. Safe on non-Hyprland (the env-var gate short-circuits
/// before we shell out).
pub fn unbind_super_mouse() {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        return;
    }
    for key in KEYS_TO_HIJACK {
        let _ = Command::new("hyprctl")
            .args(["keyword", "unbind", &format!("SUPER,{key}")])
            .status();
    }
}

/// Restore every snapshotted bind by issuing `hyprctl keyword
/// bind[lren] SUPER,KEY,DISPATCHER,ARG`. Flag letters are appended
/// in `l r e n` order matching Hyprland's accepted suffixes:
/// `l`=locked, `r`=release, `e`=repeat, `n`=non_consuming. An empty
/// snapshot (non-Hyprland host, or no matching binds at startup) is
/// a clean no-op.
pub fn rebind_all(binds: &[BindEntry]) {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        return;
    }
    for entry in binds {
        let mut keyword = String::from("bind");
        if entry.locked {
            keyword.push('l');
        }
        if entry.release {
            keyword.push('r');
        }
        if entry.repeat {
            keyword.push('e');
        }
        if entry.non_consuming {
            keyword.push('n');
        }
        let directive = format!(
            "SUPER,{},{},{}",
            entry.key, entry.dispatcher, entry.arg
        );
        let _ = Command::new("hyprctl")
            .args(["keyword", &keyword, &directive])
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_super_workspace_wheel_binds() {
        let json = r#"[{
    "locked": false,
    "mouse": true,
    "release": false,
    "repeat": false,
    "non_consuming": false,
    "has_description": false,
    "modmask": 64,
    "submap": "",
    "key": "mouse_up",
    "keycode": 0,
    "catch_all": false,
    "description": "",
    "dispatcher": "workspace",
    "arg": "e+1"
},{
    "locked": false,
    "mouse": true,
    "release": false,
    "repeat": false,
    "non_consuming": false,
    "has_description": false,
    "modmask": 64,
    "submap": "",
    "key": "mouse_down",
    "keycode": 0,
    "catch_all": false,
    "description": "",
    "dispatcher": "workspace",
    "arg": "e-1"
}]"#;
        let binds = parse_binds(json);
        assert_eq!(binds.len(), 2);
        assert_eq!(binds[0].key, "mouse_up");
        assert_eq!(binds[0].dispatcher, "workspace");
        assert_eq!(binds[0].arg, "e+1");
        assert_eq!(binds[1].key, "mouse_down");
        assert_eq!(binds[1].arg, "e-1");
        assert!(!binds[0].locked);
    }

    #[test]
    fn skips_non_super_mouse_wheel_binds() {
        // SUPER+SHIFT (modmask 65) shouldn't qualify — we only want
        // pure Super, since that's what the compositor steals from us.
        let json = r#"[{
    "modmask": 65,
    "submap": "",
    "key": "mouse_up",
    "catch_all": false,
    "dispatcher": "workspace",
    "arg": "1",
    "locked": false,
    "release": false,
    "repeat": false,
    "non_consuming": false
}]"#;
        assert!(parse_binds(json).is_empty());
    }

    #[test]
    fn skips_binds_in_other_submaps() {
        let json = r#"[{
    "modmask": 64,
    "submap": "resize",
    "key": "mouse_up",
    "catch_all": false,
    "dispatcher": "workspace",
    "arg": "e+1",
    "locked": false,
    "release": false,
    "repeat": false,
    "non_consuming": false
}]"#;
        assert!(parse_binds(json).is_empty());
    }

    #[test]
    fn skips_non_wheel_keys() {
        let json = r#"[{
    "modmask": 64,
    "submap": "",
    "key": "Q",
    "catch_all": false,
    "dispatcher": "killactive",
    "arg": "",
    "locked": false,
    "release": false,
    "repeat": false,
    "non_consuming": false
}]"#;
        assert!(parse_binds(json).is_empty());
    }

    #[test]
    fn captures_bind_flag_booleans() {
        let json = r#"[{
    "modmask": 64,
    "submap": "",
    "key": "mouse_up",
    "catch_all": false,
    "dispatcher": "workspace",
    "arg": "e+1",
    "locked": true,
    "release": false,
    "repeat": true,
    "non_consuming": false
}]"#;
        let binds = parse_binds(json);
        assert_eq!(binds.len(), 1);
        assert!(binds[0].locked);
        assert!(binds[0].repeat);
        assert!(!binds[0].release);
    }
}
