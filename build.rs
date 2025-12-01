#[allow(dead_code)]
use std::fs;
use std::io;

use clap::CommandFactory;
use clap_complete::{generate_to, Shell};
use clap_complete_fig::Fig;
use clap_complete_nushell::Nushell;

use satty_cli::command_line;

fn main() -> Result<(), io::Error> {
    let cmd = &mut command_line::CommandLine::command();
    let bin = "satty";
    let out = "completions";

    fs::create_dir_all(out)?;
    generate_to(Shell::Bash, cmd, bin, out)?;
    generate_to(Shell::Fish, cmd, bin, out)?;
    generate_to(Shell::Zsh, cmd, bin, out)?;
    generate_to(Shell::Elvish, cmd, bin, out)?;
    generate_to(Nushell, cmd, bin, out)?;
    generate_to(Fig, cmd, bin, out)?;

    relm4_icons_build::bundle_icons(
        "icon_names.rs",
        Some("com.gabm.satty"),
        None,
        None::<&str>,
        [
            "pen-regular",
            "color-regular",
            "cursor-regular",
            "number-circle-1-regular",
            "drop-regular",
            "highlight-regular",
            "arrow-redo-filled",
            "arrow-undo-filled",
            "recycling-bin",
            "save-regular",
            "save-multiple-regular",
            "copy-regular",
            "text-case-title-regular",
            "text-font-regular",
            "minus-large",
            "checkbox-unchecked-regular",
            "circle-regular",
            "crop-filled",
            "arrow-up-right-filled",
            "rectangle-landscape-regular",
            "paint-bucket-filled",
            "paint-bucket-regular",
            "page-fit-regular",
            "resize-large-regular",
        ],
    );

    Ok(())
}
