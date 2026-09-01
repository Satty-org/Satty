use std::borrow::Cow;

use crate::{
    configuration::{APP_CONFIG, Action},
    keybindings::{ShortcutCommand, ShortcutRegistry},
    style::{Color, Size},
    tools::Tools,
};

use crate::tools::GroupableTool;
use crate::ui::tool_group_button::{ToolGroupButton, ToolGroupButtonInput, ToolGroupInit};
use relm4::gtk::{
    gdk::Key,
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
    custom_color: Color,
    custom_color_pixbuf: Pixbuf,
    color_action: SimpleAction,
    size_action: SimpleAction,
    size_spin_button: gtk::SpinButton,
    fill_enabled: bool,
    round_caps_enabled: bool,
    visible: bool,
    output_dimensions: String,
    editing: bool,
}

#[derive(Debug, Copy, Clone)]
pub enum ToolbarEvent {
    FocusCanvas,
    ToolSelected(Tools),
    ColorSelected(Color),
    SetFill(bool),
    SizeSelected(Size),
    AnnotationSizeFactorChanged(f32),
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
    SetAnnotationSizeFactor(f32),
    ShowColorDialog,
    ColorDialogFinished(Option<Color>),
    SetVisibility(bool),
    ToggleVisibility,
    DimensionsChanged((i32, i32)),
    SetToolEditing(bool),
    FocusAnnotationSizeFactor,
}

fn create_icon_pixbuf(color: Color) -> Pixbuf {
    let pixbuf = Pixbuf::new(relm4::gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, 40, 40).unwrap();
    pixbuf.fill(color.to_rgba_u32());
    pixbuf
}

fn create_icon(color: Color) -> gtk::Image {
    gtk::Image::from_pixbuf(Some(&create_icon_pixbuf(color)))
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
            }],
            vec![GroupableTool {
                tool: Tools::Crop,
                icon_name: "crop-filled".into(),
                tooltip: None,
            }],
            vec![GroupableTool {
                tool: Tools::Brush,
                icon_name: "pen-regular".into(),
                tooltip: None,
            }],
            vec![
                GroupableTool {
                    tool: Tools::Line,
                    icon_name: "minus-large".into(),
                    tooltip: None,
                },
                GroupableTool {
                    tool: Tools::Arrow,
                    icon_name: "arrow-up-right-filled".into(),
                    tooltip: None,
                },
            ],
            vec![
                GroupableTool {
                    tool: Tools::Rectangle,
                    icon_name: "checkbox-unchecked-regular".into(),
                    tooltip: None,
                },
                GroupableTool {
                    tool: Tools::Ellipse,
                    icon_name: "circle-regular".into(),
                    tooltip: None,
                },
            ],
            vec![GroupableTool {
                tool: Tools::Text,
                icon_name: "text-case-title-regular".into(),
                tooltip: None,
            }],
            vec![GroupableTool {
                tool: Tools::Marker,
                icon_name: "number-circle-1-regular".into(),
                tooltip: None,
            }],
            vec![GroupableTool {
                tool: Tools::Blur,
                icon_name: "drop-regular".into(),
                tooltip: None,
            }],
            vec![GroupableTool {
                tool: Tools::Highlight,
                icon_name: "highlight-regular".into(),
                tooltip: None,
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
                g.tooltip = Some(get_hint(
                    &shortcut_registry,
                    ShortcutCommand::SelectTool(g.tool),
                ));
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

#[derive(Debug, Copy, Clone)]
pub enum ColorButtons {
    Palette(u64),
    Custom,
}

impl StyleToolbar {
    fn show_color_dialog(&self, sender: ComponentSender<StyleToolbar>, root: Option<Window>) {
        let current_color: RGBA = self.custom_color.into();
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
                    let color = Color::from_gdk(dialog_copy.rgba());
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
            ColorButtons::Custom => self.custom_color,
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

                set_label: "S",
                set_tooltip: "Small size",
                ActionablePlus::set_action::<SizeAction>: Size::Small,
            },
            #[name(size_medium_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_label: "M",
                set_tooltip: "Medium size",
                ActionablePlus::set_action::<SizeAction>: Size::Medium,
            },
            #[name(size_large_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_label: "L",
                set_tooltip: "Large size",
                ActionablePlus::set_action::<SizeAction>: Size::Large,
            },
            gtk::Label {
                set_focusable: false,
                set_hexpand: false,

                set_text: "x",
            },
            #[name(size_spin_button)]
            gtk::SpinButton {
                set_focusable: true,
                set_hexpand: false,
                set_tooltip: "Edit Annotation Size Factor",
                set_adjustment: &gtk::Adjustment::new(
                    APP_CONFIG.read().annotation_size_factor() as f64,
                    0.1, 99.99, // min, max
                    0.1, 1.0, // step sizes
                    0.0),
                set_climb_rate: 0.1,
                set_numeric: true,
                set_digits: 2,
                set_width_chars: 4,
                set_alignment: 1.0,

                connect_value_changed[sender] => move |spin_button| {
                    let new_value = spin_button.value() as f32;
                    sender.output_sender().emit(ToolbarEvent::AnnotationSizeFactorChanged(new_value));
                },

                add_controller = gtk::EventControllerKey {
                    set_propagation_phase: gtk::PropagationPhase::Capture,
                    connect_key_pressed[sender, size_spin_button] => move |_, keyval, _, _| {
                        if matches!(keyval, Key::Escape | Key::Return | Key::KP_Enter | Key::ISO_Enter) {
                            sender.output_sender().emit(ToolbarEvent::FocusCanvas);
                            return relm4::gtk::glib::Propagation::Stop;
                        }

                        if matches!(keyval, Key::Shift_L | Key::Shift_R) {
                            size_spin_button.adjustment().set_step_increment(0.01);
                        } else if matches!(keyval, Key::Control_L | Key::Control_R) {
                            size_spin_button.adjustment().set_step_increment(1.0);
                        }

                        relm4::gtk::glib::Propagation::Proceed
                    },

                    connect_key_released[size_spin_button] => move |_, keyval, _, _| {
                        if matches!(keyval, Key::Shift_L | Key::Shift_R| Key::Control_L | Key::Control_R) {
                            size_spin_button.adjustment().set_step_increment(0.1);
                        }
                    },
                },
            },
            gtk::Separator {},
            gtk::Label {
                set_focusable: false,
                set_hexpand: false,
                set_margin_start: 10,
                set_width_chars: 11,

                #[watch]
                set_text: &model.output_dimensions,
                set_tooltip: "Output dimensions (width x height)",
            },
            gtk::Separator {},
            #[name(fill_button)]
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,

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
                set_tooltip: "Toggle Round Caps",
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
                    self.custom_color_pixbuf = create_icon_pixbuf(color);

                    // set the custom button active
                    self.color_action
                        .change_state(&ColorButtons::Custom.to_variant());

                    // set new color
                    sender
                        .output_sender()
                        .emit(ToolbarEvent::ColorSelected(color));
                }
            }
            StyleToolbarInput::ColorButtonSelected(button) => {
                let color = self.map_button_to_color(button);
                self.color_action.change_state(&button.to_variant());
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
                    self.custom_color = color;
                    self.custom_color_pixbuf = create_icon_pixbuf(color);
                }

                self.color_action.change_state(&palette_match.to_variant());
            }
            StyleToolbarInput::SetFill(fill_enabled) => {
                self.fill_enabled = fill_enabled;
            }
            StyleToolbarInput::SetRoundCaps(round_caps_enabled) => {
                self.round_caps_enabled = round_caps_enabled;
            }
            StyleToolbarInput::SetSize(size) => {
                self.size_action.change_state(&size.to_variant());
            }
            StyleToolbarInput::SetAnnotationSizeFactor(value) => {
                self.size_spin_button.set_value(value as f64);
            }
            StyleToolbarInput::SetVisibility(visible) => self.visible = visible,
            StyleToolbarInput::ToggleVisibility => {
                self.visible = !self.visible;
            }
            StyleToolbarInput::DimensionsChanged((width, height)) => {
                self.output_dimensions = format!("{}x{}", width, height);
            }
            StyleToolbarInput::SetToolEditing(editing) => {
                self.editing = editing;
            }
            StyleToolbarInput::FocusAnnotationSizeFactor => {
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
            btn.set_tooltip_text(Some(&color_tooltip));

            root.prepend(&btn);
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

        // Size Action for selecting sizes
        let sender_tmp = sender.clone();
        let size_action: RelmAction<SizeAction> =
            RelmAction::new_stateful_with_target_value(&Size::Medium, move |_, state, value| {
                *state = value;
                sender_tmp
                    .output_sender()
                    .emit(ToolbarEvent::SizeSelected(*state));
            });

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
            custom_color,
            custom_color_pixbuf,
            color_action: SimpleAction::from(color_action.clone()),
            size_action: SimpleAction::from(size_action.clone()),
            size_spin_button: gtk::SpinButton::new(None::<&gtk::Adjustment>, 0.1, 2),
            fill_enabled: APP_CONFIG.read().default_fill_shapes(),
            round_caps_enabled: APP_CONFIG.read().default_round_caps(),
            visible: !APP_CONFIG.read().default_hide_toolbars(),
            output_dimensions: String::new(),
            editing: false,
        };

        // create widgets
        let widgets = view_output!();
        model.size_spin_button = widgets.size_spin_button.clone();

        update_hint(
            &shortcut_registry,
            &widgets.size_small_button,
            ShortcutCommand::SelectSize(Size::Small),
        );
        update_hint(
            &shortcut_registry,
            &widgets.size_medium_button,
            ShortcutCommand::SelectSize(Size::Medium),
        );
        update_hint(
            &shortcut_registry,
            &widgets.size_large_button,
            ShortcutCommand::SelectSize(Size::Large),
        );
        update_hint(
            &shortcut_registry,
            &widgets.size_spin_button,
            ShortcutCommand::FocusAnnotationSizeFactor,
        );
        update_hint(
            &shortcut_registry,
            &widgets.fill_button,
            ShortcutCommand::ToggleFill,
        );
        update_hint(
            &shortcut_registry,
            &widgets.round_caps_button,
            ShortcutCommand::ToggleRoundCaps,
        );

        let mut group = RelmActionGroup::<StyleToolbarActionGroup>::new();
        group.add_action(color_action);
        group.add_action(size_action);

        group.register_for_widget(&widgets.root);

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

relm4::new_stateful_action!(SizeAction, StyleToolbarActionGroup, "sizes", Size, Size);

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
            std::u64::MAX => Self::Custom,
            _ => Self::Palette(v),
        })
    }
}
