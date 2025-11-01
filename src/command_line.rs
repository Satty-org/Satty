use clap::{Parser, ValueEnum};
use serde::Deserialize;
use std::str::FromStr;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct CommandLine {
    /// Path to the config file. Otherwise will be read from XDG_CONFIG_DIR/satty/config.toml
    #[arg(short, long)]
    pub config: Option<String>,

    /// Path to input image or '-' to read from stdin
    #[arg(short, long)]
    pub filename: String,

    /// Start Satty in fullscreen mode. Since NEXTRELEASE, takes optional parameter.
    /// --fullscreen without parameter is equivalent to --fullscreen current.
    /// Mileage may vary depending on compositor.
    #[arg(long, num_args = 0..=1, default_missing_value = "current-screen", value_enum)]
    pub fullscreen: Option<Fullscreen>,

    /// Resize to coordinates or use smart mode (NEXTRELEASE).
    /// --resize without parameter is equivalent to --resize smart
    /// [possible values: smart, WxH.]
    #[arg(long, num_args=0..=1, value_name="MODE|WIDTHxHEIGHT", default_missing_value = "smart", value_parser = Resize::from_str)]
    pub resize: Option<Resize>,

    /// Try to enforce floating (NEXTRELEASE).
    /// Mileage may vary depending on compositor.
    #[arg(long)]
    pub floating_hack: bool,

    /// Filename to use for saving action or '-' to print to stdout. Omit to disable saving to file. Might contain format
    /// specifiers: <https://docs.rs/chrono/latest/chrono/format/strftime/index.html>.
    /// Since 0.20.0, can contain tilde (~) for home dir
    #[arg(short, long)]
    pub output_filename: Option<String>,

    /// Exit directly after copy/save action
    #[arg(long)]
    pub early_exit: bool,

    /// Draw corners of rectangles round if the value is greater than 0
    /// (Defaults to 12) (0 disables rounded corners)
    #[arg(long)]
    pub corner_roundness: Option<f32>,

    /// Select the tool on startup
    #[arg(long, value_name = "TOOL", visible_alias = "init-tool")]
    pub initial_tool: Option<Tools>,

    /// Configure the command to be called on copy, for example `wl-copy`
    #[arg(long)]
    pub copy_command: Option<String>,

    /// Increase or decrease the size of the annotations
    #[arg(long)]
    pub annotation_size_factor: Option<f32>,

    /// After copying the screenshot, save it to a file as well
    /// Preferably use the `action_on_copy` option instead.
    #[arg(long)]
    pub save_after_copy: bool,

    /// Actions to perform when pressing Enter
    #[arg(long, value_delimiter = ',')]
    pub actions_on_enter: Option<Vec<Action>>,

    /// Actions to perform when pressing Escape
    #[arg(long, value_delimiter = ',')]
    pub actions_on_escape: Option<Vec<Action>>,

    /// Actions to perform when hitting the copy Button.
    #[arg(long, value_delimiter = ',')]
    pub actions_on_right_click: Option<Vec<Action>>,

    /// Hide toolbars by default
    #[arg(short, long)]
    pub default_hide_toolbars: bool,

    /// Experimental (since 0.20.0): Whether to toggle toolbars based on focus. Doesn't affect initial state.
    #[arg(long)]
    pub focus_toggles_toolbars: bool,

    /// Experimental feature (since 0.20.0): Fill shapes by default
    #[arg(long)]
    pub default_fill_shapes: bool,

    /// Font family to use for text annotations
    #[arg(long)]
    pub font_family: Option<String>,

    /// Font style to use for text annotations
    #[arg(long)]
    pub font_style: Option<String>,

    /// The primary highlighter to use, secondary is accessible with CTRL
    #[arg(long)]
    pub primary_highlighter: Option<Highlighters>,

    /// Disable notifications
    #[arg(long)]
    pub disable_notifications: bool,

    /// Print profiling
    #[arg(long)]
    pub profile_startup: bool,

    /// Disable the window decoration (title bar, borders, etc.)
    /// Please note that the compositor has the final say in this.
    /// Requires xdg-decoration-unstable-v1
    #[arg(long)]
    pub no_window_decoration: bool,

    /// Experimental feature: How many points to use for the brush smoothing
    /// algorithm.
    /// 0 disables smoothing.
    /// The default value is 0 (disabled).
    #[arg(long)]
    pub brush_smooth_history_size: Option<usize>,

    // --- deprecated options ---
    /// Right click to copy.
    /// Preferably use the `action_on_right_click` option instead.
    #[arg(long)]
    pub right_click_copy: bool,
    /// Action to perform when pressing Enter.
    /// Preferably use the `actions_on_enter` option instead.
    #[arg(long, value_delimiter = ',')]
    pub action_on_enter: Option<Action>,
    // ---
}

#[derive(Debug, Deserialize, Clone, Copy, ValueEnum, PartialEq)]
#[value(rename_all = "kebab-case")]
#[serde(rename_all = "kebab-case")]
pub enum Fullscreen {
    All,
    CurrentScreen,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "kebab-case", tag = "mode")]
pub enum Resize {
    Size { width: i32, height: i32 },
    Smart,
}

impl FromStr for Resize {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim().to_lowercase();
        match s.as_str() {
            "smart" => Ok(Resize::Smart),
            _ => {
                let (w, h) = s.split_once('x').ok_or("Expected size=WxH")?;
                let w: i32 = w.parse().map_err(|_| "Invalid width".to_string())?;
                let h: i32 = h.parse().map_err(|_| "Invalid height".to_string())?;
                Ok(Resize::Size {
                    width: w,
                    height: h,
                })
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum Tools {
    #[default]
    Pointer,
    Crop,
    Line,
    Arrow,
    Rectangle,
    Ellipse,
    Text,
    Marker,
    Blur,
    Highlight,
    Brush,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Action {
    SaveToClipboard,
    SaveToFile,
    Exit,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum Highlighters {
    #[default]
    Block,
    Freehand,
}

impl std::fmt::Display for Tools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use Tools::*;
        let s = match self {
            Pointer => "pointer",
            Crop => "crop",
            Line => "line",
            Arrow => "arrow",
            Rectangle => "rectangle",
            Ellipse => "ellipse",
            Text => "text",
            Marker => "marker",
            Blur => "blur",
            Highlight => "highlight",
            Brush => "brush",
        };
        f.write_str(s)
    }
}
