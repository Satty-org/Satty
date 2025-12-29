//! Per-request configuration for daemon mode
//!
//! When running in daemon mode, each window can have its own configuration
//! derived from the DaemonRequest. This replaces the global APP_CONFIG for
//! per-request overrides while still using defaults from the config file.

use crate::configuration::{Action, APP_CONFIG};
use crate::tools::Tools;

use super::protocol::DaemonRequest;

/// Configuration for a single daemon request/window
///
/// This contains all the configurable options that can be overridden per-request.
/// Options are merged with the global configuration: request values take precedence.
#[derive(Debug, Clone)]
pub struct RequestConfig {
    pub input_filename: String,
    pub output_filename: Option<String>,
    pub copy_command: Option<String>,
    pub initial_tool: Tools,
    pub fullscreen: bool,
    pub early_exit: bool,
    pub corner_roundness: f32,
    pub annotation_size_factor: f32,
    pub default_hide_toolbars: bool,
    pub no_window_decoration: bool,
    pub focus_toggles_toolbars: bool,
    pub actions_on_enter: Vec<Action>,
    pub actions_on_escape: Vec<Action>,
    pub actions_on_right_click: Vec<Action>,
}

impl RequestConfig {
    /// Create a RequestConfig from a DaemonRequest, merging with global config
    pub fn from_request(request: &DaemonRequest) -> Self {
        let global = APP_CONFIG.read();

        Self {
            input_filename: request.filename.clone(),
            output_filename: request
                .output_filename
                .clone()
                .or_else(|| global.output_filename().cloned()),
            copy_command: request
                .copy_command
                .clone()
                .or_else(|| global.copy_command().cloned()),
            initial_tool: request
                .initial_tool
                .as_ref()
                .and_then(|s| parse_tool(s))
                .unwrap_or_else(|| global.initial_tool()),
            fullscreen: request.fullscreen.unwrap_or_else(|| global.fullscreen()),
            early_exit: request.early_exit.unwrap_or_else(|| global.early_exit()),
            corner_roundness: request
                .corner_roundness
                .unwrap_or_else(|| global.corner_roundness()),
            annotation_size_factor: request
                .annotation_size_factor
                .unwrap_or_else(|| global.annotation_size_factor()),
            default_hide_toolbars: request
                .default_hide_toolbars
                .unwrap_or_else(|| global.default_hide_toolbars()),
            no_window_decoration: request
                .no_window_decoration
                .unwrap_or_else(|| global.no_window_decoration()),
            focus_toggles_toolbars: global.focus_toggles_toolbars(),
            actions_on_enter: global.actions_on_enter(),
            actions_on_escape: global.actions_on_escape(),
            actions_on_right_click: global.actions_on_right_click(),
        }
    }

    /// Create a RequestConfig from the global configuration
    /// Used for non-daemon mode or testing
    pub fn from_global() -> Self {
        let global = APP_CONFIG.read();

        Self {
            input_filename: global.input_filename().to_string(),
            output_filename: global.output_filename().cloned(),
            copy_command: global.copy_command().cloned(),
            initial_tool: global.initial_tool(),
            fullscreen: global.fullscreen(),
            early_exit: global.early_exit(),
            corner_roundness: global.corner_roundness(),
            annotation_size_factor: global.annotation_size_factor(),
            default_hide_toolbars: global.default_hide_toolbars(),
            no_window_decoration: global.no_window_decoration(),
            focus_toggles_toolbars: global.focus_toggles_toolbars(),
            actions_on_enter: global.actions_on_enter(),
            actions_on_escape: global.actions_on_escape(),
            actions_on_right_click: global.actions_on_right_click(),
        }
    }
}

/// Parse a tool name from a string
fn parse_tool(s: &str) -> Option<Tools> {
    match s.to_lowercase().as_str() {
        "pointer" => Some(Tools::Pointer),
        "crop" => Some(Tools::Crop),
        "line" => Some(Tools::Line),
        "arrow" => Some(Tools::Arrow),
        "rectangle" => Some(Tools::Rectangle),
        "ellipse" => Some(Tools::Ellipse),
        "text" => Some(Tools::Text),
        "marker" => Some(Tools::Marker),
        "blur" => Some(Tools::Blur),
        "highlight" => Some(Tools::Highlight),
        "brush" => Some(Tools::Brush),
        _ => None,
    }
}

impl Default for RequestConfig {
    fn default() -> Self {
        Self {
            input_filename: String::new(),
            output_filename: None,
            copy_command: None,
            initial_tool: Tools::Pointer,
            fullscreen: false,
            early_exit: false,
            corner_roundness: 12.0,
            annotation_size_factor: 1.0,
            default_hide_toolbars: false,
            no_window_decoration: false,
            focus_toggles_toolbars: false,
            actions_on_enter: vec![],
            actions_on_escape: vec![Action::Exit],
            actions_on_right_click: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tool() {
        assert_eq!(parse_tool("pointer"), Some(Tools::Pointer));
        assert_eq!(parse_tool("ARROW"), Some(Tools::Arrow));
        assert_eq!(parse_tool("Rectangle"), Some(Tools::Rectangle));
        assert_eq!(parse_tool("invalid"), None);
    }

    #[test]
    fn test_default_config() {
        let config = RequestConfig::default();
        assert!(config.input_filename.is_empty());
        assert_eq!(config.initial_tool, Tools::Pointer);
        assert!(!config.fullscreen);
    }
}
