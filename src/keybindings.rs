use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::sync::OnceLock;

use relm4::gtk;
use relm4::gtk::gdk::prelude::DisplayExtManual;
use relm4::gtk::gdk::{Key, ModifierType};

use crate::configuration::{APP_CONFIG, Action, log_config_error};
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
    PasteImage,

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
            ShortcutCommand::PasteImage => "paste-image",

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
            "paste-image" => Ok(ShortcutCommand::PasteImage),

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

/// Returns true for a letter of a script other than Latin, e.g. Cyrillic or Greek.
fn is_non_latin_letter(key: Key) -> bool {
    key.to_unicode().is_some_and(|c| {
        // Basic Latin to Latin Extended-B, and Latin Extended Additional
        let latin = c <= '\u{024F}' || ('\u{1E00}'..='\u{1EFF}').contains(&c);
        c.is_alphabetic() && !latin
    })
}

/// The unshifted keys of the US QWERTY layout, each row with the evdev keycode
/// of its first key.
#[rustfmt::skip]
const US_QWERTY_ROWS: [(u32, &[Key]); 5] = [
    (2, &[
        Key::_1, Key::_2, Key::_3, Key::_4, Key::_5, Key::_6, Key::_7, Key::_8, Key::_9, Key::_0,
        Key::minus, Key::equal,
    ]),
    (16, &[
        Key::q, Key::w, Key::e, Key::r, Key::t, Key::y, Key::u, Key::i, Key::o, Key::p,
        Key::bracketleft, Key::bracketright,
    ]),
    (30, &[
        Key::a, Key::s, Key::d, Key::f, Key::g, Key::h, Key::j, Key::k, Key::l,
        Key::semicolon, Key::apostrophe, Key::grave,
    ]),
    (43, &[
        Key::backslash, Key::z, Key::x, Key::c, Key::v, Key::b, Key::n, Key::m,
        Key::comma, Key::period, Key::slash,
    ]),
    // the extra key of ISO keyboards, between the left Shift and Z
    (86, &[Key::less]),
];

/// Returns the key at the physical position of `keycode` in the US QWERTY layout.
fn us_qwerty_key(keycode: u32) -> Option<Key> {
    // XKB keycodes are evdev keycodes plus 8
    let evdev = keycode.checked_sub(8)?;
    US_QWERTY_ROWS.iter().find_map(|(first, row)| {
        let index = usize::try_from(evdev.checked_sub(*first)?).ok()?;
        row.get(index).copied()
    })
}

/// Returns the key to match shortcuts with when `active` cannot match them.
///
/// `active` is the unshifted key in the active layout, `other_layouts` holds the
/// unshifted keys of the same physical key in the other configured layouts, in
/// layout order, and `keycode` is the hardware keycode of the event.
///
/// Only a letter of a non-Latin script falls back, because no default shortcut
/// can match it. A Latin key never does: in Dvorak or QWERTZ, a key at the
/// position of a bound QWERTY key would otherwise trigger that binding too.
/// A configured Latin layout wins over the US QWERTY position, so that a
/// German user gets the German layout.
fn fallback_key(active: Key, other_layouts: &[Key], keycode: u32) -> Option<Key> {
    if !is_non_latin_letter(active) {
        return None;
    }
    other_layouts
        .iter()
        .copied()
        .find(|key| !is_non_latin_letter(*key))
        .or_else(|| us_qwerty_key(keycode))
}

/// Returns the keys a key event matches shortcuts with: the unshifted key in
/// the active `layout`, and the key to fall back to, see [`fallback_key`].
pub fn shortcut_keys(key: Key, keycode: u32, layout: u32) -> (Key, Option<Key>) {
    let Some(display) = gtk::gdk::Display::default() else {
        return (key, None);
    };
    let layouts: Vec<u32> = display
        .map_keycode(keycode)
        .unwrap_or_default()
        .iter()
        .filter_map(|(keymap_key, _)| u32::try_from(keymap_key.group()).ok())
        .collect();
    let unshifted = |layout: u32| {
        display
            .translate_key(keycode, ModifierType::empty(), i32::try_from(layout).ok()?)
            .map(|t| t.0)
    };
    match resolve_shortcut_keys(keycode, layout, &layouts, unshifted) {
        Some(keys) => keys,
        None => (key, None),
    }
}

/// Returns the unshifted key of `keycode` in the `active` layout and the key to
/// fall back to, or `None` if the active layout has no key for `keycode`.
///
/// `layouts` holds the layouts of the keymap that have a key for `keycode`, and
/// `unshifted` returns that key in one of them.
fn resolve_shortcut_keys(
    keycode: u32,
    active: u32,
    layouts: &[u32],
    unshifted: impl Fn(u32) -> Option<Key>,
) -> Option<(Key, Option<Key>)> {
    let active_key = unshifted(active)?;
    let mut other_layouts: Vec<u32> = layouts
        .iter()
        .copied()
        .filter(|layout| *layout != active)
        .collect();
    other_layouts.sort_unstable();
    other_layouts.dedup();
    let other_keys: Vec<Key> = other_layouts.into_iter().filter_map(unshifted).collect();
    Some((active_key, fallback_key(active_key, &other_keys, keycode)))
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct KeyBinding {
    modifiers: ModifierType,
    key: Key,
}

impl KeyBinding {
    /// Returns the binding of the typed `key` without the Shift needed to type it,
    /// or `None` if `key` is the `unshifted_key` or only its uppercase letter.
    fn typed(key: Key, unshifted_key: Key, modifiers: ModifierType) -> Option<Self> {
        (key.to_lower() != unshifted_key).then(|| KeyBinding {
            key,
            modifiers: modifiers - ModifierType::SHIFT_MASK,
        })
    }

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
                log_config_error(format!(
                    "Invalid key binding '{}' for command {:?}: {}",
                    key_binding_str, command, err
                ));
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
        registry.add_key_binding("<Control>equal", SC::Zoom(1));
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
        registry.add_key_binding("<Shift>g", SC::SelectTool(Tools::HighlightFreehand));
        registry.add_key_binding("x", SC::SelectTool(Tools::FringePixelate));
        registry.add_key_binding("o", SC::SelectTool(Tools::Image));
        registry.add_key_binding("<Control>c", SC::RunAction(A::SaveToClipboard));
        registry.add_key_binding("<Control><Alt>c", SC::RunAction(A::CopyFilepathToClipboard));
        registry.add_key_binding("<Control>s", SC::RunAction(A::SaveToFile));
        registry.add_key_binding("<Shift><Control>s", SC::RunAction(A::SaveToFileAs));
        registry.add_key_binding("<Control>v", SC::PasteImage);

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
                log_config_error(format!(
                    "Deprecated syntax for key binding: {kb_str} = \"{tool_or_cmd}\""
                ));
                eprintln!("   Please update the config to: \"{tool_or_cmd}\" = \"{kb_str}\"");
            } else if let Ok(command) = SC::from_str(tool_or_cmd.as_str()) {
                registry.add_key_binding(kb_str, command);
            } else if tool_or_cmd == "none" {
                match KeyBinding::from_binding_str(kb_str) {
                    Ok(key_binding) => {
                        registry.key_bindings.remove(&key_binding);
                    }
                    Err(err) => {
                        log_config_error(format!(
                            "Invalid key binding '{}' for command 'none': {}",
                            kb_str, err
                        ));
                    }
                }
            } else {
                log_config_error(format!(
                    "Unknown tool or command in config for key '{kb_str}': '{tool_or_cmd}'"
                ));
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

        let command = self.command_for_keys(
            event.key,
            event.shortcut_key,
            event.fallback_key,
            event.modifier,
        );
        if command.is_none() && !modifier_only {
            let key_binding = KeyBinding {
                key: event.shortcut_key,
                modifiers: event.modifier,
            };
            let bindings = match KeyBinding::typed(event.key, event.shortcut_key, event.modifier) {
                Some(typed) => {
                    format!("\"{key_binding}\" (\"{typed}\")")
                }
                None => format!("\"{key_binding}\""),
            };

            eprintln!("{bindings} is not bound to a command or tool")
        }
        command
    }

    /// Looks up the typed `key` first, then the `unshifted_key`, and the `fallback_key` last.
    fn command_for_keys(
        &self,
        key: Key,
        unshifted_key: Key,
        fallback_key: Option<Key>,
        modifiers: ModifierType,
    ) -> Option<ShortcutCommand> {
        KeyBinding::typed(key, unshifted_key, modifiers)
            .into_iter()
            .chain(
                [unshifted_key]
                    .into_iter()
                    .chain(fallback_key)
                    .map(|key| KeyBinding { modifiers, key }),
            )
            .find_map(|key_binding| self.key_bindings.get(&key_binding))
            .copied()
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

#[cfg(test)]
mod tests {
    use super::*;
    use relm4::gtk::glib::translate::IntoGlib;
    use xkbcommon::xkb;

    // XKB keycodes of the keys at the US QWERTY positions of "b" and "z"
    const KEYCODE_B: u32 = 56;
    const KEYCODE_Z: u32 = 52;

    #[test]
    fn non_latin_letters_are_detected() {
        assert!(is_non_latin_letter(Key::Cyrillic_i));
        assert!(is_non_latin_letter(Key::Greek_beta));
        assert!(!is_non_latin_letter(Key::b));
        assert!(!is_non_latin_letter(Key::odiaeresis));
        assert!(!is_non_latin_letter(Key::period));
        assert!(!is_non_latin_letter(Key::_1));
        assert!(!is_non_latin_letter(Key::Return));
    }

    #[test]
    fn latin_key_has_no_fallback() {
        // Dvorak types "x" where QWERTY types "b"
        assert_eq!(fallback_key(Key::x, &[Key::b], KEYCODE_B), None);
    }

    #[test]
    fn punctuation_has_no_fallback() {
        // Dvorak types "." where QWERTY types "e"
        assert_eq!(fallback_key(Key::period, &[Key::e], 26), None);
    }

    #[test]
    fn non_ascii_latin_letter_has_no_fallback() {
        // German types "ö" where QWERTY types ";"
        assert_eq!(fallback_key(Key::odiaeresis, &[], 47), None);
    }

    #[test]
    fn non_latin_letter_falls_back_to_latin_layout() {
        assert_eq!(
            fallback_key(Key::Cyrillic_i, &[Key::b], KEYCODE_B),
            Some(Key::b)
        );
    }

    #[test]
    fn configured_latin_layout_wins_over_physical_position() {
        // German types "y" where QWERTY types "z"
        assert_eq!(
            fallback_key(Key::Cyrillic_ya, &[Key::y], KEYCODE_Z),
            Some(Key::y)
        );
    }

    #[test]
    fn first_latin_layout_wins() {
        assert_eq!(
            fallback_key(
                Key::Cyrillic_ya,
                &[Key::Greek_zeta, Key::y, Key::z],
                KEYCODE_Z
            ),
            Some(Key::y)
        );
    }

    #[test]
    fn without_latin_layout_falls_back_to_physical_position() {
        assert_eq!(fallback_key(Key::Cyrillic_i, &[], KEYCODE_B), Some(Key::b));
        assert_eq!(
            fallback_key(Key::Cyrillic_i, &[Key::Greek_beta], KEYCODE_B),
            Some(Key::b)
        );
        // Russian types "х" where QWERTY types "["
        assert_eq!(
            fallback_key(Key::Cyrillic_ha, &[], 34),
            Some(Key::bracketleft)
        );
    }

    #[test]
    fn unknown_physical_position_has_no_fallback() {
        assert_eq!(fallback_key(Key::Cyrillic_i, &[], 200), None);
    }

    fn registry(bindings: &[(Key, ShortcutCommand)]) -> ShortcutRegistry {
        let mut registry = ShortcutRegistry::default();
        for (key, command) in bindings {
            registry.key_bindings.insert(
                KeyBinding {
                    modifiers: ModifierType::empty(),
                    key: *key,
                },
                *command,
            );
        }
        registry
    }

    #[test]
    fn fallback_key_matches_when_active_key_is_unbound() {
        let registry = registry(&[(Key::b, ShortcutCommand::SelectTool(Tools::Brush))]);
        assert_eq!(
            registry.command_for_keys(
                Key::Cyrillic_i,
                Key::Cyrillic_i,
                Some(Key::b),
                ModifierType::empty()
            ),
            Some(ShortcutCommand::SelectTool(Tools::Brush))
        );
    }

    #[test]
    fn active_key_wins_over_fallback_key() {
        let registry = registry(&[
            (Key::b, ShortcutCommand::SelectTool(Tools::Brush)),
            (Key::Cyrillic_i, ShortcutCommand::SelectTool(Tools::Text)),
        ]);
        assert_eq!(
            registry.command_for_keys(
                Key::Cyrillic_i,
                Key::Cyrillic_i,
                Some(Key::b),
                ModifierType::empty()
            ),
            Some(ShortcutCommand::SelectTool(Tools::Text))
        );
    }

    /// A keyboard with real XKB layouts, as RMLVO names like "us,ru" and "dvorak,".
    struct Keyboard {
        keymap: xkb::Keymap,
    }

    impl Keyboard {
        fn new(layouts: &str, variants: &str) -> Self {
            let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
            let keymap = xkb::Keymap::new_from_names(
                &context,
                "",
                "",
                layouts,
                variants,
                None,
                xkb::KEYMAP_COMPILE_NO_FLAGS,
            )
            .expect("compile the XKB keymap");
            Self { keymap }
        }

        /// Returns the unshifted key of the XKB `keycode` in `layout`.
        fn unshifted(&self, keycode: u32, layout: u32) -> Option<Key> {
            let code = xkb::Keycode::new(keycode);
            let sym = *self.keymap.key_get_syms_by_level(code, layout, 0).first()?;
            Key::from_name(xkb::keysym_get_name(sym))
        }

        /// Returns the command for a press of the key with the `evdev` keycode
        /// while the layout with index `active` is active.
        fn command(
            &self,
            registry: &ShortcutRegistry,
            modifiers: ModifierType,
            evdev: u32,
            active: u32,
        ) -> Option<ShortcutCommand> {
            let keycode = evdev + 8;
            let code = xkb::Keycode::new(keycode);
            let layouts: Vec<u32> = (0..self.keymap.num_layouts_for_key(code)).collect();
            let (key, fallback_key) = resolve_shortcut_keys(keycode, active, &layouts, |layout| {
                self.unshifted(keycode, layout)
            })?;
            let typed_key = self.typed(keycode, modifiers, active)?;
            registry.command_for_keys(typed_key, key, fallback_key, modifiers)
        }

        /// Returns the key the XKB `keycode` types in `layout`, with Shift if
        /// `modifiers` holds it. Stands in for the key GTK passes with a key event,
        /// which tests do not have.
        fn typed(&self, keycode: u32, modifiers: ModifierType, layout: u32) -> Option<Key> {
            let level = u32::from(modifiers.contains(ModifierType::SHIFT_MASK));
            let code = xkb::Keycode::new(keycode);
            let sym = *self
                .keymap
                .key_get_syms_by_level(code, layout, level)
                .first()?;
            Key::from_name(xkb::keysym_get_name(sym))
        }
    }

    #[test]
    fn us_qwerty_table_matches_the_us_layout() {
        // Both ways: each key of the table is right, and no printable key is missing.
        // The keypad is not in the table: its keysyms are outside the ASCII range.
        // Evdev keycodes from 128 are extra keys, e.g. the parentheses of some
        // keypads, which type ASCII but are not on the main block.
        let us = Keyboard::new("us", "");
        for keycode in 0..128 + 8 {
            let printable = us
                .unshifted(keycode, 0)
                .filter(|key| (0x21..=0x7e).contains(&key.into_glib()));
            assert_eq!(us_qwerty_key(keycode), printable, "XKB keycode {keycode}");
        }
    }

    // evdev keycodes, named by the US QWERTY key at their position
    const QWERTY_E: u32 = 18;
    const QWERTY_Y: u32 = 21;
    const QWERTY_J: u32 = 36;
    const QWERTY_Z: u32 = 44;
    const QWERTY_C: u32 = 46;
    const QWERTY_B: u32 = 48;
    const QWERTY_N: u32 = 49;
    const QWERTY_SLASH: u32 = 53;

    const NONE: ModifierType = ModifierType::empty();
    const CTRL: ModifierType = ModifierType::CONTROL_MASK;
    const BRUSH: Option<ShortcutCommand> = Some(ShortcutCommand::SelectTool(Tools::Brush));
    const PIXELATE: Option<ShortcutCommand> =
        Some(ShortcutCommand::SelectTool(Tools::FringePixelate));
    const ARROW: Option<ShortcutCommand> = Some(ShortcutCommand::SelectTool(Tools::Arrow));
    const UNDO: Option<ShortcutCommand> = Some(ShortcutCommand::Undo);
    const REDO: Option<ShortcutCommand> = Some(ShortcutCommand::Redo);

    fn default_registry() -> ShortcutRegistry {
        let mut registry = ShortcutRegistry::default();
        for (modifiers, key, command) in [
            (NONE, Key::b, BRUSH),
            (NONE, Key::x, PIXELATE),
            (NONE, Key::z, ARROW),
            (NONE, Key::c, Some(ShortcutCommand::SelectTool(Tools::Crop))),
            (
                NONE,
                Key::e,
                Some(ShortcutCommand::SelectTool(Tools::Ellipse)),
            ),
            (CTRL, Key::z, UNDO),
            (CTRL, Key::y, REDO),
        ] {
            registry
                .key_bindings
                .insert(KeyBinding { modifiers, key }, command.unwrap());
        }
        registry
    }

    #[test]
    fn german_layout_keeps_its_own_z_and_y() {
        // #508: Ctrl+Z on a German keyboard must not do a redo
        let r = default_registry();
        let de = Keyboard::new("de", "");
        assert_eq!(de.command(&r, CTRL, QWERTY_Y, 0), UNDO);
        assert_eq!(de.command(&r, CTRL, QWERTY_Z, 0), REDO);
        assert_eq!(de.command(&r, NONE, QWERTY_Y, 0), ARROW);
    }

    #[test]
    fn german_and_us_layouts_follow_the_active_layout() {
        let r = default_registry();
        let de_us = Keyboard::new("de,us", "");
        assert_eq!(de_us.command(&r, CTRL, QWERTY_Y, 0), UNDO);
        assert_eq!(de_us.command(&r, CTRL, QWERTY_Z, 1), UNDO);
        assert_eq!(de_us.command(&r, CTRL, QWERTY_Y, 1), REDO);
    }

    #[test]
    fn russian_falls_back_to_the_german_layout() {
        let r = default_registry();
        let de_ru = Keyboard::new("de,ru", "");
        assert_eq!(de_ru.command(&r, CTRL, QWERTY_Y, 1), UNDO);
        assert_eq!(de_ru.command(&r, CTRL, QWERTY_Z, 1), REDO);
    }

    #[test]
    fn russian_falls_back_to_the_us_layout_in_either_order() {
        let r = default_registry();
        for (layouts, ru, us) in [("us,ru", 1, 0), ("ru,us", 0, 1)] {
            let keyboard = Keyboard::new(layouts, "");
            for active in [ru, us] {
                assert_eq!(keyboard.command(&r, NONE, QWERTY_B, active), BRUSH);
                assert_eq!(keyboard.command(&r, CTRL, QWERTY_Z, active), UNDO);
            }
        }
    }

    #[test]
    fn russian_alone_falls_back_to_the_physical_position() {
        let r = default_registry();
        let ru = Keyboard::new("ru", "");
        assert_eq!(ru.command(&r, NONE, QWERTY_B, 0), BRUSH);
        assert_eq!(ru.command(&r, CTRL, QWERTY_Z, 0), UNDO);
    }

    #[test]
    fn dvorak_alone_follows_its_own_letters() {
        let r = default_registry();
        let dvorak = Keyboard::new("us", "dvorak");
        // Dvorak types "b" on the QWERTY "n" key and "x" on the QWERTY "b" key
        assert_eq!(dvorak.command(&r, NONE, QWERTY_N, 0), BRUSH);
        assert_eq!(dvorak.command(&r, NONE, QWERTY_B, 0), PIXELATE);
        // and "." on the QWERTY "e" key, "j" on the QWERTY "c" key
        assert_eq!(dvorak.command(&r, NONE, QWERTY_E, 0), None);
        assert_eq!(dvorak.command(&r, NONE, QWERTY_C, 0), None);
        // and "z" on the QWERTY "/" key
        assert_eq!(dvorak.command(&r, CTRL, QWERTY_SLASH, 0), UNDO);
        assert_eq!(dvorak.command(&r, CTRL, QWERTY_Z, 0), None);
    }

    #[test]
    fn dvorak_as_second_layout_follows_its_own_letters() {
        let r = default_registry();
        let us_dvorak = Keyboard::new("us,us", ",dvorak");
        assert_eq!(us_dvorak.command(&r, NONE, QWERTY_N, 1), BRUSH);
        assert_eq!(us_dvorak.command(&r, NONE, QWERTY_B, 1), PIXELATE);
        assert_eq!(us_dvorak.command(&r, NONE, QWERTY_E, 1), None);
        assert_eq!(us_dvorak.command(&r, NONE, QWERTY_J, 1), None);
    }

    #[test]
    fn russian_dvorak_falls_back_to_english_dvorak() {
        let r = default_registry();
        let dvorak_ru = Keyboard::new("us,ru", "dvorak,phonetic_dvorak");
        for active in [0, 1] {
            assert_eq!(dvorak_ru.command(&r, NONE, QWERTY_N, active), BRUSH);
            assert_eq!(dvorak_ru.command(&r, NONE, QWERTY_B, active), PIXELATE);
            assert_eq!(dvorak_ru.command(&r, CTRL, QWERTY_SLASH, active), UNDO);
        }
    }

    #[test]
    fn uppercase_letter_is_not_a_typed_key() {
        let shift = ModifierType::SHIFT_MASK;
        assert_eq!(KeyBinding::typed(Key::G, Key::g, shift), None);
        assert_eq!(
            KeyBinding::typed(Key::Odiaeresis, Key::odiaeresis, shift),
            None
        );
        assert_eq!(
            KeyBinding::typed(Key::equal, Key::_0, CTRL | shift),
            Some(KeyBinding {
                key: Key::equal,
                modifiers: CTRL
            })
        );
    }

    #[test]
    fn modifiers_must_match() {
        let registry = registry(&[(Key::z, ShortcutCommand::Undo)]);
        assert_eq!(
            registry.command_for_keys(
                Key::Cyrillic_ya,
                Key::Cyrillic_ya,
                Some(Key::z),
                ModifierType::CONTROL_MASK
            ),
            None
        );
    }
}
