//! `--doctor`: a quick environment check — report whether the optional
//! external tools the Tensaku screenshot workflow leans on are present.
//! Tensaku degrades gracefully without them; this just makes a missing
//! piece easy to spot.

use anyhow::Result;

/// Is `bin` an executable file somewhere on `$PATH`?
fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|dir| dir.join(bin).is_file()))
        .unwrap_or(false)
}

/// A single environment check shown in the `--doctor` report.
struct Check {
    label: &'static str,
    ok: bool,
    /// Shown indented below the label when the check fails.
    hint: &'static str,
}

/// Print the environment report.
pub fn run() -> Result<()> {
    let checks = [
        Check {
            label: "Wayland session (WAYLAND_DISPLAY)",
            ok: std::env::var_os("WAYLAND_DISPLAY").is_some(),
            hint: "Tensaku is a Wayland app — launch it from a Wayland session.",
        },
        Check {
            label: "grim — screenshot capture",
            ok: on_path("grim"),
            hint: "Install grim to pipe screenshots in: grim -g \"$(slurp)\" - | tensaku -f -",
        },
        Check {
            label: "slurp — region selector",
            ok: on_path("slurp"),
            hint: "Install slurp to drag-select a capture region.",
        },
        Check {
            label: "wl-copy — clipboard (default copy-command)",
            ok: on_path("wl-copy"),
            hint: "Install wl-clipboard, or set copy-command to your clipboard tool.",
        },
    ];

    println!("Tensaku environment check\n");
    let mut missing = 0;
    for c in &checks {
        if c.ok {
            println!("  [ ok ]  {}", c.label);
        } else {
            missing += 1;
            println!("  [miss]  {}", c.label);
            println!("          {}", c.hint);
        }
    }
    println!();
    if missing == 0 {
        println!("All good — every tool Tensaku's workflow uses is present.");
    } else {
        println!("{missing} missing. Tensaku still runs, but the noted features won't work.");
    }
    Ok(())
}
