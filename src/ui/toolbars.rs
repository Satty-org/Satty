use std::{borrow::Cow, collections::HashMap};

use crate::{
    configuration::APP_CONFIG,
    style::{Color, Size},
    tools::Tools,
};

use gdk_pixbuf::{
    gio::SimpleAction,
    glib::{Variant, VariantTy},
    Pixbuf,
};
use gtk::ToggleButton;
use relm4::{
    actions::{ActionablePlus, RelmAction, RelmActionGroup},
    gtk::{gdk::RGBA, prelude::*, Align, ColorChooserDialog, ResponseType, Window},
    prelude::*,
};

pub struct ToolsToolbar {
    visible: bool,
    active_button: Option<ToggleButton>,
    tool_buttons: HashMap<Tools, ToggleButton>,
    tool_action: SimpleAction,
}

pub struct StyleToolbar {
    custom_color: Color,
    custom_color_pixbuf: Pixbuf,
    color_action: SimpleAction,
    visible: bool,
    annotation_size: f32,
    annotation_size_formatted: String,
    annotation_dialog_controller: Option<Controller<AnnotationSizeDialog>>,
}

pub struct AnnotationSizeDialog {
    annotation_size: f32,
}

#[derive(Debug, Copy, Clone)]
pub enum ToolbarEvent {
    ToolSelected(Tools),
    ColorSelected(Color),
    SizeSelected(Size),
    Redo,
    Undo,
    SaveFile,
    CopyClipboard,
    ToggleFill,
    AnnotationSizeChanged(f32),
    Reset,
    SaveFileAs,
    Resize,
    OriginalScale,
}

#[derive(Debug, Copy, Clone)]
pub enum ToolsToolbarInput {
    SetVisibility(bool),
    ToggleVisibility,
    SwitchSelectedTool(Tools),
}

#[derive(Debug, Copy, Clone)]
pub enum StyleToolbarInput {
    ColorButtonSelected(ColorButtons),
    ShowColorDialog,
    ColorDialogFinished(Option<Color>),
    SetVisibility(bool),
    ToggleVisibility,
    ShowAnnotationDialog,
    AnnotationDialogFinished(Option<f32>),
}

#[derive(Debug, Copy, Clone)]
pub enum AnnotationSizeDialogInput {
    ValueChanged(f32),
    Reset,
    Show(f32),
    Submit,
    Cancel,
}

#[derive(Debug, Copy, Clone)]
pub enum AnnotationSizeDialogOutput {
    AnnotationSizeSubmitted(f32),
}

fn create_icon_pixbuf(color: Color) -> Pixbuf {
    let pixbuf = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, 40, 40).unwrap();
    pixbuf.fill(color.to_rgba_u32());
    pixbuf
}
fn create_icon(color: Color) -> gtk::Image {
    gtk::Image::from_pixbuf(Some(&create_icon_pixbuf(color)))
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

            gtk::Button {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "resize-large-regular",
                set_tooltip: "1:1",
                connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::OriginalScale);},
            },
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "page-fit-regular",
                set_tooltip: "Resize",
                connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::Resize);},
            },
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "recycling-bin",
                set_tooltip: "Reset",
                connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::Reset);},
            },
            gtk::Separator {},
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "arrow-undo-filled",
                set_tooltip: "Undo (Ctrl-Z)",
                connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::Undo);},
            },
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "arrow-redo-filled",
                set_tooltip: "Redo (Ctrl-Y)",
                connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::Redo);},
            },
            gtk::Separator {},
            #[name(pointer_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "cursor-regular",
                // tooltip set programatically
                ActionablePlus::set_action::<ToolsAction>: Tools::Pointer,
            },
            #[name(crop_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "crop-filled",
                // tooltip set programatically
                ActionablePlus::set_action::<ToolsAction>: Tools::Crop,
            },
            #[name(brush_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "pen-regular",
                // tooltip set programatically
                ActionablePlus::set_action::<ToolsAction>: Tools::Brush,
            },
            #[name(line_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "minus-large",
                // tooltip set programatically
                ActionablePlus::set_action::<ToolsAction>: Tools::Line,
            },
            #[name(arrow_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "arrow-up-right-filled",
                // tooltip set programatically
                ActionablePlus::set_action::<ToolsAction>: Tools::Arrow,
            },
            #[name(rectangle_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "checkbox-unchecked-regular",
                // tooltip set programatically
                ActionablePlus::set_action::<ToolsAction>: Tools::Rectangle,
            },
            #[name(ellipse_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "circle-regular",
                // tooltip set programatically
                ActionablePlus::set_action::<ToolsAction>: Tools::Ellipse,
            },
            #[name(text_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "text-case-title-regular",
                // tooltip set programatically
                ActionablePlus::set_action::<ToolsAction>: Tools::Text,
            },
            #[name(marker_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "number-circle-1-regular",
                // tooltip set programatically
                ActionablePlus::set_action::<ToolsAction>: Tools::Marker,
            },
            #[name(blur_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "drop-regular",
                // tooltip set programatically
                ActionablePlus::set_action::<ToolsAction>: Tools::Blur,
            },
            #[name(highlight_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "highlight-regular",
                // tooltip set programatically
                ActionablePlus::set_action::<ToolsAction>: Tools::Highlight,
            },
            gtk::Separator {},
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "copy-regular",
                set_tooltip: "Copy to clipboard (Ctrl+C)",
                connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::CopyClipboard);},
            },
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "save-regular",
                set_tooltip: "Save (Ctrl+S)",
                connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::SaveFile);},

                set_visible: APP_CONFIG.read().output_filename().is_some()
            },
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "save-multiple-regular",
                set_tooltip: "Save as (Ctrl+Shift+S)",
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

                if let Some(selected_tool_button) = self.tool_buttons.get(&tool) {
                    self.active_button = Some(selected_tool_button.clone());
                }
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
            },
        );

        let mut model = ToolsToolbar {
            visible: !APP_CONFIG.read().default_hide_toolbars(),
            active_button: None,
            tool_buttons: HashMap::new(),
            tool_action: tool_action.clone().into(),
        };
        let widgets = view_output!();

        model.tool_buttons = HashMap::from([
            (Tools::Pointer, widgets.pointer_button.clone()),
            (Tools::Crop, widgets.crop_button.clone()),
            (Tools::Brush, widgets.brush_button.clone()),
            (Tools::Line, widgets.line_button.clone()),
            (Tools::Arrow, widgets.arrow_button.clone()),
            (Tools::Rectangle, widgets.rectangle_button.clone()),
            (Tools::Ellipse, widgets.ellipse_button.clone()),
            (Tools::Text, widgets.text_button.clone()),
            (Tools::Marker, widgets.marker_button.clone()),
            (Tools::Blur, widgets.blur_button.clone()),
            (Tools::Highlight, widgets.highlight_button.clone()),
        ]);

        // reverse shortcuts mapping
        let config = APP_CONFIG.read();
        let tool_to_key_map: HashMap<&Tools, &char> = config
            .keybinds()
            .shortcuts()
            .iter()
            .map(|(k, v)| (v, k))
            .collect();

        // Update tooltips based on configured keybinds
        for (tool, button) in &model.tool_buttons {
            let display_name = tool.display_name();

            let tooltip = if let Some(key) = tool_to_key_map.get(tool) {
                &format!("{} ({})", display_name, key.to_uppercase())
            } else {
                display_name
            };
            button.set_tooltip_text(Some(tooltip));
        }

        // Set initial active button correctly
        let initial_tool = APP_CONFIG.read().initial_tool();
        if let Some(button) = model.tool_buttons.get(&initial_tool) {
            model.active_button = Some(button.clone());
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

    fn show_annotation_dialog(
        &mut self,
        sender: ComponentSender<StyleToolbar>,
        root: Option<Window>,
    ) {
        if self.annotation_dialog_controller.is_none() {
            let mut builder = AnnotationSizeDialog::builder();
            if let Some(w) = root {
                builder = builder.transient_for(&w);
            }

            let connector = builder.launch(self.annotation_size);

            let mut controller = connector.forward(sender.input_sender(), |output| match output {
                AnnotationSizeDialogOutput::AnnotationSizeSubmitted(value) => {
                    StyleToolbarInput::AnnotationDialogFinished(Some(value))
                }
            });

            controller.detach_runtime();
            self.annotation_dialog_controller = Some(controller);
        }

        let ctrl = self.annotation_dialog_controller.as_mut().unwrap();
        ctrl.emit(AnnotationSizeDialogInput::Show(self.annotation_size));
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
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_label: "S",
                set_tooltip: "Small size",
                ActionablePlus::set_action::<SizeAction>: Size::Small,
            },
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_label: "M",
                set_tooltip: "Medium size",
                ActionablePlus::set_action::<SizeAction>: Size::Medium,
            },
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
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,

                #[watch]
                set_label: &model.annotation_size_formatted,
                set_tooltip: "Edit Annotation Size Factor",

                connect_clicked => StyleToolbarInput::ShowAnnotationDialog
            },
            gtk::Separator {},
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: if APP_CONFIG.read().default_fill_shapes() {
                    "paint-bucket-filled"
                } else {
                    "paint-bucket-regular"
                },
                set_tooltip: "Fill shape",
                connect_clicked[sender] => move |button| {
                    sender.output_sender().emit(ToolbarEvent::ToggleFill);
                    let new_icon = if button.icon_name() == Some("paint-bucket-regular".into()) {
                        "paint-bucket-filled"
                    } else {
                        "paint-bucket-regular"
                    };
                    button.set_icon_name(new_icon);
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
                sender
                    .output_sender()
                    .emit(ToolbarEvent::ColorSelected(color));
            }

            StyleToolbarInput::ShowAnnotationDialog => {
                self.show_annotation_dialog(sender, root.toplevel_window());
            }

            StyleToolbarInput::AnnotationDialogFinished(value) => {
                if let Some(value) = value {
                    self.annotation_size = value;
                    self.annotation_size_formatted = format!("{value:.2}");

                    sender
                        .output_sender()
                        .emit(ToolbarEvent::AnnotationSizeChanged(value));
                }
            }

            StyleToolbarInput::SetVisibility(visible) => self.visible = visible,
            StyleToolbarInput::ToggleVisibility => {
                self.visible = !self.visible;
            }
        }
    }

    fn init(
        _: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
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
        let model = StyleToolbar {
            custom_color,
            custom_color_pixbuf,
            color_action: SimpleAction::from(color_action.clone()),
            visible: !APP_CONFIG.read().default_hide_toolbars(),
            annotation_size: APP_CONFIG.read().annotation_size_factor(),
            annotation_size_formatted: format!(
                "{0:.2}",
                APP_CONFIG.read().annotation_size_factor()
            ),
            annotation_dialog_controller: None,
        };

        // create widgets
        let widgets = view_output!();

        let mut group = RelmActionGroup::<StyleToolbarActionGroup>::new();
        group.add_action(color_action);
        group.add_action(size_action);

        group.register_for_widget(&widgets.root);

        ComponentParts { model, widgets }
    }
}
relm4::new_action_group!(ToolsToolbarActionGroup, "tools-toolbars");
relm4::new_stateful_action!(ToolsAction, ToolsToolbarActionGroup, "tools", Tools, Tools);

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

#[relm4::component(pub)]
impl Component for AnnotationSizeDialog {
    type Init = f32;
    type Input = AnnotationSizeDialogInput;
    type Output = AnnotationSizeDialogOutput;
    type CommandOutput = ();

    view! {
        gtk::Window {
            set_modal: true,
            set_title: Some("Choose Annotation Size"),
            set_titlebar: Some(&header_bar),

            #[wrap(Some)]
            set_child = &gtk::Box {
                set_spacing: 10,
                set_margin_all: 12,
                set_orientation: gtk::Orientation::Horizontal,

                #[name = "spin"]
                gtk::SpinButton {
                    set_editable: true,
                    set_can_focus: true,
                    set_hexpand: false,

                    set_tooltip: "Annotation Size Factor",
                    set_numeric: true,
                    set_adjustment: &gtk::Adjustment::new(0.0, 0.0, 100.0, 0.01, 0.1, 0.0),
                    set_climb_rate: 0.1,
                    set_digits: 2,
                    #[watch]
                    #[block_signal(value_changed)]
                    set_value: model.annotation_size.into(),

                    connect_value_changed[sender] => move |button| {
                        sender.input(AnnotationSizeDialogInput::ValueChanged(button.value() as f32));
                        } @value_changed,
                },
                #[name = "spin_reset"]
                gtk::Button {
                    set_focusable: false,
                    set_hexpand: false,

                    set_tooltip: "Reset Annotation Size Factor",
                    set_icon_name: "edit-reset-symbolic",
                    connect_clicked[sender] => move |_| {
                        sender.input(AnnotationSizeDialogInput::Reset);
                    },
                },

            },
        }
    }

    fn init(
        init_value: f32,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = AnnotationSizeDialog {
            annotation_size: init_value,
        };

        // the title bar didn't really work within the view! macro.
        let title_label = gtk::Label::builder()
            .label("Choose Annotation Size")
            .margin_start(6)
            .build();

        let cancel_button = gtk::Button::builder().label("Cancel").build();
        let sender_clone = sender.clone();
        cancel_button.connect_clicked(move |_| {
            sender_clone.input(AnnotationSizeDialogInput::Cancel);
        });

        let ok_button = gtk::Button::builder().label("OK").build();

        let sender_clone = sender.clone();
        ok_button.connect_clicked(move |_| {
            sender_clone.input(AnnotationSizeDialogInput::Submit);
        });

        let header_bar = gtk::HeaderBar::builder().show_title_buttons(false).build();

        header_bar.set_title_widget(Some(&title_label));
        header_bar.pack_start(&cancel_button);
        header_bar.pack_end(&ok_button);

        let widgets = view_output!();

        let key_controller = gtk::EventControllerKey::builder()
            // not sure if this is the correct phase, but anything higher and Enter to close doesn't work consistently
            .propagation_phase(gtk::PropagationPhase::Capture)
            .build();

        key_controller.connect_key_pressed(move |_, keyval, _, _| {
            use gtk::gdk::Key;
            match keyval {
                Key::Return => {
                    sender.input(AnnotationSizeDialogInput::Submit);
                    glib::Propagation::Stop
                }
                Key::Escape => {
                    sender.input(AnnotationSizeDialogInput::Cancel);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        root.add_controller(key_controller);

        ComponentParts { model, widgets }
    }

    fn update(
        &mut self,
        message: AnnotationSizeDialogInput,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            AnnotationSizeDialogInput::ValueChanged(value) => self.annotation_size = value,
            AnnotationSizeDialogInput::Reset => {
                let a = APP_CONFIG.read().annotation_size_factor();
                self.annotation_size = a;
            }
            AnnotationSizeDialogInput::Show(value) => {
                self.annotation_size = value;
                root.show();
            }
            AnnotationSizeDialogInput::Cancel => {
                root.hide();
            }
            AnnotationSizeDialogInput::Submit => {
                // yeah, not sure if this can even happen.
                if let Err(e) = sender.output(AnnotationSizeDialogOutput::AnnotationSizeSubmitted(
                    self.annotation_size,
                )) {
                    eprintln!("Error submitting annotation size factor: {e:?}");
                }
                root.hide();
            }
        }
    }
}
