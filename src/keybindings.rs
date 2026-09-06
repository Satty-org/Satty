use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::sync::OnceLock;

use relm4::gtk;
use relm4::gtk::gdk::prelude::DisplayExtManual;
use relm4::gtk::gdk::{Key, ModifierType};

use crate::configuration::{APP_CONFIG, Action};
use crate::sketch_board::KeyEventMsg;
use crate::style::Size;
use crate::tools::Tools;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionTrigger {
    Escape,
    Enter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutCommand {
    // generic
    ToggleToolbars,
    OpenGtkInspector,
    PanLeft,
    PanRight,
    PanUp,
    PanDown,
    Zoom(i16),
    DeleteSelection,
    RunConfiguredActions(ActionTrigger),

    // top toolbar
    Scale(u16), // in %, 0 means fit to window
    ClearAll,
    SelectTool(Tools),
    Undo,
    Redo,
    RunAction(Action),

    // bottom toolbar
    SelectColorIndex(u64),
    CycleSize,
    SelectSize(Size),
    FocusAnnotationSizeFactor,
    ToggleFill,
    ToggleRoundCaps,
}

impl fmt::Display for ShortcutCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            // generic
            ShortcutCommand::OpenGtkInspector => "open-gtk-inspector",
            ShortcutCommand::PanLeft => "pan-left",
            ShortcutCommand::PanRight => "pan-right",
            ShortcutCommand::PanUp => "pan-up",
            ShortcutCommand::PanDown => "pan-down",
            ShortcutCommand::Zoom(factor) => {
                write!(f, "zoom:{}", factor)?;
                return Ok(());
            }
            ShortcutCommand::DeleteSelection => "delete-selection",
            ShortcutCommand::RunConfiguredActions(ActionTrigger::Escape) => "run-actions-on-escape",
            ShortcutCommand::RunConfiguredActions(ActionTrigger::Enter) => "run-actions-on-enter",
            ShortcutCommand::ToggleToolbars => "toggle-toolbars",

            // top toolbar
            ShortcutCommand::Scale(factor) => {
                write!(f, "scale:{}", factor)?;
                return Ok(());
            }
            ShortcutCommand::ClearAll => "clear-all",
            ShortcutCommand::Undo => "undo",
            ShortcutCommand::Redo => "redo",
            ShortcutCommand::SelectTool(tool) => {
                write!(f, "{}", tool.to_string().to_lowercase())?;
                return Ok(());
            }
            ShortcutCommand::RunAction(action) => match action {
                Action::SaveToClipboard => "save-to-clipboard",
                Action::SaveToFile => "save-to-file",
                Action::SaveToFileAs => "save-to-file-as",
                Action::CopyFilepathToClipboard => "copy-filepath-to-clipboard",
                Action::Exit => "exit",
            },

            // bottom toolbar
            ShortcutCommand::SelectColorIndex(index) => {
                write!(f, "select-color-index:{}", index + 1)?;
                return Ok(());
            }
            ShortcutCommand::CycleSize => "cycle-size",
            ShortcutCommand::SelectSize(size) => match size {
                Size::Small => "select-size:small",
                Size::Medium => "select-size:medium",
                Size::Large => "select-size:large",
            },
            ShortcutCommand::FocusAnnotationSizeFactor => "focus-annotation-size-factor",
            ShortcutCommand::ToggleFill => "toggle-fill",
            ShortcutCommand::ToggleRoundCaps => "toggle-round-caps",
        };
        write!(f, "{}", name)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ParseCommandError;

impl FromStr for ShortcutCommand {
    type Err = ParseCommandError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            // generic
            "open-gtk-inspector" => Ok(ShortcutCommand::OpenGtkInspector),
            "toggle-toolbars" => Ok(ShortcutCommand::ToggleToolbars),
            "pan-left" => Ok(ShortcutCommand::PanLeft),
            "pan-right" => Ok(ShortcutCommand::PanRight),
            "pan-up" => Ok(ShortcutCommand::PanUp),
            "pan-down" => Ok(ShortcutCommand::PanDown),
            text if text.starts_with("zoom:") => {
                let num_str = text.strip_prefix("zoom:").unwrap();
                if let Ok(num) = num_str.parse::<i16>() {
                    return Ok(ShortcutCommand::Zoom(num));
                }
                Err(ParseCommandError)
            }
            "delete-selection" => Ok(ShortcutCommand::DeleteSelection),
            "run-actions-on-escape" => {
                Ok(ShortcutCommand::RunConfiguredActions(ActionTrigger::Escape))
            }
            "run-actions-on-enter" => {
                Ok(ShortcutCommand::RunConfiguredActions(ActionTrigger::Enter))
            }

            // top toolbar
            text if text.starts_with("scale:") => {
                let num_str = text.strip_prefix("scale:").unwrap();
                if let Ok(num) = num_str.parse::<u16>() {
                    return Ok(ShortcutCommand::Scale(num));
                }
                Err(ParseCommandError)
            }
            "clear-all" => Ok(ShortcutCommand::ClearAll),
            "undo" => Ok(ShortcutCommand::Undo),
            "redo" => Ok(ShortcutCommand::Redo),
            "select-tool" => Ok(ShortcutCommand::SelectTool(Tools::Rectangle)),
            "save-to-file" => Ok(ShortcutCommand::RunAction(Action::SaveToFile)),
            "save-to-file-as" => Ok(ShortcutCommand::RunAction(Action::SaveToFileAs)),
            "save-to-clipboard" => Ok(ShortcutCommand::RunAction(Action::SaveToClipboard)),
            "copy-filepath-to-clipboard" => {
                Ok(ShortcutCommand::RunAction(Action::CopyFilepathToClipboard))
            }
            "exit" => Ok(ShortcutCommand::RunAction(Action::Exit)),

            // bottom toolbar
            text if text.starts_with("select-color-index:") => {
                let num_str = text.strip_prefix("select-color-index:").unwrap();

                if let Some(num) = num_str.parse::<u64>().ok().filter(|n| *n > 0) {
                    return Ok(ShortcutCommand::SelectColorIndex(num - 1));
                }
                Err(ParseCommandError)
            }
            "cycle-size" => Ok(ShortcutCommand::CycleSize),
            "select-size:small" => Ok(ShortcutCommand::SelectSize(Size::Small)),
            "select-size:medium" => Ok(ShortcutCommand::SelectSize(Size::Medium)),
            "select-size:large" => Ok(ShortcutCommand::SelectSize(Size::Large)),
            "focus-annotation-size-factor" => Ok(ShortcutCommand::FocusAnnotationSizeFactor),
            "toggle-fill" => Ok(ShortcutCommand::ToggleFill),
            "toggle-round-caps" => Ok(ShortcutCommand::ToggleRoundCaps),
            _ => Err(ParseCommandError),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct KeyBinding {
    modifiers: ModifierType,
    key: Key,
}

impl KeyBinding {
    fn from_binding_str(key_binding: &str) -> Result<Self, String> {
        if let Some((key, modifiers)) = gtk::accelerator_parse(key_binding) {
            if gtk::accelerator_valid(key, modifiers) {
                Ok(KeyBinding { modifiers, key })
            } else {
                Err(format!(
                    "Keybinding '{}' parsed successfully but not a valid hardware shortcut context.",
                    key_binding
                ))
            }
        } else {
            Err(format!(
                "Syntax Error: '{}' is not a recognized GTK accelerator string name.",
                key_binding
            ))
        }
    }
}

impl fmt::Display for KeyBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", gtk::accelerator_name(self.key, self.modifiers))
    }
}

#[derive(Debug, Default, Clone)]
pub struct ShortcutRegistry {
    key_bindings: HashMap<KeyBinding, ShortcutCommand>,
}

impl ShortcutRegistry {
    fn add_key_binding(&mut self, key_binding_str: &str, command: ShortcutCommand) {
        match KeyBinding::from_binding_str(key_binding_str) {
            Ok(key_binding) => {
                self.key_bindings.insert(key_binding, command);
            }
            Err(err) => {
                eprintln!(
                    "Invalid key binding '{}' for command {:?}: {}",
                    key_binding_str, command, err
                );
            }
        }
    }

    pub fn from_config() -> Self {
        static REGISTRY: OnceLock<ShortcutRegistry> = OnceLock::new();
        REGISTRY.get_or_init(Self::build_from_config).clone()
    }

    fn build_from_config() -> Self {
        let mut registry = Self::default();

        type SC = ShortcutCommand;
        type A = Action;

        // generic
        registry.add_key_binding("<Shift><Control>d", SC::OpenGtkInspector);
        registry.add_key_binding("<Shift><Control>i", SC::OpenGtkInspector);
        registry.add_key_binding("<Control>t", SC::ToggleToolbars);
        registry.add_key_binding("<Alt>Left", SC::PanLeft);
        registry.add_key_binding("<Alt>Right", SC::PanRight);
        registry.add_key_binding("<Alt>Up", SC::PanUp);
        registry.add_key_binding("<Alt>Down", SC::PanDown);
        registry.add_key_binding("<Control>plus", SC::Zoom(1));
        registry.add_key_binding("<Control>minus", SC::Zoom(-1));
        registry.add_key_binding("Delete", SC::DeleteSelection);
        registry.add_key_binding("<Shift>Delete", SC::ClearAll);
        registry.add_key_binding("Escape", SC::RunConfiguredActions(ActionTrigger::Escape));
        registry.add_key_binding("Return", SC::RunConfiguredActions(ActionTrigger::Enter));
        registry.add_key_binding("KP_Enter", SC::RunConfiguredActions(ActionTrigger::Enter));

        registry.add_key_binding("<Alt>2", SC::Scale(50));
        registry.add_key_binding("<Alt>3", SC::Scale(33));
        registry.add_key_binding("<Alt>4", SC::Scale(25));
        registry.add_key_binding("<Control>2", SC::Scale(200));
        registry.add_key_binding("<Control>3", SC::Scale(300));
        registry.add_key_binding("<Control>4", SC::Scale(400));

        // top toolbar
        registry.add_key_binding("<Alt>1", SC::Scale(100));
        registry.add_key_binding("<Control>1", SC::Scale(0)); // fit to window
        registry.add_key_binding("<Control>z", SC::Undo);
        registry.add_key_binding("<Control>y", SC::Redo);
        registry.add_key_binding("p", SC::SelectTool(Tools::Pointer));
        registry.add_key_binding("c", SC::SelectTool(Tools::Crop));
        registry.add_key_binding("b", SC::SelectTool(Tools::Brush));
        registry.add_key_binding("i", SC::SelectTool(Tools::Line));
        registry.add_key_binding("z", SC::SelectTool(Tools::Arrow));
        registry.add_key_binding("r", SC::SelectTool(Tools::Rectangle));
        registry.add_key_binding("e", SC::SelectTool(Tools::Ellipse));
        registry.add_key_binding("t", SC::SelectTool(Tools::Text));
        registry.add_key_binding("m", SC::SelectTool(Tools::Marker));
        registry.add_key_binding("u", SC::SelectTool(Tools::Blur));
        registry.add_key_binding("g", SC::SelectTool(Tools::Highlight));
        registry.add_key_binding("x", SC::SelectTool(Tools::FringePixelate));
        registry.add_key_binding("<Control>c", SC::RunAction(A::SaveToClipboard));
        registry.add_key_binding("<Control><Alt>c", SC::RunAction(A::CopyFilepathToClipboard));
        registry.add_key_binding("<Control>s", SC::RunAction(A::SaveToFile));
        registry.add_key_binding("<Shift><Control>s", SC::RunAction(A::SaveToFileAs));

        // bottom toolbar
        for i in 1..11 {
            let key = (i % 10).to_string();
            registry.add_key_binding(&key, SC::SelectColorIndex(i - 1));
        }

        registry.add_key_binding("minus", SC::CycleSize);
        registry.add_key_binding("s", SC::FocusAnnotationSizeFactor);
        registry.add_key_binding("f", SC::ToggleFill);

        // merge with config keybinds, allowing config to override defaults
        for (kb_str, tool_or_cmd) in APP_CONFIG.read().keybinds() {
            if let Ok(tool) = Tools::from_str(tool_or_cmd.as_str()) {
                registry.add_key_binding(kb_str, SC::SelectTool(tool));
            } else if let Ok(tool) = Tools::from_str(kb_str.as_str()) {
                registry.add_key_binding(tool_or_cmd, SC::SelectTool(tool));
                eprintln!("Deprecated syntax for key binding: {kb_str} = \"{tool_or_cmd}\"");
                eprintln!("    Please update the config to: \"{tool_or_cmd}\" = \"{kb_str}\"");
            } else if let Ok(command) = SC::from_str(tool_or_cmd.as_str()) {
                registry.add_key_binding(kb_str, command);
            } else if tool_or_cmd == "none" {
                match KeyBinding::from_binding_str(kb_str) {
                    Ok(key_binding) => {
                        registry.key_bindings.remove(&key_binding);
                    }
                    Err(err) => {
                        eprintln!(
                            "Invalid key binding '{}' for command 'none': {}",
                            kb_str, err
                        );
                    }
                }
            } else {
                eprintln!("Unknown tool or command in config for key '{kb_str}': '{tool_or_cmd}'");
            }
        }

        registry
    }

    pub fn get_command_for_key_event(&self, event: &KeyEventMsg) -> Option<ShortcutCommand> {
        let modifier_only = matches!(
            event.key,
            Key::Control_L
                | Key::Control_R
                | Key::Shift_L
                | Key::Shift_R
                | Key::Alt_L
                | Key::Alt_R
                | Key::Meta_L
                | Key::Meta_R
                | Key::Super_L
                | Key::Super_R
        );

        // determine unshifted key
        let key = gtk::gdk::Display::default()
            .and_then(|d| d.translate_key(event.code, gtk::gdk::ModifierType::empty(), 0))
            .map_or(event.key, |t| t.0);

        let key_binding = KeyBinding {
            key,
            modifiers: event.modifier,
        };

        if let Some(command) = self.key_bindings.get(&key_binding) {
            Some(*command)
        } else if !modifier_only {
            eprintln!("\"{}\" is not bound to a command or tool", key_binding);
            None
        } else {
            None
        }
    }

    pub fn get_binding_for_command(&self, command: ShortcutCommand) -> Option<String> {
        self.key_bindings.iter().find_map(|(binding, cmd)| {
            if *cmd == command {
                Some(gtk::accelerator_get_label(binding.key, binding.modifiers).to_string())
            } else {
                None
            }
        })
    }
}
