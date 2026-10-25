use std::borrow::Cow;
use std::cell::Cell;
use std::rc::Rc;

use crate::{
    configuration::{APP_CONFIG, Action},
    keybindings::{ShortcutCommand, ShortcutRegistry},
    math::{Vec2D, get_closest_aspect_ratio},
    style::{Color, Size, SizePreset, StyleField, StyleFields},
    tools::Tools,
};

use crate::tools::GroupableTool;
use crate::ui::tool_group_button::{ToolGroupButton, ToolGroupButtonInput, ToolGroupInit};
use relm4::gtk::{
    gdk::{Key, ModifierType},
    gdk_pixbuf::{
        Pixbuf,
        gio::SimpleAction,
        glib::{Variant, VariantTy},
    },
};
use relm4::{
    actions::{ActionablePlus, RelmAction, RelmActionGroup},
    gtk::{Align, ColorChooserDialog, ResponseType, Window, gdk::RGBA, prelude::*},
    prelude::*,
};

pub struct ToolsToolbar {
    visible: bool,
    tool_buttons: FactoryVecDeque<ToolGroupButton>,
    tool_action: SimpleAction,
}

pub struct StyleToolbar {
    custom_color: RGBA,
    custom_color_pixbuf: Pixbuf,
    color_action: SimpleAction,
    size: Size,
    size_presets: [Size; 3],
    size_hints: [String; 3],
    size_spin_button: gtk::SpinButton,
    // set while the size is set from outside, to not report it back as change
    size_spin_updating: Rc<Cell<bool>>,
    // the active tool and the style fields it has itself, for the tooltips
    scope_tool: Tools,
    scope_own: StyleFields,
    palette_buttons: Vec<(gtk::ToggleButton, String)>,
    // the triangles showing whether their field is the tool's own, see
    // update_own_indicators
    own_indicators: Vec<(OwnIndicator, gtk::Image)>,
    selected_color: ColorButtons,
    size_spin_hint: String,
    fill_hint: String,
    round_caps_hint: String,
    fill_enabled: bool,
    round_caps_enabled: bool,
    visible: bool,
    output_dimensions: String,
    aspect_ratio: String,
    editing: bool,
}

#[derive(Debug, Copy, Clone)]
pub enum ToolbarEvent {
    FocusCanvas,
    ToolSelected(Tools),
    ColorSelected(Color),
    SetFill(bool),
    SizeSelected(Size),
    SizePresetSelected(SizePreset),
    /// whether the active tool has the field itself or uses the general one
    SetStyleFieldOwn(StyleField, bool),
    Redo,
    Undo,
    SaveFile,
    CopyClipboard,
    ToggleFill,
    ToggleRoundCaps,
    ClearAll,
    SaveFileAs,
    ScaleFitToWindow,
    ScaleOriginal,
}

#[derive(Debug, Copy, Clone)]
pub enum ToolsToolbarInput {
    SetVisibility(bool),
    ToggleVisibility,
    SwitchSelectedTool(Tools),
    SetToolEditing(bool),
}

#[derive(Debug, Copy, Clone)]
pub enum StyleToolbarInput {
    ColorButtonSelected(ColorButtons),
    SetColor(Color),
    SetFill(bool),
    SetRoundCaps(bool),
    SetSize(Size),
    SetSizePresets([Size; 3]),
    SizePresetClicked(SizePreset),
    SetStyleScope(Tools, StyleFields),
    /// Ctrl+click on a style button
    ToggleStyleFieldOwn(StyleField),
    ShowColorDialog,
    ColorDialogFinished(Option<RGBA>),
    SetVisibility(bool),
    ToggleVisibility,
    DimensionsChanged(Vec2D),
    LineDimensionsChanged(Vec2D),
    SetToolEditing(bool),
    FocusSize,
}

/// The style button a triangle in its top left corner is on. The triangle
/// shows whether the active tool has the field itself or uses the general one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnIndicator {
    Color(ColorButtons),
    /// the size input field
    Size,
    Fill,
    RoundCaps,
}

impl OwnIndicator {
    fn field(self) -> StyleField {
        match self {
            OwnIndicator::Color(_) => StyleField::Color,
            OwnIndicator::Size => StyleField::Size,
            OwnIndicator::Fill => StyleField::Fill,
            OwnIndicator::RoundCaps => StyleField::RoundCaps,
        }
    }
}

/// size of the corner with the triangle that can be clicked
const OWN_INDICATOR_CORNER: f64 = 12.0;

/// Puts `button` into an overlay with the triangle of `indicator` in its top
/// left corner. A click on the triangle or a Ctrl+click on the button switches
/// the field between the own one of the active tool and the general one.
fn add_own_indicator(
    button: &impl IsA<gtk::Widget>,
    sender: &ComponentSender<StyleToolbar>,
    indicator: OwnIndicator,
) -> gtk::Image {
    let button = button.upcast_ref::<gtk::Widget>();
    let parent = button
        .parent()
        .and_downcast::<gtk::Box>()
        .expect("style buttons are in a box");
    let previous = button.prev_sibling();
    parent.remove(button);
    let overlay = gtk::Overlay::new();
    overlay.add_css_class("own-style-overlay");
    overlay.set_child(Some(button));
    parent.insert_child_after(&overlay, previous.as_ref());

    let image = gtk::Image::builder()
        .icon_name("caret-down-right-filled")
        .pixel_size(8)
        .halign(Align::Start)
        .valign(Align::Start)
        .can_target(false)
        .build();
    image.add_css_class("own-style-indicator");
    overlay.add_overlay(&image);

    let in_corner = {
        let image = image.clone();
        move |x: f64, y: f64| {
            image.is_visible() && x <= OWN_INDICATOR_CORNER && y <= OWN_INDICATOR_CORNER
        }
    };

    let motion = gtk::EventControllerMotion::new();
    let hover = {
        let image = image.clone();
        let in_corner = in_corner.clone();
        move |x: f64, y: f64| {
            if in_corner(x, y) {
                image.add_css_class("own-style-indicator-hover");
            } else {
                image.remove_css_class("own-style-indicator-hover");
            }
        }
    };
    let hover_enter = hover.clone();
    motion.connect_enter(move |_, x, y| hover_enter(x, y));
    motion.connect_motion(move |_, x, y| hover(x, y));
    let leave_image = image.clone();
    motion.connect_leave(move |_| leave_image.remove_css_class("own-style-indicator-hover"));
    overlay.add_controller(motion);

    let gesture = gtk::GestureClick::builder().button(1).build();
    gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
    let sender = sender.clone();
    let overlay_for_pick = overlay.clone();
    gesture.connect_pressed(move |gesture, _, x, y| {
        // in the size field Ctrl+click on its + and - buttons steps by 10
        let on_step_button = indicator == OwnIndicator::Size
            && overlay_for_pick
                .pick(x, y, gtk::PickFlags::DEFAULT)
                .is_some_and(|w| {
                    w.is::<gtk::Button>() || w.ancestor(gtk::Button::static_type()).is_some()
                });
        let ctrl = !on_step_button
            && gesture
                .current_event_state()
                .contains(ModifierType::CONTROL_MASK);
        if ctrl || in_corner(x, y) {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            sender.input(StyleToolbarInput::ToggleStyleFieldOwn(indicator.field()));
        }
    });
    overlay.add_controller(gesture);

    image
}

/// Ctrl+click on `button` switches `field` like the triangle, see add_own_indicator.
fn connect_own_style_toggle(
    button: &impl IsA<gtk::Widget>,
    sender: &ComponentSender<StyleToolbar>,
    field: StyleField,
) {
    let gesture = gtk::GestureClick::builder().button(1).build();
    gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
    let sender = sender.clone();
    gesture.connect_pressed(move |gesture, _, _, _| {
        if gesture
            .current_event_state()
            .contains(ModifierType::CONTROL_MASK)
        {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            sender.input(StyleToolbarInput::ToggleStyleFieldOwn(field));
        }
    });
    button.add_controller(gesture);
}

fn create_icon_pixbuf(color: Color) -> Pixbuf {
    let pixbuf = Pixbuf::new(relm4::gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, 40, 40).unwrap();
    pixbuf.fill(color.to_rgba_u32());
    pixbuf
}

fn create_icon(color: Color) -> gtk::Image {
    gtk::Image::from_pixbuf(Some(&create_icon_pixbuf(color)))
}

/// Length and angle of a line from its start to its end. The angle is
/// counterclockwise from the right, from -180° to 180°.
fn format_line_dimensions(delta: Vec2D) -> (String, String) {
    let length = format!("{:.0}px", delta.norm());
    if delta.norm() < f32::EPSILON {
        return (length, String::new());
    }
    // y points down on screen; 0.0 - y instead of -y gives no negative zero,
    // which would turn 180° into -180°, and + 0.0 avoids showing -0
    let angle = (0.0 - delta.y).atan2(delta.x).to_degrees().round() + 0.0;
    (length, format!("{angle}°"))
}

fn get_hint(shortcut_registry: &ShortcutRegistry, command: ShortcutCommand) -> String {
    let command_name = command.to_string();
    let shortcut_hint = shortcut_registry.get_binding_for_command(command);
    match shortcut_hint {
        Some(hint) => format!("{command_name} ({hint})"),
        None => command_name,
    }
}

fn update_hint(
    shortcut_registry: &ShortcutRegistry,
    widget: &impl IsA<gtk::Widget>,
    command: ShortcutCommand,
) {
    let new_hint = get_hint(shortcut_registry, command);
    widget.set_tooltip_text(Some(&new_hint));
}

#[relm4::component(pub)]
impl SimpleComponent for ToolsToolbar {
    type Init = ();
    type Input = ToolsToolbarInput;
    type Output = ToolbarEvent;

    view! {
        root = gtk::Box {
            set_orientation: gtk::Orientation::Horizontal,
            set_spacing: 2,
            set_valign: Align::Start,
            set_halign: Align::Center,
            add_css_class: "toolbar",
            add_css_class: "toolbar-top",

            #[watch]
            set_visible: model.visible,

            #[name(original_scale_button)]
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,
                set_icon_name: "resize-large-regular",
                connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::ScaleOriginal);},
            },
            #[name(fit_to_window_button)]
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,
                set_icon_name: "page-fit-regular",
                connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::ScaleFitToWindow);},
            },
            #[name(reset_button)]
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,
                set_icon_name: "recycling-bin",
                connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::ClearAll);},
            },
            gtk::Separator {},
            #[name(undo_button)]
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,
                set_icon_name: "arrow-undo-filled",
                connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::Undo);},
            },
            #[name(redo_button)]
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,
                set_icon_name: "arrow-redo-filled",
                connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::Redo);},
            },
            gtk::Separator {},
            #[local_ref]
            tools_box -> gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,
                set_spacing: 2,
            },
            gtk::Separator {},
            #[name(copy_to_clipboard_button)]
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,
                set_icon_name: "copy-regular",
                connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::CopyClipboard);},
            },
            #[name(save_button)]
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,
                set_icon_name: "save-regular",
                connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::SaveFile);},
                set_visible: APP_CONFIG.read().output_filename().is_some()
            },
            #[name(save_as_button)]
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,
                set_icon_name: "save-multiple-regular",
                connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::SaveFileAs);},
            },
        },
    }

    fn update(&mut self, message: Self::Input, _sender: ComponentSender<Self>) {
        match message {
            ToolsToolbarInput::SetVisibility(visible) => self.visible = visible,
            ToolsToolbarInput::ToggleVisibility => {
                self.visible = !self.visible;
            }
            ToolsToolbarInput::SwitchSelectedTool(tool) => {
                // Change state of action, let GTK update the UI
                self.tool_action.change_state(&tool.to_variant());
                self.tool_buttons
                    .broadcast(ToolGroupButtonInput::SelectedToolChanged(tool));
            }
            ToolsToolbarInput::SetToolEditing(editing) => {
                self.tool_buttons
                    .broadcast(ToolGroupButtonInput::SetEditing(editing));
            }
        }
    }

    fn init(
        _: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let sender_tmp: ComponentSender<ToolsToolbar> = sender.clone();
        let tool_action: RelmAction<ToolsAction> = RelmAction::new_stateful_with_target_value(
            &APP_CONFIG.read().initial_tool(),
            move |_, state, value| {
                *state = value;
                // notify parent of change
                sender_tmp
                    .output_sender()
                    .emit(ToolbarEvent::ToolSelected(*state));
                // also change tracked active button
                sender_tmp
                    .input_sender()
                    .emit(ToolsToolbarInput::SwitchSelectedTool(*state))
            },
        );

        let shortcut_registry = ShortcutRegistry::from_config();

        let tools = [
            vec![GroupableTool {
                tool: Tools::Pointer,
                icon_name: "cursor-regular".into(),
                tooltip: None,
                shortcut: None,
            }],
            vec![GroupableTool {
                tool: Tools::Crop,
                icon_name: "crop-filled".into(),
                tooltip: None,
                shortcut: None,
            }],
            vec![
                GroupableTool {
                    tool: Tools::Pen,
                    icon_name: "pen-regular".into(),
                    tooltip: None,
                    shortcut: None,
                },
                GroupableTool {
                    tool: Tools::Highlighter,
                    icon_name: "format-ink-highlighter".into(),
                    tooltip: None,
                    shortcut: None,
                },
                GroupableTool {
                    tool: Tools::Brush,
                    icon_name: "brush-fill".into(),
                    tooltip: None,
                    shortcut: None,
                },
            ],
            vec![
                GroupableTool {
                    tool: Tools::Line,
                    icon_name: "minus-large".into(),
                    tooltip: None,
                    shortcut: None,
                },
                GroupableTool {
                    tool: Tools::Arrow,
                    icon_name: "arrow-up-right-filled".into(),
                    tooltip: None,
                    shortcut: None,
                },
            ],
            vec![
                GroupableTool {
                    tool: Tools::Rectangle,
                    icon_name: "checkbox-unchecked-regular".into(),
                    tooltip: None,
                    shortcut: None,
                },
                GroupableTool {
                    tool: Tools::Ellipse,
                    icon_name: "circle-regular".into(),
                    tooltip: None,
                    shortcut: None,
                },
            ],
            vec![GroupableTool {
                tool: Tools::Text,
                icon_name: "text-case-title-regular".into(),
                tooltip: None,
                shortcut: None,
            }],
            vec![GroupableTool {
                tool: Tools::Marker,
                icon_name: "number-circle-1-regular".into(),
                tooltip: None,
                shortcut: None,
            }],
            vec![
                GroupableTool {
                    tool: Tools::FringePixelate,
                    icon_name: "eye-off-regular".into(),
                    tooltip: None,
                    shortcut: None,
                },
                GroupableTool {
                    tool: Tools::Pixelate,
                    icon_name: "checkerboard".into(),
                    tooltip: None,
                    shortcut: None,
                },
                GroupableTool {
                    tool: Tools::Fringe,
                    icon_name: "tetris-app-regular".into(),
                    tooltip: None,
                    shortcut: None,
                },
                GroupableTool {
                    tool: Tools::Blur,
                    icon_name: "drop-regular".into(),
                    tooltip: None,
                    shortcut: None,
                },
                GroupableTool {
                    tool: Tools::Blackout,
                    icon_name: "rectangle-landscape-filled".into(),
                    tooltip: None,
                    shortcut: None,
                },
            ],
            vec![
                GroupableTool {
                    tool: Tools::BlockHighlighter,
                    icon_name: "highlight-regular".into(),
                    tooltip: None,
                    shortcut: None,
                },
                GroupableTool {
                    tool: Tools::HighlightSpotlight,
                    icon_name: "flashlight-regular".into(),
                    tooltip: None,
                    shortcut: None,
                },
                GroupableTool {
                    tool: Tools::Lensblur,
                    icon_name: "lens-blur".into(),
                    tooltip: None,
                    shortcut: None,
                },
            ],
            vec![GroupableTool {
                tool: Tools::Image,
                icon_name: "image-regular".into(),
                tooltip: None,
                shortcut: None,
            }],
        ];

        // Set initial active button correctly
        let initial_tool = APP_CONFIG.read().initial_tool();
        let mut tool_buttons = FactoryVecDeque::<ToolGroupButton>::builder()
            .launch(relm4::gtk::Box::default())
            .detach();

        let mut guard = tool_buttons.guard();
        for mut tg in tools {
            for g in &mut tg {
                let command = ShortcutCommand::SelectTool(g.tool);
                g.tooltip = Some(get_hint(&shortcut_registry, command));
                g.shortcut = shortcut_registry.get_binding_for_command(command);
            }
            let init = ToolGroupInit {
                group: tg,
                initial_tool,
            };
            guard.push_back(init);
        }
        drop(guard);

        let model = ToolsToolbar {
            visible: !APP_CONFIG.read().default_hide_toolbars(),
            tool_buttons,
            tool_action: tool_action.clone().into(),
        };

        let tools_box = model.tool_buttons.widget();
        let widgets = view_output!();

        type SC = ShortcutCommand;
        let other_commands = vec![
            (SC::Scale(100), &widgets.original_scale_button),
            (SC::Scale(0), &widgets.fit_to_window_button),
            (SC::ClearAll, &widgets.reset_button),
            (SC::Undo, &widgets.undo_button),
            (SC::Redo, &widgets.redo_button),
            // in between are the tools
            (
                SC::RunAction(Action::SaveToClipboard),
                &widgets.copy_to_clipboard_button,
            ),
            (SC::RunAction(Action::SaveToFile), &widgets.save_button),
            (SC::RunAction(Action::SaveToFileAs), &widgets.save_as_button),
        ];

        for (command, button) in other_commands {
            update_hint(&shortcut_registry, button, command);
        }

        let mut group = RelmActionGroup::<ToolsToolbarActionGroup>::new();
        group.add_action(tool_action);
        group.register_for_widget(&widgets.root);

        ComponentParts { model, widgets }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ColorButtons {
    Palette(u64),
    Custom,
}

impl StyleToolbar {
    /// `hint` with whether `field` is the one of the active tool or the general one.
    fn scope_tooltip(&self, hint: &str, field: StyleField) -> String {
        let tool = self.scope_tool;
        let name = field.name();
        if self.scope_own.contains(field) {
            format!("{hint}\n{tool}: own {name}, Ctrl+click: use the general one")
        } else {
            format!("{hint}\n{tool}: general {name}, Ctrl+click: own one for {tool}")
        }
    }

    /// Shows the triangles of the fields the active tool has itself, the others
    /// only on hover; for the color only on the selected button.
    fn update_own_indicators(&self) {
        for (indicator, image) in &self.own_indicators {
            image.set_visible(match indicator {
                OwnIndicator::Color(button) => *button == self.selected_color,
                OwnIndicator::Size | OwnIndicator::Fill | OwnIndicator::RoundCaps => true,
            });
            if self.scope_own.contains(indicator.field()) {
                image.add_css_class("own");
            } else {
                image.remove_css_class("own");
            }
        }
    }

    fn update_palette_tooltips(&self) {
        for (button, hint) in &self.palette_buttons {
            button.set_tooltip_text(Some(&self.scope_tooltip(hint, StyleField::Color)));
        }
    }

    fn show_color_dialog(&self, sender: ComponentSender<StyleToolbar>, root: Option<Window>) {
        let current_color: RGBA = self.custom_color;
        relm4::spawn_local(async move {
            let mut builder = ColorChooserDialog::builder()
                .modal(true)
                .title("Choose Color")
                .hide_on_close(true)
                .rgba(&current_color);

            if let Some(w) = root {
                builder = builder.transient_for(&w);
            }

            // build dialog and configure further
            let dialog = builder.build();
            dialog.set_use_alpha(true);

            let custom_colors = APP_CONFIG
                .read()
                .color_palette()
                .custom()
                .iter()
                .copied()
                .map(RGBA::from)
                .collect::<Vec<_>>();

            if !custom_colors.is_empty() {
                dialog.add_palette(
                    gtk::Orientation::Horizontal,
                    8, // A more or less arbitrary, but reasonable value.
                    &custom_colors,
                );
            }

            // set callback for result
            let dialog_copy = dialog.clone();
            dialog.connect_response(move |_, r| {
                if r == ResponseType::Ok {
                    dialog_copy.hide();
                    let color = dialog_copy.rgba();
                    sender.input(StyleToolbarInput::ColorDialogFinished(Some(color)));
                } else if r == ResponseType::Cancel || r == ResponseType::Close {
                    dialog_copy.hide();
                }
            });

            dialog.show();
        });
    }

    fn map_button_to_color(&self, button: ColorButtons) -> Color {
        let config = APP_CONFIG.read();
        match button {
            ColorButtons::Palette(n) => config.color_palette().palette()[n as usize],
            ColorButtons::Custom => self.custom_color.into(),
        }
    }
}

#[relm4::component(pub)]
impl Component for StyleToolbar {
    type Init = ();
    type Input = StyleToolbarInput;
    type Output = ToolbarEvent;
    type CommandOutput = ();

    view! {
        root = gtk::Box {
            set_orientation: gtk::Orientation::Horizontal,
            set_spacing: 2,
            set_valign: Align::End,
            set_halign: Align::Center,
            add_css_class: "toolbar",
            add_css_class: "toolbar-bottom",

            #[watch]
            set_visible: model.visible,

            gtk::Separator {},
            #[name(custom_color_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                gtk::Image::from_pixbuf(Some(&model.custom_color_pixbuf)) {
                    #[watch]
                    set_from_pixbuf: Some(&model.custom_color_pixbuf)
                },
                ActionablePlus::set_action::<ColorAction>: ColorButtons::Custom,
                #[watch]
                set_tooltip: &model.scope_tooltip("custom color", StyleField::Color),
            },
            gtk::Button {

                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "color-regular",
                set_tooltip: "Pick custom color",

                connect_clicked => StyleToolbarInput::ShowColorDialog,
            },
            gtk::Separator {},
            #[name(size_small_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,
                add_css_class: "size-button",

                set_label: "S",
                #[watch]
                set_tooltip: &model.scope_tooltip(
                    &format!("{}: {}", model.size_hints[0], model.size_presets[0].value()),
                    StyleField::Size,
                ),
                #[watch]
                set_active: model.size == model.size_presets[0],
                // only the active one has a background
                #[watch]
                set_has_frame: model.size == model.size_presets[0],
                // via input, so the toggle state is restored when the size does not change
                connect_clicked => StyleToolbarInput::SizePresetClicked(SizePreset::Small),
            },
            #[name(size_medium_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,
                add_css_class: "size-button",

                set_label: "M",
                #[watch]
                set_tooltip: &model.scope_tooltip(
                    &format!("{}: {}", model.size_hints[1], model.size_presets[1].value()),
                    StyleField::Size,
                ),
                #[watch]
                set_active: model.size == model.size_presets[1],
                // only the active one has a background
                #[watch]
                set_has_frame: model.size == model.size_presets[1],
                // via input, so the toggle state is restored when the size does not change
                connect_clicked => StyleToolbarInput::SizePresetClicked(SizePreset::Medium),
            },
            #[name(size_large_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,
                add_css_class: "size-button",

                set_label: "L",
                #[watch]
                set_tooltip: &model.scope_tooltip(
                    &format!("{}: {}", model.size_hints[2], model.size_presets[2].value()),
                    StyleField::Size,
                ),
                #[watch]
                set_active: model.size == model.size_presets[2],
                // only the active one has a background
                #[watch]
                set_has_frame: model.size == model.size_presets[2],
                // via input, so the toggle state is restored when the size does not change
                connect_clicked => StyleToolbarInput::SizePresetClicked(SizePreset::Large),
            },
            #[name(size_spin_button)]
            gtk::SpinButton {
                add_css_class: "size-spin",
                set_focusable: true,
                set_hexpand: false,
                set_adjustment: &gtk::Adjustment::new(
                    model.size.value() as f64,
                    0.0, Size::MAX as f64, // min, max
                    1.0, 10.0, // step sizes
                    0.0),
                set_climb_rate: 1.0,
                set_numeric: true,
                set_digits: 1,
                set_width_chars: 5,
                set_alignment: 1.0,

                connect_value_changed[sender, updating = model.size_spin_updating.clone()] => move |spin_button| {
                    if updating.get() {
                        return;
                    }
                    let new_value = Size::new(spin_button.value() as f32);
                    sender.output_sender().emit(ToolbarEvent::SizeSelected(new_value));
                },

                add_controller = gtk::EventControllerKey {
                    set_propagation_phase: gtk::PropagationPhase::Capture,
                    connect_key_pressed[sender, size_spin_button] => move |_, keyval, _, _| {
                        if matches!(keyval, Key::Escape | Key::Return | Key::KP_Enter | Key::ISO_Enter) {
                            sender.output_sender().emit(ToolbarEvent::FocusCanvas);
                            return relm4::gtk::glib::Propagation::Stop;
                        }

                        if matches!(keyval, Key::Shift_L | Key::Shift_R) {
                            size_spin_button.adjustment().set_step_increment(0.1);
                        } else if matches!(keyval, Key::Control_L | Key::Control_R) {
                            size_spin_button.adjustment().set_step_increment(10.0);
                        }

                        relm4::gtk::glib::Propagation::Proceed
                    },

                    connect_key_released[size_spin_button] => move |_, keyval, _, _| {
                        if matches!(keyval, Key::Shift_L | Key::Shift_R| Key::Control_L | Key::Control_R) {
                            size_spin_button.adjustment().set_step_increment(1.0);
                        }
                    },
                },
            },
            gtk::Separator {},
            gtk::Label {
                set_focusable: false,
                set_hexpand: false,
                set_margin_start: 0,
                set_width_chars: 9,

                #[watch]
                set_text: &model.output_dimensions,
                set_tooltip: "Output dimensions (width x height)",
            },
            gtk::Label {
                set_focusable: false,
                set_hexpand: false,
                set_margin_start: 0,
                set_width_chars: 4,

                #[watch]
                set_text: &model.aspect_ratio,
                set_tooltip: "Aspect Ratio",
            },
            gtk::Separator {},
            #[name(fill_button)]
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,

                #[watch]
                set_tooltip: &model.scope_tooltip(&model.fill_hint, StyleField::Fill),
                #[watch]
                set_icon_name: if model.fill_enabled {
                    "paint-bucket-filled"
                } else {
                    "paint-bucket-regular"
                },
                connect_clicked[sender] => move |_| {
                    sender.output_sender().emit(ToolbarEvent::ToggleFill);
                },
            },
            gtk::Separator {},
            #[name(round_caps_button)]
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,
                #[watch]
                set_tooltip: &model.scope_tooltip(&model.round_caps_hint, StyleField::RoundCaps),
                #[watch]
                set_icon_name: if model.round_caps_enabled {
                    "circle-line-filled"
                } else {
                    "square-filled"
                },
                connect_clicked[sender] => move |_| {
                    sender.output_sender().emit(ToolbarEvent::ToggleRoundCaps);
                },
            },
        },
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
        match message {
            StyleToolbarInput::ShowColorDialog => {
                self.show_color_dialog(sender, root.toplevel_window());
            }
            StyleToolbarInput::ColorDialogFinished(color) => {
                if let Some(color) = color {
                    self.custom_color = color;
                    self.custom_color_pixbuf = create_icon_pixbuf(color.into());

                    // set the custom button active
                    self.color_action
                        .change_state(&ColorButtons::Custom.to_variant());
                    self.selected_color = ColorButtons::Custom;
                    self.update_own_indicators();

                    // set new color
                    sender
                        .output_sender()
                        .emit(ToolbarEvent::ColorSelected(color.into()));
                }
            }
            StyleToolbarInput::ColorButtonSelected(button) => {
                let color = self.map_button_to_color(button);
                self.color_action.change_state(&button.to_variant());
                self.selected_color = button;
                self.update_own_indicators();
                sender
                    .output_sender()
                    .emit(ToolbarEvent::ColorSelected(color));
            }
            StyleToolbarInput::SetColor(color) => {
                let palette_match = APP_CONFIG
                    .read()
                    .color_palette()
                    .palette()
                    .iter()
                    .position(|&p| p == color)
                    .map(|index| ColorButtons::Palette(index as u64))
                    .unwrap_or(ColorButtons::Custom);

                // Only update custom_color if this is not a palette color
                if matches!(palette_match, ColorButtons::Custom) {
                    self.custom_color = color.into();
                    self.custom_color_pixbuf = create_icon_pixbuf(color);
                }

                self.color_action.change_state(&palette_match.to_variant());
                self.selected_color = palette_match;
                self.update_own_indicators();
            }
            StyleToolbarInput::SetFill(fill_enabled) => {
                self.fill_enabled = fill_enabled;
            }
            StyleToolbarInput::SetRoundCaps(round_caps_enabled) => {
                self.round_caps_enabled = round_caps_enabled;
            }
            StyleToolbarInput::SetSize(size) => {
                self.size = size;
                self.size_spin_updating.set(true);
                self.size_spin_button.set_value(size.value() as f64);
                self.size_spin_updating.set(false);
            }
            StyleToolbarInput::SetSizePresets(presets) => {
                self.size_presets = presets;
            }
            StyleToolbarInput::SetStyleScope(tool, own) => {
                self.scope_tool = tool;
                self.scope_own = own;
                self.size_spin_button.set_tooltip_text(Some(
                    &self.scope_tooltip(&self.size_spin_hint, StyleField::Size),
                ));
                self.update_palette_tooltips();
                self.update_own_indicators();
            }
            StyleToolbarInput::ToggleStyleFieldOwn(field) => {
                sender.output_sender().emit(ToolbarEvent::SetStyleFieldOwn(
                    field,
                    !self.scope_own.contains(field),
                ));
            }
            StyleToolbarInput::SizePresetClicked(preset) => {
                sender
                    .output_sender()
                    .emit(ToolbarEvent::SizePresetSelected(preset));
            }
            StyleToolbarInput::SetVisibility(visible) => self.visible = visible,
            StyleToolbarInput::ToggleVisibility => {
                self.visible = !self.visible;
            }
            StyleToolbarInput::DimensionsChanged(size) => {
                self.output_dimensions = format!("{}x{}", size.x as u32, size.y as u32);
                if size.x.abs() < f32::EPSILON || size.y.abs() < f32::EPSILON {
                    self.aspect_ratio = "".to_string();
                    return;
                }
                let aspect_ratio = size.x / size.y;
                let config = APP_CONFIG.read();
                let closest_ar = get_closest_aspect_ratio(aspect_ratio, config.aspect_ratios());
                let delta = (aspect_ratio - closest_ar.0 / closest_ar.1).abs();
                if delta > 0.1 {
                    self.aspect_ratio = "".to_string();
                } else {
                    self.aspect_ratio = format!(
                        "{}{}:{}",
                        if delta > 0.01 { "~" } else { "" },
                        closest_ar.0,
                        closest_ar.1
                    );
                }
            }
            StyleToolbarInput::LineDimensionsChanged(delta) => {
                (self.output_dimensions, self.aspect_ratio) = format_line_dimensions(delta);
            }
            StyleToolbarInput::SetToolEditing(editing) => {
                self.editing = editing;
            }
            StyleToolbarInput::FocusSize => {
                self.size_spin_button.grab_focus();
            }
        }
    }

    fn init(
        _: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let shortcut_registry = ShortcutRegistry::from_config();

        let mut palette_buttons = Vec::new();
        let mut own_indicators = Vec::new();
        for (i, &color) in APP_CONFIG
            .read()
            .color_palette()
            .palette()
            .iter()
            .enumerate()
            .rev()
        {
            let btn = gtk::ToggleButton::builder()
                .focusable(false)
                .hexpand(false)
                .child(&create_icon(color))
                .build();
            btn.set_action::<ColorAction>(ColorButtons::Palette(i as u64));

            let color_tooltip = match shortcut_registry
                .get_binding_for_command(ShortcutCommand::SelectColorIndex(i as u64))
            {
                Some(hint) => format!("color {} ({hint})", i + 1),
                None => format!("color {}", i + 1),
            };
            root.prepend(&btn);
            let indicator = OwnIndicator::Color(ColorButtons::Palette(i as u64));
            own_indicators.push((indicator, add_own_indicator(&btn, &sender, indicator)));
            palette_buttons.push((btn, color_tooltip));
        }

        // Color Action for selecting colors
        let sender_tmp: ComponentSender<StyleToolbar> = sender.clone();
        let color_action: RelmAction<ColorAction> = RelmAction::new_stateful_with_target_value(
            &ColorButtons::Palette(0),
            move |_, state, value| {
                *state = value;
                sender_tmp.input(StyleToolbarInput::ColorButtonSelected(value));
            },
        );

        let (initial_tool, initial_style, initial_own) = {
            let config = APP_CONFIG.read();
            let tool = config.initial_tool();
            (tool, config.style_for(tool), config.own_style_fields(tool))
        };

        let custom_color = APP_CONFIG
            .read()
            .color_palette()
            .custom()
            .first()
            .copied()
            .unwrap_or(Color::red());
        let custom_color_pixbuf = create_icon_pixbuf(custom_color);

        // create model
        let mut model = StyleToolbar {
            custom_color: custom_color.into(),
            custom_color_pixbuf,
            color_action: SimpleAction::from(color_action.clone()),
            size: initial_style.size,
            size_presets: initial_style.presets,
            size_hints: SizePreset::ALL
                .map(|p| get_hint(&shortcut_registry, ShortcutCommand::SelectSize(p))),
            size_spin_button: gtk::SpinButton::new(None::<&gtk::Adjustment>, 1.0, 1),
            size_spin_updating: Rc::new(Cell::new(false)),
            scope_tool: initial_tool,
            scope_own: initial_own,
            palette_buttons,
            own_indicators,
            selected_color: ColorButtons::Palette(0),
            size_spin_hint: get_hint(&shortcut_registry, ShortcutCommand::FocusSize),
            fill_hint: get_hint(&shortcut_registry, ShortcutCommand::ToggleFill),
            round_caps_hint: get_hint(&shortcut_registry, ShortcutCommand::ToggleRoundCaps),
            fill_enabled: initial_style.fill,
            round_caps_enabled: initial_style.round_caps,
            visible: !APP_CONFIG.read().default_hide_toolbars(),
            output_dimensions: String::new(),
            aspect_ratio: String::new(),
            editing: false,
        };

        // create widgets
        let widgets = view_output!();
        model.size_spin_button = widgets.size_spin_button.clone();

        model.update_palette_tooltips();
        widgets.size_spin_button.set_tooltip_text(Some(
            &model.scope_tooltip(&model.size_spin_hint, StyleField::Size),
        ));
        let buttons: [(&gtk::Widget, OwnIndicator); 4] = [
            (
                widgets.custom_color_button.upcast_ref(),
                OwnIndicator::Color(ColorButtons::Custom),
            ),
            (widgets.size_spin_button.upcast_ref(), OwnIndicator::Size),
            (widgets.fill_button.upcast_ref(), OwnIndicator::Fill),
            (
                widgets.round_caps_button.upcast_ref(),
                OwnIndicator::RoundCaps,
            ),
        ];
        for (button, indicator) in buttons {
            let image = add_own_indicator(button, &sender, indicator);
            model.own_indicators.push((indicator, image));
        }
        // the size has its triangle on the size field, S, M and L only Ctrl+click
        for button in [
            &widgets.size_small_button,
            &widgets.size_medium_button,
            &widgets.size_large_button,
        ] {
            connect_own_style_toggle(button, &sender, StyleField::Size);
        }
        model.update_own_indicators();

        let mut group = RelmActionGroup::<StyleToolbarActionGroup>::new();
        group.add_action(color_action);

        group.register_for_widget(&widgets.root);

        // select the button of the initial color, may be a custom one
        sender.input(StyleToolbarInput::SetColor(initial_style.color));

        ComponentParts { model, widgets }
    }
}
relm4::new_action_group!(pub ToolsToolbarActionGroup, "tools-toolbars");
relm4::new_stateful_action!(pub ToolsAction, ToolsToolbarActionGroup, "tools", Tools, Tools);

relm4::new_action_group!(StyleToolbarActionGroup, "style-toolbars");
relm4::new_stateful_action!(
    ColorAction,
    StyleToolbarActionGroup,
    "colors",
    ColorButtons,
    ColorButtons
);

impl Clone for ColorAction {
    fn clone(&self) -> Self {
        Self {}
    }
}

impl StaticVariantType for ColorButtons {
    fn static_variant_type() -> Cow<'static, VariantTy> {
        Cow::Borrowed(VariantTy::UINT64)
    }
}

impl ToVariant for ColorButtons {
    fn to_variant(&self) -> Variant {
        Variant::from(match *self {
            Self::Palette(i) => i,
            Self::Custom => u64::MAX,
        })
    }
}

impl FromVariant for ColorButtons {
    fn from_variant(variant: &Variant) -> Option<Self> {
        <u64>::from_variant(variant).map(|v| match v {
            u64::MAX => Self::Custom,
            _ => Self::Palette(v),
        })
    }
}
