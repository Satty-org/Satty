//! Desktop integration for a `cargo install`ed Tensaku.
//!
//! `cargo install` places only the executable — no icon, no `.desktop`
//! entry — so a crates.io install wouldn't appear in launchers or file
//! managers. This module writes those files into the user's XDG data
//! dir (the same files an AUR / `make install` package drops
//! system-wide, just user-local). It runs two ways:
//!
//! - [`run`] — the explicit `--install-desktop` flag; prints a report.
//! - [`ensure_first_launch`] — silent and one-shot, on the first
//!   normal launch, so the integration appears with no command to
//!   remember.

use std::path::PathBuf;

use anyhow::{Context, Result};
use xdg::BaseDirectories;

/// App icon and desktop entry, embedded into the binary so the install
/// works from a `cargo install`ed binary with no repo checkout present.
const ICON_SVG: &[u8] = include_bytes!("../assets/tensaku.svg");
const DESKTOP_ENTRY: &str = include_str!("../dev.tensaku.Tensaku.desktop");

/// Tensaku's reverse-DNS app id — the basename of the desktop entry
/// (`.desktop`) and, with an `.svg` suffix, the icon.
const APP_ID: &str = "dev.tensaku.Tensaku";

/// `$XDG_DATA_HOME`, falling back to `$HOME/.local/share`.
fn xdg_data_home() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME").context("neither XDG_DATA_HOME nor HOME is set")?;
    Ok(PathBuf::from(home).join(".local/share"))
}

/// Write the icon + desktop entry into the user's XDG data dir.
/// Returns the written (icon, desktop-entry) paths.
fn install() -> Result<(PathBuf, PathBuf)> {
    let data = xdg_data_home()?;

    // Icon -> icons/hicolor/scalable/apps/<app-id>.svg, so the desktop
    // entry's `Icon=dev.tensaku.Tensaku` resolves by name.
    let icon_dir = data.join("icons/hicolor/scalable/apps");
    std::fs::create_dir_all(&icon_dir).with_context(|| format!("create {}", icon_dir.display()))?;
    let icon_path = icon_dir.join(format!("{APP_ID}.svg"));
    std::fs::write(&icon_path, ICON_SVG)
        .with_context(|| format!("write {}", icon_path.display()))?;

    // Desktop entry -> applications/<app-id>.desktop, with Exec/TryExec
    // rewritten to this binary's absolute path: `cargo install` drops
    // it in ~/.cargo/bin, which a launcher's environment may not have
    // on PATH.
    let app_dir = data.join("applications");
    std::fs::create_dir_all(&app_dir).with_context(|| format!("create {}", app_dir.display()))?;
    let exe = std::env::current_exe()
        .context("locate the running binary")?
        .display()
        .to_string();
    let entry = DESKTOP_ENTRY
        .replace("TryExec=tensaku", &format!("TryExec={exe}"))
        .replace("Exec=tensaku ", &format!("Exec={exe} "));
    let entry_path = app_dir.join(format!("{APP_ID}.desktop"));
    std::fs::write(&entry_path, entry)
        .with_context(|| format!("write {}", entry_path.display()))?;

    Ok((icon_path, entry_path))
}

/// `--install-desktop`: install the integration and report what landed.
pub fn run() -> Result<()> {
    let (icon_path, entry_path) = install()?;

    println!("Installed Tensaku desktop integration:");
    println!("  icon           {}", icon_path.display());
    println!("  desktop entry  {}", entry_path.display());
    println!();
    println!("Tensaku is now registered with launchers and file managers.");
    Ok(())
}

/// Does a system XDG data dir already provide our desktop entry? An
/// AUR / distro / `make install` package drops it under, typically,
/// `/usr/share/applications` (or `/usr/local/share/applications`) — in
/// which case a user-local copy would only shadow it.
fn packaged_entry_exists() -> bool {
    let dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    std::env::split_paths(&dirs)
        .any(|d| d.join(format!("applications/{APP_ID}.desktop")).is_file())
}

/// Install the desktop integration silently on the first normal launch,
/// so a `cargo install`ed Tensaku shows up in launchers without the
/// user knowing to run `--install-desktop`.
///
/// One-shot: a marker in the XDG state dir records that the first-launch
/// step has run, so it never repeats — not even if the user later
/// removes the entry on purpose (a *first-launch* action happens once).
/// Packaged installs (Flatpak, AUR / distro) already ship an entry and
/// are skipped. Best-effort throughout — desktop-file housekeeping must
/// never break app startup, so any failure is swallowed (and left to
/// retry next launch); `--install-desktop` is the loud, explicit path.
pub fn ensure_first_launch() {
    let _ = try_ensure_first_launch();
}

fn try_ensure_first_launch() -> Result<()> {
    // Inside a Flatpak the runtime ships the entry, and the sandboxed
    // XDG dirs make a user-local copy pointless either way.
    if std::env::var_os("FLATPAK_ID").is_some() {
        return Ok(());
    }

    // The marker means the one-time first-launch step is already done.
    // `place_state_file` also creates the state dir for the write below.
    let marker = BaseDirectories::with_prefix(env!("CARGO_PKG_NAME"))
        .place_state_file("desktop-install-done")
        .context("locate the XDG state dir")?;
    if marker.exists() {
        return Ok(());
    }

    // Install only if nothing already provides the entry — neither an
    // earlier --install-desktop run nor a system package.
    let user_entry = xdg_data_home()?.join(format!("applications/{APP_ID}.desktop"));
    if !user_entry.exists() && !packaged_entry_exists() {
        install()?;
    }

    // Record completion last, so a failed install above is retried on
    // the next launch rather than marked done.
    std::fs::write(
        &marker,
        "Tensaku ran its one-time first-launch desktop integration.\n",
    )
    .with_context(|| format!("write {}", marker.display()))?;
    Ok(())
}
