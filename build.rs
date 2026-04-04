#[allow(dead_code)]
use std::borrow::BorrowMut;
use std::fs;
use std::io;
use std::path::PathBuf;

use clap::CommandFactory;
use clap_complete::{Shell, generate_to};
use clap_complete_fig::Fig;
use clap_complete_nushell::Nushell;
use clap_mangen::Man;

use satty_cli::command_line;

fn main() -> Result<(), io::Error> {
    let out_dir =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").ok_or(std::io::ErrorKind::NotFound)?);
    let mut cmd = command_line::CommandLine::command();
    let cmd2 = cmd.borrow_mut();
    let bin = "satty";
    let completions = if cfg!(feature = "ci-release") {
        PathBuf::from("completions")
    } else {
        // make cargo publish happy about OUT_DIR ;)
        out_dir.join(PathBuf::from("completions"))
    };

    fs::create_dir_all(completions.as_path())?;
    generate_to(Shell::Bash, cmd2, bin, &completions)?;
    generate_to(Shell::Fish, cmd2, bin, &completions)?;
    generate_to(Shell::Zsh, cmd2, bin, &completions)?;
    generate_to(Shell::Elvish, cmd2, bin, &completions)?;
    generate_to(Nushell, cmd2, bin, &completions)?;
    generate_to(Fig, cmd2, bin, &completions)?;

    let man = Man::new(cmd);
    let mut buffer: Vec<u8> = Default::default();
    man.title(bin).render(&mut buffer)?;
    if cfg!(feature = "ci-release") {
        let man_dir = PathBuf::from("man");
        fs::create_dir_all(man_dir.as_path())?;
        std::fs::write(man_dir.join(format!("{}.1", bin)), buffer.clone())?;
    }
    std::fs::write(out_dir.join(format!("{}.1", bin)), buffer)?;

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
            "tetris-app-regular",
        ],
    );

    Ok(())
}
