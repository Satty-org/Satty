use clap::Parser;
use hex_color::HexColor;
use relm4::SharedState;
use serde::Deserialize;
use serde::de::Deserializer;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use std::{collections::HashMap, fs, io, path::Path};
use thiserror::Error;
use xdg::{BaseDirectories, BaseDirectoriesError};

use crate::logging::log;
use crate::{
    style::{Color, Size, Style, StyleField, StyleFields},
    tools::Tools,
};

use satty_cli::command_line::{
    Action as CommandLineAction, CommandLine, EarlyExitTriggers, Fullscreen, Highlighters,
    LogLevel, NotificationThumbnail, Resize,
};

pub static APP_CONFIG: SharedState<Configuration> = SharedState::new();

// independent of APP_CONFIG so it can be used while the config lock is held
static CONFIG_ERROR_COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn log_config_error(message: impl std::fmt::Display) {
    CONFIG_ERROR_COUNT.fetch_add(1, Ordering::Relaxed);
    log(&format!("{message}"), LogLevel::Warn);
}

pub fn config_error_count() -> usize {
    CONFIG_ERROR_COUNT.load(Ordering::Relaxed)
}

#[derive(Error, Debug)]
enum ConfigurationFileError {
    #[error("XDG context error: {0}")]
    Xdg(#[from] BaseDirectoriesError),

    #[error("Error reading file: {0}")]
    ReadFile(#[from] io::Error),

    #[error("Decoding toml failed: {0}")]
    TomlDecoding(#[from] toml::de::Error),
}

pub enum Input {
    Stdin,
    File(String),
    ClipboardCommand(String),
}

pub struct Configuration {
    man: bool,
    license: bool,
    input: Option<Input>,
    output_filename: Option<String>,
    log_level: LogLevel,
    fullscreen: Option<Fullscreen>,
    resize: Option<Resize>,
    floating_hack: bool,
    early_exit: EarlyExit,
    initial_tool: Tools,
    copy_command: Option<String>,
    annotation_size_factor: f32,
    save_after_copy: bool,
    auto_copy: bool,
    auto_select: bool,
    actions_on_enter: Vec<Action>,
    actions_on_escape: Vec<Action>,
    actions_on_right_click: Vec<Action>,
    color_palette: ColorPalette,
    default_hide_toolbars: bool,
    focus_toggles_toolbars: bool,
    style: StyleFile,
    tool_styles: HashMap<Tools, StyleFile>,
    font: FontConfiguration,
    aspect_ratios: Vec<(f32, f32)>,
    disable_notifications: bool,
    no_window_decoration: bool,
    brush_smooth_history_size: usize,
    keybinds: HashMap<String, String>, // key_binding -> tool_or_command
    zoom_factor: f32,
    pan_step_size: f32,
    text_move_length: f32,
    input_scale: Option<f32>,
    title: Option<String>,
    app_id: Option<String>,
    notification_thumbnail: NotificationThumbnail,
    notification_grace_period: Duration,
}

#[derive(Default)]
pub struct FontConfiguration {
    family: Option<String>,
    style: Option<String>,
    fallback: Vec<String>,
}

impl FontConfiguration {
    pub fn family(&self) -> Option<&str> {
        self.family.as_deref()
    }

    pub fn style(&self) -> Option<&str> {
        self.style.as_deref()
    }

    pub fn fallback(&self) -> &[String] {
        &self.fallback
    }

    fn merge(&mut self, file_font: FontFile) {
        if let Some(v) = file_font.family {
            self.family = Some(v);
        }
        if let Some(v) = file_font.style {
            self.style = Some(v);
        }
        if let Some(v) = file_font.fallback {
            self.fallback.extend(v);
        }
    }
}

pub struct ColorPalette {
    palette: Vec<Color>,
    custom: Vec<Color>,
}

impl ColorPalette {
    pub fn palette(&self) -> &[Color] {
        &self.palette
    }

    pub fn custom(&self) -> &[Color] {
        &self.custom
    }

    fn merge(&mut self, file_palette: ColorPaletteFile) {
        if let Some(v) = file_palette.palette {
            self.palette = v.into_iter().map(Color::from).collect();
        }
        if let Some(v) = file_palette.custom {
            self.custom = v.into_iter().map(Color::from).collect();
        }
    }
}

// remain compatible with old config with fullscreen=true/false
#[derive(Deserialize)]
#[serde(untagged)]
#[serde(rename_all = "kebab-case")]
enum FullscreenCompat {
    Bool(bool),
    Mode(Fullscreen),
}

fn de_fullscreen_mode<'de, D>(d: D) -> Result<Option<Fullscreen>, D::Error>
where
    D: Deserializer<'de>,
{
    match FullscreenCompat::deserialize(d)? {
        FullscreenCompat::Bool(true) => Ok(Some(Fullscreen::CurrentScreen)),
        FullscreenCompat::Bool(false) => Ok(None),
        FullscreenCompat::Mode(m) => Ok(Some(m)),
    }
}

// backwards compatibility, allow early-exit = true in config
#[derive(Deserialize)]
#[serde(untagged)]
#[serde(rename_all = "kebab-case")]
enum EarlyExitCompat {
    Bool(bool),
    Triggers(Vec<EarlyExitTriggers>),
}

fn de_early_exit<'de, D>(d: D) -> Result<Option<Vec<EarlyExitTriggers>>, D::Error>
where
    D: Deserializer<'de>,
{
    match EarlyExitCompat::deserialize(d)? {
        EarlyExitCompat::Bool(true) => Ok(Some(vec![EarlyExitTriggers::All])),
        EarlyExitCompat::Bool(false) => Ok(None),
        EarlyExitCompat::Triggers(m) => Ok(Some(m)),
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct EarlyExit {
    copy: bool,
    save: bool,
    save_as: bool,
}

impl EarlyExit {
    pub fn copy(&self) -> bool {
        self.copy
    }
    pub fn save(&self) -> bool {
        self.save
    }
    pub fn save_as(&self) -> bool {
        self.save_as
    }
}

impl From<Vec<EarlyExitTriggers>> for EarlyExit {
    fn from(val: Vec<EarlyExitTriggers>) -> Self {
        let mut ee = EarlyExit::default();
        for e in val {
            match e {
                EarlyExitTriggers::All => {
                    ee.copy = true;
                    ee.save = true;
                    ee.save_as = true;
                }
                EarlyExitTriggers::Copy => {
                    ee.copy = true;
                }
                EarlyExitTriggers::Save => {
                    ee.save = true;
                }
                EarlyExitTriggers::SaveAs => {
                    ee.save_as = true;
                }
            }
        }
        ee
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    SaveToClipboard,
    SaveToFile,
    SaveToFileAs,
    CopyFilepathToClipboard,
    Exit,
}

impl From<CommandLineAction> for Action {
    fn from(action: CommandLineAction) -> Self {
        match action {
            CommandLineAction::SaveToClipboard => Self::SaveToClipboard,
            CommandLineAction::SaveToFile => Self::SaveToFile,
            CommandLineAction::SaveToFileAs => Self::SaveToFileAs,
            CommandLineAction::CopyFilepathToClipboard => Self::CopyFilepathToClipboard,
            CommandLineAction::Exit => Self::Exit,
        }
    }
}

impl Configuration {
    pub fn load() {
        // parse commandline options and exit if error
        let command_line = match CommandLine::try_parse() {
            Ok(cmd) => cmd,
            Err(e) => e.exit(),
        };

        // read configuration file and exit on error
        let files = match ConfigurationFile::try_read(&command_line.config) {
            Ok(c) => c,
            Err(e) => {
                log_config_error(format!("Error reading given config file: {e}"));
                vec![]
            }
        };

        APP_CONFIG.write().merge(files, command_line);
    }

    fn merge_general(&mut self, general: ConfigurationFileGeneral) {
        if let Some(v) = general.fullscreen {
            self.fullscreen = Some(v);
        }
        if let Some(v) = general.resize {
            self.resize = Some(v);
        }
        if let Some(v) = general.floating_hack {
            self.floating_hack = v;
        }
        if let Some(v) = general.early_exit {
            self.early_exit = v.into();
        }
        if let Some(v) = general.initial_tool {
            self.initial_tool = v;
        }
        if let Some(v) = general.copy_command {
            self.copy_command = Some(v);
        }
        if let Some(v) = general.output_filename {
            self.output_filename = Some(v);
        }
        if let Some(v) = general.log_level {
            self.log_level = v
        }
        if let Some(v) = general.annotation_size_factor {
            self.annotation_size_factor = v;
        }
        if let Some(v) = general.save_after_copy {
            self.save_after_copy = v;
        }
        if let Some(v) = general.auto_copy {
            self.auto_copy = v;
        }
        if let Some(v) = general.auto_select_new {
            self.auto_select = v;
        }
        if let Some(v) = general.actions_on_enter {
            self.actions_on_enter = v;
        }
        if let Some(v) = general.actions_on_escape {
            self.actions_on_escape = v;
        }
        if let Some(v) = general.actions_on_right_click {
            self.actions_on_right_click = v;
        }
        if let Some(v) = general.default_hide_toolbars {
            self.default_hide_toolbars = v;
        }
        if let Some(v) = general.focus_toggles_toolbars {
            self.focus_toggles_toolbars = v;
        }
        if let Some(v) = general.aspect_ratios {
            self.aspect_ratios = v;
        }
        if let Some(v) = general.disable_notifications {
            self.disable_notifications = v;
        }
        if let Some(v) = general.no_window_decoration {
            self.no_window_decoration = v;
        }
        if let Some(v) = general.brush_smooth_history_size {
            self.brush_smooth_history_size = v;
        }
        if let Some(v) = general.zoom_factor {
            self.zoom_factor = v;
        }
        if let Some(v) = general.pan_step_size {
            self.pan_step_size = v;
        }
        if let Some(v) = general.text_move_length {
            self.text_move_length = v;
        }
        if let Some(v) = general.input_scale {
            self.input_scale = Some(v);
        }
        if let Some(v) = general.title {
            self.title = Some(v);
        }
        if let Some(v) = general.app_id {
            self.app_id = Some(v);
        }
        if let Some(v) = general.notification_thumbnail {
            self.notification_thumbnail = v;
        }
        if let Some(v) = general.notification_grace_period {
            self.notification_grace_period = std::time::Duration::from_millis(v);
        }

        // --- deprecated options ---
        if let Some(v) = general.right_click_copy
            && v
            && !self
                .actions_on_right_click
                .contains(&Action::SaveToClipboard)
        {
            log_config_error(
                "Deprecated config option: right-click-copy, use actions-on-right-click instead",
            );
            self.actions_on_right_click
                .insert(0, Action::SaveToClipboard);
        }
        if let Some(v) = general.action_on_enter {
            log_config_error(
                "Deprecated config option: action-on-enter, use actions-on-enter instead",
            );
            self.actions_on_enter.insert(0, v);
        }
        // ---
    }

    fn merge_style(&mut self, mut style: StyleFile) {
        for (key, value) in std::mem::take(&mut style.tools) {
            // the names of the tools as in the keybinds, like spotlight
            let tool = match key.parse::<Tools>() {
                Ok(tool) => tool,
                Err(_) => {
                    log_config_error(format!("Unknown field in config [style]: {key}"));
                    continue;
                }
            };
            match StyleFile::deserialize(value) {
                Ok(tool_style) => {
                    for nested in tool_style.tools.keys() {
                        log_config_error(format!(
                            "Unknown field in config [style.{key}]: {nested}"
                        ));
                    }
                    self.tool_styles.entry(tool).or_default().merge(tool_style);
                }
                Err(e) => log_config_error(format!("Invalid config [style.{key}]: {e}")),
            }
        }
        self.style.merge(style);
    }

    fn merge(&mut self, files: Vec<ConfigurationFile>, command_line: CommandLine) {
        // input is required and needs to be overwritten
        self.input = if let Some(f) = command_line.filename {
            match f.as_str() {
                "-" => Some(Input::Stdin),
                _ => Some(Input::File(f)),
            }
        } else {
            command_line.from_clipboard.map(Input::ClipboardCommand)
        };

        // overwrite with all specified values from config file
        for file in files {
            if let Some(general) = file.general {
                self.merge_general(general);
            }
            if let Some(v) = file.color_palette {
                self.color_palette.merge(v);
            }
            if let Some(v) = file.style {
                self.merge_style(v);
            }
            if let Some(v) = file.font {
                self.font.merge(v);
            }
            if let Some(v) = file.keybinds {
                self.keybinds.extend(v);
            }
        }

        let styles = std::iter::once(("general".to_string(), &self.style)).chain(
            self.tool_styles
                .iter()
                .map(|(tool, style)| (tool.to_string(), style)),
        );
        for (name, style) in styles {
            if let Some(color @ ColorSpec::Index(index)) = &style.color
                && color.resolve(&self.color_palette).is_none()
            {
                log_config_error(format!(
                    "Invalid color index in config [style] for {name}: {index}, the palette has {} colors",
                    self.color_palette.palette().len()
                ));
            }
        }

        // overwrite with all specified values from command line
        if let Some(v) = command_line.fullscreen {
            self.fullscreen = Some(v);
        }
        if let Some(v) = command_line.resize {
            self.resize = Some(v);
        }
        if command_line.floating_hack {
            self.floating_hack = command_line.floating_hack;
        }
        if command_line.man {
            self.man = command_line.man;
        }
        if command_line.license {
            self.license = command_line.license;
        }
        if let Some(v) = command_line.early_exit {
            self.early_exit = v.into();
        }
        if let Some(v) = command_line.corner_radius {
            self.style.corner_radius = Some(v);
        }
        if command_line.default_hide_toolbars {
            self.default_hide_toolbars = command_line.default_hide_toolbars;
        }
        if command_line.focus_toggles_toolbars {
            self.focus_toggles_toolbars = command_line.focus_toggles_toolbars
        }
        if command_line.default_fill_shapes {
            self.style.fill = Some(true);
        }
        if let Some(v) = command_line.initial_tool {
            self.initial_tool = v.into();
        }
        if let Some(v) = command_line.copy_command {
            self.copy_command = Some(v);
        }
        if let Some(v) = command_line.output_filename {
            self.output_filename = Some(v);
        }
        if let Some(v) = command_line.log_level {
            self.log_level = v;
        }
        if let Some(v) = command_line.annotation_size_factor {
            self.annotation_size_factor = v;
        }
        if command_line.save_after_copy {
            self.save_after_copy = command_line.save_after_copy;
        }
        if command_line.auto_copy {
            self.auto_copy = command_line.auto_copy;
        }
        if let Some(v) = command_line.actions_on_enter {
            self.actions_on_enter = v.iter().cloned().map(Into::into).collect();
        }
        if let Some(v) = command_line.actions_on_escape {
            self.actions_on_escape = v.iter().cloned().map(Into::into).collect();
        }
        if let Some(v) = command_line.actions_on_right_click {
            self.actions_on_right_click = v.iter().cloned().map(Into::into).collect();
        }
        if let Some(v) = command_line.font_family {
            self.font.family = Some(v);
        }
        if let Some(v) = command_line.font_style {
            self.font.style = Some(v);
        }
        if command_line.disable_notifications {
            self.disable_notifications = command_line.disable_notifications;
        }
        if command_line.profile_startup && self.log_level < LogLevel::Debug {
            self.log_level = LogLevel::Debug;
        }
        if command_line.no_window_decoration {
            self.no_window_decoration = command_line.no_window_decoration;
        }
        if let Some(v) = command_line.brush_smooth_history_size {
            self.brush_smooth_history_size = v;
        }
        if let Some(v) = command_line.zoom_factor {
            self.zoom_factor = v;
        }
        if let Some(v) = command_line.pan_step_size {
            self.pan_step_size = v;
        }
        if let Some(v) = command_line.text_move_length {
            self.text_move_length = v;
        }
        if let Some(v) = command_line.input_scale {
            self.input_scale = Some(v);
        }
        if let Some(v) = command_line.title {
            self.title = Some(v);
        }
        if let Some(v) = command_line.app_id {
            self.app_id = Some(v);
        }
        if let Some(v) = command_line.notification_thumbnail {
            self.notification_thumbnail = v;
        }

        // --- deprecated options ---
        if command_line.right_click_copy
            && !self
                .actions_on_right_click
                .contains(&Action::SaveToClipboard)
        {
            log_config_error(
                "Deprecated command line option: right-click-copy, use actions-on-right-click instead",
            );
            self.actions_on_right_click
                .insert(0, Action::SaveToClipboard);
        }
        if let Some(v) = command_line.action_on_enter {
            log_config_error(
                "Deprecated command line option: action-on-enter, use actions-on-enter instead",
            );
            self.actions_on_enter.insert(0, v.into());
        }
        // ---
    }

    pub fn man(&self) -> bool {
        self.man
    }

    pub fn license(&self) -> bool {
        self.license
    }

    pub fn early_exit_copy(&self) -> bool {
        self.early_exit.copy()
    }

    pub fn early_exit_save(&self) -> bool {
        self.early_exit.save()
    }

    pub fn early_exit_save_as(&self) -> bool {
        self.early_exit.save_as()
    }

    pub fn initial_tool(&self) -> Tools {
        self.initial_tool
    }

    pub fn copy_command(&self) -> Option<&String> {
        self.copy_command.as_ref()
    }

    pub fn fullscreen(&self) -> Option<Fullscreen> {
        self.fullscreen
    }

    pub fn resize(&self) -> Option<Resize> {
        self.resize
    }

    pub fn floating_hack(&self) -> bool {
        self.floating_hack
    }

    pub fn output_filename(&self) -> Option<&String> {
        self.output_filename.as_ref()
    }

    pub fn log_level(&self) -> LogLevel {
        self.log_level
    }

    pub fn input(&self) -> Option<&Input> {
        self.input.as_ref()
    }

    pub fn save_after_copy(&self) -> bool {
        self.save_after_copy
    }

    pub fn auto_copy(&self) -> bool {
        self.auto_copy
    }

    pub fn auto_select(&self) -> bool {
        self.auto_select
    }

    pub fn actions_on_enter(&self) -> Vec<Action> {
        self.actions_on_enter.clone()
    }

    pub fn actions_on_escape(&self) -> Vec<Action> {
        self.actions_on_escape.clone()
    }

    pub fn actions_on_right_click(&self) -> Vec<Action> {
        self.actions_on_right_click.clone()
    }

    /// The general style, used for tools without their own style.
    pub fn style(&self) -> Style {
        self.resolve_style(None)
    }

    /// The style of `tool`, falling back per field to the built-in defaults of
    /// the tool and then to the general style.
    pub fn style_for(&self, tool: Tools) -> Style {
        self.resolve_style(Some(tool))
    }

    /// The fields `tool` has itself, set in its [style.TOOL] or built in;
    /// the others it shares with the other tools through the general style.
    pub fn own_style_fields(&self, tool: Tools) -> StyleFields {
        let tool_style = self.tool_styles.get(&tool);
        let has = |field: fn(&StyleFile) -> bool| tool_style.is_some_and(field);
        let mut own = StyleFields::default();
        own.set(
            StyleField::Color,
            builtin_color(tool).is_some() || has(|s| s.color.is_some()),
        );
        own.set(
            StyleField::Size,
            builtin_size_presets(tool).is_some()
                || has(|s| {
                    s.size.is_some() || s.small.is_some() || s.medium.is_some() || s.large.is_some()
                }),
        );
        own.set(
            StyleField::Fill,
            builtin_fill(tool).is_some() || has(|s| s.fill.is_some()),
        );
        own.set(
            StyleField::RoundCaps,
            builtin_round_caps(tool).is_some() || has(|s| s.round_caps.is_some()),
        );
        own.set(StyleField::CornerRadius, has(|s| s.corner_radius.is_some()));
        own.set(
            StyleField::Opacity,
            builtin_opacity(tool).is_some() || has(|s| s.opacity.is_some()),
        );
        own
    }

    fn resolve_style(&self, tool: Option<Tools>) -> Style {
        let general = &self.style;
        let tool_style = tool.and_then(|t| self.tool_styles.get(&t));
        let builtin = tool.and_then(builtin_size_presets);
        let pick = |field: fn(&StyleFile) -> Option<f32>, builtin: Option<f32>| {
            tool_style
                .and_then(field)
                .or(builtin)
                .or_else(|| field(general))
        };

        // the annotation size factor scales only line widths and font sizes
        // the general size is used only by tools with line widths
        let factor = if tool.is_none_or(scales_with_annotation_size_factor) {
            self.annotation_size_factor
        } else {
            1.0
        };
        let presets = [
            pick(|s| s.small, builtin.map(|b| b[0])).unwrap_or(3.0),
            pick(|s| s.medium, builtin.map(|b| b[1])).unwrap_or(5.0),
            pick(|s| s.large, builtin.map(|b| b[2])).unwrap_or(7.0),
        ]
        .map(|v| Size::new(v * factor));
        // tools with built-in sizes ignore the general size
        let size = tool_style
            .and_then(|s| s.size)
            .or_else(|| builtin.is_none().then_some(general.size).flatten())
            .map(|v| Size::new(v * factor))
            .unwrap_or(presets[1]);

        let resolve = |s: &StyleFile| {
            s.color
                .as_ref()
                .and_then(|c| c.resolve(&self.color_palette))
        };
        let color = tool_style
            .and_then(resolve)
            .or(tool.and_then(builtin_color))
            .or_else(|| resolve(general))
            // not Color::default(), it would lock APP_CONFIG again
            .or_else(|| self.color_palette.palette().first().copied())
            .unwrap_or(Color::red());

        Style {
            color,
            size,
            fill: tool_style
                .and_then(|s| s.fill)
                .or(tool.and_then(builtin_fill))
                .or(general.fill)
                .unwrap_or(false),
            round_caps: tool_style
                .and_then(|s| s.round_caps)
                .or(tool.and_then(builtin_round_caps))
                .or(general.round_caps)
                .unwrap_or(true),
            corner_radius: tool_style
                .and_then(|s| s.corner_radius)
                .or(general.corner_radius)
                .unwrap_or(12.0),
            opacity: tool_style
                .and_then(|s| s.opacity)
                .or(tool.and_then(builtin_opacity))
                .or(general.opacity)
                .unwrap_or(100.0)
                .clamp(0.0, 100.0),
            presets,
        }
    }

    pub fn color_palette(&self) -> &ColorPalette {
        &self.color_palette
    }

    pub fn default_hide_toolbars(&self) -> bool {
        self.default_hide_toolbars
    }

    pub fn focus_toggles_toolbars(&self) -> bool {
        self.focus_toggles_toolbars
    }

    pub fn aspect_ratios(&self) -> &[(f32, f32)] {
        &self.aspect_ratios
    }

    pub fn disable_notifications(&self) -> bool {
        self.disable_notifications
    }

    pub fn no_window_decoration(&self) -> bool {
        self.no_window_decoration
    }

    pub fn font(&self) -> &FontConfiguration {
        &self.font
    }

    pub fn brush_smooth_history_size(&self) -> usize {
        self.brush_smooth_history_size
    }

    pub fn keybinds(&self) -> &HashMap<String, String> {
        &self.keybinds
    }

    pub fn zoom_factor(&self) -> f32 {
        self.zoom_factor
    }

    pub fn pan_step_size(&self) -> f32 {
        self.pan_step_size
    }

    pub fn text_move_length(&self) -> f32 {
        self.text_move_length
    }

    pub fn input_scale(&self) -> Option<f32> {
        self.input_scale
    }

    pub fn title(&self) -> Option<&String> {
        self.title.as_ref()
    }

    pub fn app_id(&self) -> Option<&String> {
        self.app_id.as_ref()
    }

    pub fn notification_thumbnail(&self) -> NotificationThumbnail {
        self.notification_thumbnail
    }

    pub fn notification_grace_period(&self) -> Duration {
        self.notification_grace_period
    }
}

impl Default for Configuration {
    fn default() -> Self {
        Self {
            man: false,
            license: false,
            input: None,
            output_filename: None,
            log_level: LogLevel::Info,
            fullscreen: None,
            resize: None,
            floating_hack: false,
            early_exit: EarlyExit::default(),
            initial_tool: Tools::Pointer,
            copy_command: None,
            annotation_size_factor: 1.0,
            save_after_copy: false,
            auto_copy: false,
            auto_select: false,
            actions_on_enter: vec![Action::SaveToClipboard],
            actions_on_escape: vec![Action::Exit],
            actions_on_right_click: vec![],
            color_palette: ColorPalette::default(),
            default_hide_toolbars: false,
            focus_toggles_toolbars: false,
            style: StyleFile::default(),
            tool_styles: HashMap::new(),
            font: FontConfiguration::default(),
            aspect_ratios: vec![
                (1.0, 1.0),
                (5.0, 4.0),
                (4.0, 3.0),
                (7.0, 5.0),
                (3.0, 2.0),
                (16.0, 9.0),
            ],
            disable_notifications: false,
            no_window_decoration: false,
            brush_smooth_history_size: 0, // default to 0, no history
            keybinds: HashMap::new(),
            zoom_factor: 1.1,
            pan_step_size: 50.,
            text_move_length: 50.0,
            input_scale: None,
            title: None,
            app_id: None,
            notification_thumbnail: NotificationThumbnail::default(),
            notification_grace_period: Duration::from_millis(250),
        }
    }
}

impl Default for ColorPalette {
    fn default() -> Self {
        Self {
            palette: vec![
                Color::orange(),
                Color::red(),
                Color::green(),
                Color::blue(),
                Color::cove(),
            ],
            custom: vec![],
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct ConfigurationFile {
    general: Option<ConfigurationFileGeneral>,
    color_palette: Option<ColorPaletteFile>,
    style: Option<StyleFile>,
    font: Option<FontFile>,
    keybinds: Option<HashMap<String, String>>,

    // collect unknown fields
    #[serde(flatten)]
    pub unknown_fields: HashMap<String, toml::Value>,
}

/// Built-in small, medium and large sizes for tools where the size does not
/// mean a line width; all others use the general style.
fn builtin_size_presets(tool: Tools) -> Option<[f32; 3]> {
    match tool {
        Tools::Highlighter => Some([15.0, 30.0, 45.0]),
        Tools::Text | Tools::Marker => Some([36.0, 54.0, 96.0]),
        Tools::Pixelate | Tools::FringePixelate => Some([4.0, 8.0, 16.0]),
        Tools::Blur | Tools::Lensblur => Some([4.0, 8.0, 16.0]),
        Tools::BlockHighlighter | Tools::HighlightSpotlight => Some([40.0, 60.0, 80.0]),
        _ => None,
    }
}

/// Tools whose size is a line width or font size.
fn scales_with_annotation_size_factor(tool: Tools) -> bool {
    matches!(
        tool,
        Tools::Pen
            | Tools::Brush
            | Tools::Highlighter
            | Tools::Line
            | Tools::Arrow
            | Tools::Rectangle
            | Tools::Ellipse
            | Tools::Text
            | Tools::Marker
    )
}

/// Built-in opacity in percent, the freehand highlighter is translucent.
fn builtin_opacity(tool: Tools) -> Option<f32> {
    match tool {
        Tools::Highlighter => Some(40.0),
        // text uses it for its background
        Tools::Text => Some(0.0),
        _ => None,
    }
}

/// Built-in color, a blackout is black.
fn builtin_color(tool: Tools) -> Option<Color> {
    (tool == Tools::Blackout).then(|| Color::new(0, 0, 0, 255))
}

/// Built-in fill for tools where fill means a rectangle instead of an ellipse.
fn builtin_fill(tool: Tools) -> Option<bool> {
    match tool {
        Tools::Blur | Tools::BlockHighlighter | Tools::Blackout => Some(true),
        // fill and round caps select the text effect, by default none
        Tools::Text => Some(false),
        _ => None,
    }
}

/// Built-in round caps, see builtin_fill.
fn builtin_round_caps(tool: Tools) -> Option<bool> {
    (tool == Tools::Text).then_some(true)
}

/// A color in the config, either a 1-based index into the palette or a hex color.
#[derive(Deserialize, Clone, Debug)]
#[serde(untagged)]
enum ColorSpec {
    Index(usize),
    Hex(HexColor),
}

impl ColorSpec {
    fn resolve(&self, palette: &ColorPalette) -> Option<Color> {
        match self {
            ColorSpec::Hex(hex) => Some((*hex).into()),
            ColorSpec::Index(index) => index
                .checked_sub(1)
                .and_then(|i| palette.palette().get(i))
                .copied(),
        }
    }
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "kebab-case")]
struct StyleFile {
    color: Option<ColorSpec>,
    size: Option<f32>,
    fill: Option<bool>,
    round_caps: Option<bool>,
    corner_radius: Option<f32>,
    opacity: Option<f32>,
    small: Option<f32>,
    medium: Option<f32>,
    large: Option<f32>,

    // per tool styles, like [style.arrow], and unknown fields
    #[serde(flatten)]
    tools: HashMap<String, toml::Value>,
}

impl StyleFile {
    fn merge(&mut self, other: StyleFile) {
        macro_rules! take {
            ($($field:ident),*) => {
                $(if other.$field.is_some() {
                    self.$field = other.$field;
                })*
            };
        }
        take!(
            color,
            size,
            fill,
            round_caps,
            corner_radius,
            opacity,
            small,
            medium,
            large
        );
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct FontFile {
    family: Option<String>,
    style: Option<String>,
    fallback: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct ConfigurationFileGeneral {
    #[serde(deserialize_with = "de_fullscreen_mode", default)]
    fullscreen: Option<Fullscreen>,
    resize: Option<Resize>,
    floating_hack: Option<bool>,
    #[serde(deserialize_with = "de_early_exit", default)]
    early_exit: Option<Vec<EarlyExitTriggers>>,
    initial_tool: Option<Tools>,
    copy_command: Option<String>,
    annotation_size_factor: Option<f32>,
    save_after_copy: Option<bool>,
    auto_copy: Option<bool>,
    auto_select_new: Option<bool>,
    output_filename: Option<String>,
    log_level: Option<LogLevel>,
    actions_on_enter: Option<Vec<Action>>,
    actions_on_escape: Option<Vec<Action>>,
    actions_on_right_click: Option<Vec<Action>>,
    default_hide_toolbars: Option<bool>,
    focus_toggles_toolbars: Option<bool>,
    #[allow(dead_code)]
    primary_highlighter: Option<Highlighters>,
    aspect_ratios: Option<Vec<(f32, f32)>>,
    disable_notifications: Option<bool>,
    no_window_decoration: Option<bool>,
    brush_smooth_history_size: Option<usize>,
    zoom_factor: Option<f32>,
    pan_step_size: Option<f32>,
    text_move_length: Option<f32>,
    input_scale: Option<f32>,
    title: Option<String>,
    app_id: Option<String>,
    notification_thumbnail: Option<NotificationThumbnail>,
    notification_grace_period: Option<u64>,

    // --- deprecated options ---
    right_click_copy: Option<bool>,
    action_on_enter: Option<Action>,
    // ---

    // collect unknown fields
    #[serde(flatten)]
    pub unknown_fields: HashMap<String, toml::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct ColorPaletteFile {
    palette: Option<Vec<HexColor>>,
    custom: Option<Vec<HexColor>>,

    // collect unknown fields
    #[serde(flatten)]
    pub unknown_fields: HashMap<String, toml::Value>,
}

impl ConfigurationFile {
    fn try_read(
        specified_path: &Option<String>,
    ) -> Result<Vec<ConfigurationFile>, ConfigurationFileError> {
        if let Some(p) = specified_path {
            return match Self::try_read_path(p)? {
                Some(f) => Ok(vec![f]),
                // _ can't happen with current implementation of try_read_path, any errors (including file not found) are propagated
                _ => Ok(vec![]),
            };
        }

        let mut files = Vec::new();

        let dirs = BaseDirectories::with_prefix(env!("CARGO_PKG_NAME"));
        // only present config files are used
        for c in dirs.find_config_files("config.toml") {
            if let Some(f) = Self::try_read_path(c)? {
                files.push(f);
            }
        }

        Ok(files)
    }

    fn warning_unknown_field(prefix: &str, unknown_fields: &HashMap<String, toml::Value>) {
        for key in unknown_fields.keys() {
            log_config_error(format!("Unknown field in config{}: {}", prefix, key));
        }
    }

    fn try_read_path<P: AsRef<Path>>(
        path: P,
    ) -> Result<Option<ConfigurationFile>, ConfigurationFileError> {
        let path = path.as_ref();
        log(
            &format!("Reading configuration file: {:?}", path),
            LogLevel::Info,
        );
        let config = toml::from_str::<ConfigurationFile>(&fs::read_to_string(path)?)?;

        if !config.unknown_fields.is_empty() {
            Self::warning_unknown_field("", &config.unknown_fields);
        }
        if let Some(ref general) = config.general
            && !general.unknown_fields.is_empty()
        {
            Self::warning_unknown_field(".general", &general.unknown_fields);
        }
        if let Some(ref color_palette) = config.color_palette
            && !color_palette.unknown_fields.is_empty()
        {
            Self::warning_unknown_field(".color-palette", &color_palette.unknown_fields);
        }

        Ok(Some(config))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_style(toml: &str) -> Configuration {
        let mut config = Configuration::default();
        config.merge_style(toml::from_str(toml).unwrap());
        config
    }

    #[test]
    fn builtin_defaults() {
        let config = Configuration::default();
        let line = config.style_for(Tools::Line);
        assert_eq!(line.presets.map(Size::value), [3.0, 5.0, 7.0]);
        assert_eq!(line.size.value(), 5.0);
        assert!(!line.fill);
        assert!(line.round_caps);
        assert_eq!(line.corner_radius, 12.0);
        assert_eq!(line.opacity, 100.0);
        assert_eq!(config.style_for(Tools::Highlighter).opacity, 40.0);
        assert_eq!(config.style_for(Tools::Text).opacity, 0.0);
        assert!(!config.style_for(Tools::Text).fill);
        assert_eq!(line.color, Color::orange());

        assert!(config.style_for(Tools::Blur).fill);
        assert!(config.style_for(Tools::BlockHighlighter).fill);
        assert!(config.style_for(Tools::Blackout).fill);
        assert!(!config.style_for(Tools::Lensblur).fill);
        assert_eq!(
            config.style_for(Tools::Blackout).color,
            Color::new(0, 0, 0, 255)
        );

        let text = config.style_for(Tools::Text);
        assert_eq!(text.presets.map(Size::value), [36.0, 54.0, 96.0]);
        assert_eq!(text.size.value(), 54.0);
    }

    #[test]
    fn tool_overrides_general() {
        let config = config_with_style(
            r##"
            color = 2
            size = 4
            fill = true
            small = 1
            medium = 2
            large = 3

            corner-radius = 8
            opacity = 50

            [rectangle]
            corner-radius = 20
            opacity = 75

            [highlighter]
            size = 40
            color = "#00ff00ff"
            fill = false
            "##,
        );

        let line = config.style_for(Tools::Line);
        assert_eq!(line.color, Color::red());
        assert_eq!(line.size.value(), 4.0);
        assert_eq!(line.presets.map(Size::value), [1.0, 2.0, 3.0]);
        assert!(line.fill);
        assert_eq!(line.corner_radius, 8.0);
        assert_eq!(config.style_for(Tools::Rectangle).corner_radius, 20.0);
        assert_eq!(line.opacity, 50.0);
        // built-in opacity of the tool wins over the general one
        assert_eq!(config.style_for(Tools::Highlighter).opacity, 40.0);
        assert_eq!(config.style_for(Tools::Text).opacity, 0.0);
        assert_eq!(config.style_for(Tools::Rectangle).opacity, 75.0);

        // built-in sizes of the tool win over the general ones
        let highlighter = config.style_for(Tools::Highlighter);
        assert_eq!(highlighter.size.value(), 40.0);
        assert_eq!(highlighter.presets.map(Size::value), [15.0, 30.0, 45.0]);
        assert_eq!(highlighter.color, Color::new(0, 255, 0, 255));
        assert!(!highlighter.fill);

        let marker = config.style_for(Tools::Marker);
        assert_eq!(marker.size.value(), 54.0);
        assert_eq!(marker.color, Color::red());
    }

    #[test]
    fn annotation_size_factor_scales_lines_and_text_only() {
        let mut config = config_with_style(
            r#"
            [line]
            size = 4
            "#,
        );
        config.annotation_size_factor = 2.0;

        let line = config.style_for(Tools::Line);
        assert_eq!(line.presets.map(Size::value), [6.0, 10.0, 14.0]);
        assert_eq!(line.size.value(), 8.0);
        let text = config.style_for(Tools::Text);
        assert_eq!(text.size.value(), 108.0);
        let pixelate = config.style_for(Tools::Pixelate);
        assert_eq!(pixelate.presets.map(Size::value), [4.0, 8.0, 16.0]);
        let highlight = config.style_for(Tools::BlockHighlighter);
        assert_eq!(highlight.size.value(), 60.0);
    }

    #[test]
    fn own_style_fields() {
        let config = config_with_style(
            r#"
            [line]
            color = 2
            small = 1
            "#,
        );
        let line = config.own_style_fields(Tools::Line);
        assert!(line.contains(StyleField::Color));
        assert!(line.contains(StyleField::Size));
        assert!(!line.contains(StyleField::Fill));

        let arrow = config.own_style_fields(Tools::Arrow);
        assert_eq!(arrow, StyleFields::default());

        let text = config.own_style_fields(Tools::Text);
        assert!(!text.contains(StyleField::Color));
        assert!(text.contains(StyleField::Size));
        assert!(text.contains(StyleField::Fill));
        assert!(text.contains(StyleField::RoundCaps));
        assert!(text.contains(StyleField::Opacity));
    }

    #[test]
    fn size_defaults_to_medium() {
        let config = config_with_style(
            r#"
            [pixelate]
            medium = 12.34
            "#,
        );
        let pixelate = config.style_for(Tools::Pixelate);
        assert_eq!(pixelate.size.value(), 12.3);
        assert_eq!(pixelate.presets.map(Size::value), [4.0, 12.3, 16.0]);
    }

    #[test]
    fn example_config_has_no_unknown_fields() {
        let file: ConfigurationFile = toml::from_str(include_str!("../config.toml")).unwrap();
        assert!(file.unknown_fields.is_empty());
        assert!(file.general.unwrap().unknown_fields.is_empty());
        let style = file.style.unwrap();
        for (key, value) in &style.tools {
            key.parse::<Tools>().unwrap();
            let tool_style = StyleFile::deserialize(value.clone()).unwrap();
            assert!(tool_style.tools.is_empty(), "[style.{key}]");
        }
    }

    #[test]
    fn invalid_color_index_falls_back() {
        let config = config_with_style("color = 42");
        assert_eq!(config.style().color, Color::orange());
    }
}
