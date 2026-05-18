//! `--install-desktop`: register a `cargo install`ed Tensaku binary
//! with the desktop. `cargo install` places only the executable; this
//! writes the icon and `.desktop` entry into the user's XDG data dir
//! — the same files a package install (AUR, `make install`) drops
//! system-wide, just user-local.

use std::path::PathBuf;

use anyhow::{Context, Result};

/// App icon and desktop entry, embedded into the binary so the install
/// works from a `cargo install`ed binary with no repo checkout present.
const ICON_SVG: &[u8] = include_bytes!("../assets/tensaku.svg");
const DESKTOP_ENTRY: &str = include_str!("../dev.tensaku.Tensaku.desktop");

/// `$XDG_DATA_HOME`, falling back to `$HOME/.local/share`.
fn xdg_data_home() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME").context("neither XDG_DATA_HOME nor HOME is set")?;
    Ok(PathBuf::from(home).join(".local/share"))
}

/// Write the icon + desktop entry into the user's XDG data dir.
pub fn run() -> Result<()> {
    let data = xdg_data_home()?;

    // Icon -> icons/hicolor/scalable/apps/dev.tensaku.Tensaku.svg, so
    // the desktop entry's `Icon=dev.tensaku.Tensaku` resolves by name.
    let icon_dir = data.join("icons/hicolor/scalable/apps");
    std::fs::create_dir_all(&icon_dir).with_context(|| format!("create {}", icon_dir.display()))?;
    let icon_path = icon_dir.join("dev.tensaku.Tensaku.svg");
    std::fs::write(&icon_path, ICON_SVG)
        .with_context(|| format!("write {}", icon_path.display()))?;

    // Desktop entry -> applications/dev.tensaku.Tensaku.desktop, with
    // Exec/TryExec rewritten to this binary's absolute path: `cargo
    // install` drops it in ~/.cargo/bin, which a launcher's environment
    // may not have on PATH.
    let app_dir = data.join("applications");
    std::fs::create_dir_all(&app_dir).with_context(|| format!("create {}", app_dir.display()))?;
    let exe = std::env::current_exe()
        .context("locate the running binary")?
        .display()
        .to_string();
    let entry = DESKTOP_ENTRY
        .replace("TryExec=tensaku", &format!("TryExec={exe}"))
        .replace("Exec=tensaku ", &format!("Exec={exe} "));
    let entry_path = app_dir.join("dev.tensaku.Tensaku.desktop");
    std::fs::write(&entry_path, entry)
        .with_context(|| format!("write {}", entry_path.display()))?;

    println!("Installed Tensaku desktop integration:");
    println!("  icon           {}", icon_path.display());
    println!("  desktop entry  {}", entry_path.display());
    println!();
    println!("Tensaku is now registered with launchers and file managers.");
    Ok(())
}
