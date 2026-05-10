use std::{borrow::Cow, collections::HashMap};

use crate::{
    configuration::APP_CONFIG,
    style::{Color, Size},
    tools::{ArrowStyle, Tools},
};

use gtk::ToggleButton;
use relm4::gtk::gdk_pixbuf::{
    Pixbuf,
    gio::SimpleAction,
    glib::{Variant, VariantTy},
};
use relm4::{
    actions::{ActionablePlus, RelmAction, RelmActionGroup},
    gtk::{Align, ColorChooserDialog, ResponseType, Window, gdk::RGBA, prelude::*},
    prelude::*,
};

/// Install a tooltip that re-shows reliably on every hover.
///
/// Why: GTK4's built-in tooltip system keeps a window-level "tooltip
/// recently shown / dismissed" state that only clears when the pointer
/// leaves the toplevel window. Toggling `has-tooltip` or returning true
/// from `query-tooltip` doesn't reset it. So we bypass the tooltip system
/// entirely and drive a per-widget `gtk::Popover` ourselves with motion
/// enter/leave events — popup on enter, popdown on leave. No window-wide
/// state to get stuck.
trait RobustTooltipExt {
    /// Tooltip pops downward (good for top-toolbar buttons).
    fn install_tooltip(&self, text: &str);
    /// Tooltip pops upward (good for bottom-toolbar buttons so it stays
    /// inside the window).
    fn install_tooltip_above(&self, text: &str);
}

impl<T: IsA<gtk::Widget> + Clone> RobustTooltipExt for T {
    fn install_tooltip(&self, text: &str) {
        attach_tooltip(self, text, gtk::PositionType::Bottom);
    }
    fn install_tooltip_above(&self, text: &str) {
        attach_tooltip(self, text, gtk::PositionType::Top);
    }
}

fn attach_tooltip<W: IsA<gtk::Widget> + Clone>(
    widget: &W,
    text: &str,
    position: gtk::PositionType,
) {
    let label = gtk::Label::builder()
        .label(text)
        .margin_start(8)
        .margin_end(8)
        .margin_top(4)
        .margin_bottom(4)
        .build();
    let popover = gtk::Popover::builder()
        .child(&label)
        .has_arrow(false)
        .autohide(false)
        .position(position)
        .build();
    popover.add_css_class("custom-tooltip");
    popover.set_can_focus(false);
    // Don't intercept hover/clicks on the underlying widget.
    popover.set_can_target(false);
    // Push the popover a few pixels away from the widget edge so the
    // text isn't crammed against the toolbar.
    let gap = 8;
    let y_offset = match position {
        gtk::PositionType::Bottom => gap,
        gtk::PositionType::Top => -gap,
        _ => 0,
    };
    popover.set_offset(0, y_offset);
    popover.set_parent(widget);

    let motion = gtk::EventControllerMotion::new();
    {
        let popover = popover.clone();
        motion.connect_enter(move |_, _, _| {
            popover.popup();
        });
    }
    {
        let popover = popover.clone();
        motion.connect_leave(move |_| {
            popover.popdown();
        });
    }
    widget.add_controller(motion);

    // GtkPopover::set_parent attaches the popover as a child of the
    // widget; we have to unparent it explicitly before the parent is
    // finalized or GTK warns on shutdown.
    widget.connect_destroy(move |_| {
        popover.unparent();
    });
}

pub struct ToolsToolbar {
    visible: bool,
    active_button: Option<ToggleButton>,
    tool_buttons: HashMap<Tools, ToggleButton>,
    tool_action: SimpleAction,
    /// Currently-selected color, mirrored on the unified color-picker
    /// MenuButton's swatch. Updated whenever a palette/custom color is
    /// chosen, so the swatch reflects what subsequent annotations will use.
    current_color: Color,
    current_color_pixbuf: Pixbuf,
    custom_color: Color,
    custom_color_pixbuf: Pixbuf,
    color_action: SimpleAction,
}

impl ToolsToolbar {
    fn map_button_to_color(&self, button: ColorButtons) -> Color {
        let config = APP_CONFIG.read();
        match button {
            ColorButtons::Palette(n) => config.color_palette().palette()[n as usize],
            ColorButtons::Custom => self.custom_color,
        }
    }

    fn show_color_dialog(&self, sender: ComponentSender<ToolsToolbar>, root: Option<Window>) {
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
                dialog.add_palette(gtk::Orientation::Horizontal, 8, &custom_colors);
            }

            let dialog_copy = dialog.clone();
            dialog.connect_response(move |_, r| {
                if r == ResponseType::Ok {
                    dialog_copy.hide();
                    let color = Color::from_gdk(dialog_copy.rgba());
                    sender.input(ToolsToolbarInput::ColorDialogFinished(Some(color)));
                } else if r == ResponseType::Cancel || r == ResponseType::Close {
                    dialog_copy.hide();
                }
            });

            dialog.show();
        });
    }
}

/// Build the popover that hangs off the unified color-picker MenuButton.
/// Lays out the palette swatches in a single row + a custom-color row.
fn build_color_popover(
    model: &ToolsToolbar,
    sender: &ComponentSender<ToolsToolbar>,
) -> gtk::Popover {
    let popover = gtk::Popover::builder()
        .has_arrow(true)
        .position(gtk::PositionType::Bottom)
        .build();
    popover.add_css_class("color-picker-popover");

    let outer = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_start(6)
        .margin_end(6)
        .margin_top(6)
        .margin_bottom(6)
        .build();

    let palette_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .build();
    for (i, &color) in APP_CONFIG
        .read()
        .color_palette()
        .palette()
        .iter()
        .enumerate()
    {
        let btn = gtk::ToggleButton::builder()
            .focusable(false)
            .hexpand(false)
            .child(&create_icon(color))
            .build();
        btn.set_action::<ColorAction>(ColorButtons::Palette(i as u64));
        let shortcut = if i < 9 {
            format!("{}", i + 1)
        } else if i == 9 {
            "0".to_string()
        } else {
            String::new()
        };
        let tooltip = if shortcut.is_empty() {
            format!("Color #{}", i + 1)
        } else {
            format!("Color #{} ({})", i + 1, shortcut)
        };
        btn.install_tooltip(&tooltip);
        palette_row.append(&btn);
    }
    outer.append(&palette_row);

    outer.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let custom_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .build();
    let custom_toggle = gtk::ToggleButton::builder()
        .focusable(false)
        .hexpand(false)
        .build();
    let custom_image = gtk::Image::from_pixbuf(Some(&model.custom_color_pixbuf));
    custom_toggle.set_child(Some(&custom_image));
    custom_toggle.set_action::<ColorAction>(ColorButtons::Custom);
    custom_toggle.install_tooltip("Custom color");
    custom_row.append(&custom_toggle);

    let pick_btn = gtk::Button::builder()
        .focusable(false)
        .hexpand(true)
        .icon_name("color-regular")
        .build();
    pick_btn.add_css_class("flat");
    pick_btn.install_tooltip("Pick custom color");
    let sender_clone = sender.clone();
    pick_btn.connect_clicked(move |_| {
        sender_clone.input(ToolsToolbarInput::ShowColorDialog);
    });
    custom_row.append(&pick_btn);
    outer.append(&custom_row);

    popover.set_child(Some(&outer));
    popover
}

pub struct StyleToolbar {
    visible: bool,
    annotation_size: f32,
    annotation_size_formatted: String,
    annotation_dialog_controller: Option<Controller<AnnotationSizeDialog>>,
    output_dimensions: String,
    /// Tracks the currently-active tool so tool-specific controls (e.g. the
    /// arrow-style dropdown) can show/hide reactively.
    current_tool: Tools,
}

pub struct AnnotationSizeDialog {
    annotation_size: f32,
}

#[derive(Debug, Copy, Clone)]
pub enum ToolbarEvent {
    ToolSelected(Tools),
    ColorSelected(Color),
    SizeSelected(Size),
    ArrowStyleSelected(ArrowStyle),
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
    ColorButtonSelected(ColorButtons),
    ShowColorDialog,
    ColorDialogFinished(Option<Color>),
}

#[derive(Debug, Copy, Clone)]
pub enum StyleToolbarInput {
    SetVisibility(bool),
    ToggleVisibility,
    ShowAnnotationDialog,
    AnnotationDialogFinished(Option<f32>),
    DimensionsChanged((i32, i32)),
    /// The active drawing tool changed; tool-specific controls re-evaluate
    /// their visibility.
    ToolChanged(Tools),
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
    let pixbuf = Pixbuf::new(relm4::gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, 40, 40).unwrap();
    pixbuf.fill(color.to_rgba_u32());
    pixbuf
}
fn create_icon(color: Color) -> gtk::Image {
    gtk::Image::from_pixbuf(Some(&create_icon_pixbuf(color)))
}

#[relm4::component(pub)]
impl Component for ToolsToolbar {
    type Init = ();
    type Input = ToolsToolbarInput;
    type Output = ToolbarEvent;
    type CommandOutput = ();

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
                install_tooltip: "1:1",
                connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::OriginalScale);},
            },
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "page-fit-regular",
                install_tooltip: "Fit to window",
                connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::Resize);},
            },
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "recycling-bin",
                install_tooltip: "Reset all annotations (Delete)",
                connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::Reset);},
            },
            gtk::Separator {},
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "arrow-undo-filled",
                install_tooltip: "Undo (Ctrl-Z)",
                connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::Undo);},
            },
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "arrow-redo-filled",
                install_tooltip: "Redo (Ctrl-Y)",
                connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::Redo);},
            },
            gtk::Separator {},
            #[name(pointer_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "cursor-regular",
                // tooltip set programmatically
                ActionablePlus::set_action::<ToolsAction>: Tools::Pointer,
            },
            #[name(crop_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "crop-filled",
                // tooltip set programmatically
                ActionablePlus::set_action::<ToolsAction>: Tools::Crop,
            },
            #[name(brush_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "pen-regular",
                // tooltip set programmatically
                ActionablePlus::set_action::<ToolsAction>: Tools::Brush,
            },
            #[name(line_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "minus-large",
                // tooltip set programmatically
                ActionablePlus::set_action::<ToolsAction>: Tools::Line,
            },
            #[name(arrow_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "arrow-up-right-filled",
                // tooltip set programmatically
                ActionablePlus::set_action::<ToolsAction>: Tools::Arrow,
            },
            #[name(rectangle_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "checkbox-unchecked-regular",
                // tooltip set programmatically
                ActionablePlus::set_action::<ToolsAction>: Tools::Rectangle,
            },
            #[name(ellipse_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "circle-regular",
                // tooltip set programmatically
                ActionablePlus::set_action::<ToolsAction>: Tools::Ellipse,
            },
            #[name(text_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "text-case-title-regular",
                // tooltip set programmatically
                ActionablePlus::set_action::<ToolsAction>: Tools::Text,
            },
            #[name(marker_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "number-circle-1-regular",
                // tooltip set programmatically
                ActionablePlus::set_action::<ToolsAction>: Tools::Marker,
            },
            #[name(blur_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "drop-regular",
                // tooltip set programmatically
                ActionablePlus::set_action::<ToolsAction>: Tools::Blur,
            },
            #[name(highlight_button)]
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "highlight-regular",
                // tooltip set programmatically
                ActionablePlus::set_action::<ToolsAction>: Tools::Highlight,
            },
            gtk::Separator {},
            // Unified color picker — single MenuButton showing the current
            // color; the popover (built in init) holds the palette and a
            // custom-color picker, mirroring a standard X's compact picker.
            #[name(color_button)]
            gtk::MenuButton {
                set_focusable: false,
                set_hexpand: false,
                add_css_class: "color-picker-button",
                install_tooltip: "Color",

                #[wrap(Some)]
                #[name(color_swatch)]
                set_child = &gtk::Image {
                    set_pixel_size: 18,
                    #[watch]
                    set_from_pixbuf: Some(&model.current_color_pixbuf),
                },
            },
            gtk::Separator {},
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "copy-regular",
                install_tooltip: "Copy to clipboard (Ctrl+C)",
                connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::CopyClipboard);},
            },
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "save-regular",
                install_tooltip: "Save (Ctrl+S)",
                connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::SaveFile);},

                set_visible: APP_CONFIG.read().output_filename().is_some()
            },
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: "save-multiple-regular",
                install_tooltip: "Save as (Ctrl+Shift+S)",
                connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::SaveFileAs);},
            },
        },
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
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
            ToolsToolbarInput::ColorButtonSelected(button) => {
                let color = self.map_button_to_color(button);
                self.color_action.change_state(&button.to_variant());
                self.current_color = color;
                self.current_color_pixbuf = create_icon_pixbuf(color);
                sender
                    .output_sender()
                    .emit(ToolbarEvent::ColorSelected(color));
            }
            ToolsToolbarInput::ShowColorDialog => {
                self.show_color_dialog(sender, root.toplevel_window());
            }
            ToolsToolbarInput::ColorDialogFinished(color) => {
                if let Some(color) = color {
                    self.custom_color = color;
                    self.custom_color_pixbuf = create_icon_pixbuf(color);
                    self.color_action
                        .change_state(&ColorButtons::Custom.to_variant());
                    self.current_color = color;
                    self.current_color_pixbuf = create_icon_pixbuf(color);
                    sender
                        .output_sender()
                        .emit(ToolbarEvent::ColorSelected(color));
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

        // Color action — palette-or-Custom enum, tracks current selection
        // and routes through `ColorButtonSelected` so the swatch updates.
        let sender_tmp = sender.clone();
        let color_action: RelmAction<ColorAction> = RelmAction::new_stateful_with_target_value(
            &ColorButtons::Palette(0),
            move |_, state, value| {
                *state = value;
                sender_tmp.input(ToolsToolbarInput::ColorButtonSelected(value));
            },
        );

        let custom_color = APP_CONFIG
            .read()
            .color_palette()
            .custom()
            .first()
            .copied()
            .unwrap_or(Color::red());
        let custom_color_pixbuf = create_icon_pixbuf(custom_color);
        let initial_color = APP_CONFIG
            .read()
            .color_palette()
            .palette()
            .first()
            .copied()
            .unwrap_or(Color::red());
        let initial_color_pixbuf = create_icon_pixbuf(initial_color);

        let mut model = ToolsToolbar {
            visible: !APP_CONFIG.read().default_hide_toolbars(),
            active_button: None,
            tool_buttons: HashMap::new(),
            tool_action: tool_action.clone().into(),
            current_color: initial_color,
            current_color_pixbuf: initial_color_pixbuf,
            custom_color,
            custom_color_pixbuf,
            color_action: SimpleAction::from(color_action.clone()),
        };
        let widgets = view_output!();

        // Build the popover for the unified color picker — palette swatches
        // stacked horizontally, then a separator, then the custom color
        // toggle and a "Pick custom color" button.
        let popover = build_color_popover(&model, &sender);
        widgets.color_button.set_popover(Some(&popover));

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
            .inspect(|(hotkey, tool)| if hotkey.is_ascii_digit() {
                eprintln!("Warning: hotkey `{}` for tool `{}` overrides built-in hotkey to select a color from the palette", hotkey, tool);
            })
            .map(|(k, v)| (v, k))
            .collect();

        // Update tooltips based on configured keybinds. `install_tooltip`
        // wires a `query-tooltip` handler that re-shows on every hover —
        // GTK4's default `tooltip-text` path can go stale after the
        // popover is dismissed once on Wayland.
        for (tool, button) in &model.tool_buttons {
            let display_name = tool.display_name();

            let tooltip = if let Some(key) = tool_to_key_map.get(tool) {
                format!("{} ({})", display_name, key.to_uppercase())
            } else {
                display_name.to_string()
            };
            button.install_tooltip(&tooltip);
        }

        // Set initial active button correctly
        let initial_tool = APP_CONFIG.read().initial_tool();
        if let Some(button) = model.tool_buttons.get(&initial_tool) {
            model.active_button = Some(button.clone());
        }

        let mut group = RelmActionGroup::<ToolsToolbarActionGroup>::new();
        group.add_action(tool_action);
        group.register_for_widget(&widgets.root);

        // Color action lives in its own group so it can target both the
        // palette buttons inside the popover and any external triggers
        // (e.g. number-key shortcuts).
        let mut color_group = RelmActionGroup::<StyleToolbarActionGroup>::new();
        color_group.add_action(color_action);
        color_group.register_for_widget(&widgets.root);

        // Suppress unused-root warning; we keep the parameter in case a
        // later popover needs to anchor itself to the toplevel.
        let _ = root;

        ComponentParts { model, widgets }
    }
}

#[derive(Debug, Copy, Clone)]
pub enum ColorButtons {
    Palette(u64),
    Custom,
}

impl StyleToolbar {
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

            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,
                set_label: "XS",
                install_tooltip_above: "Annotation size: X-Small",
                ActionablePlus::set_action::<SizeAction>: Size::XSmall,
            },
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,
                set_label: "S",
                install_tooltip_above: "Annotation size: Small",
                ActionablePlus::set_action::<SizeAction>: Size::Small,
            },
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,
                set_label: "M",
                install_tooltip_above: "Annotation size: Medium",
                ActionablePlus::set_action::<SizeAction>: Size::Medium,
            },
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,
                set_label: "L",
                install_tooltip_above: "Annotation size: Large",
                ActionablePlus::set_action::<SizeAction>: Size::Large,
            },
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,
                set_label: "XL",
                install_tooltip_above: "Annotation size: X-Large",
                ActionablePlus::set_action::<SizeAction>: Size::XLarge,
            },
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,
                set_label: "XXL",
                install_tooltip_above: "Annotation size: XX-Large",
                ActionablePlus::set_action::<SizeAction>: Size::XXLarge,
            },
            // Arrow style dropdown — only relevant when the Arrow tool is
            // active. Hidden otherwise so it doesn't clutter the toolbar.
            gtk::DropDown {
                set_focusable: false,
                set_hexpand: false,
                set_model: Some(&gtk::StringList::new(&["Standard", "Fancy", "Curved", "Double"])),
                install_tooltip_above: "Arrow style",
                set_margin_start: 4,
                #[watch]
                set_visible: model.current_tool == Tools::Arrow,
                connect_selected_notify[sender] => move |dropdown| {
                    let style = match dropdown.selected() {
                        0 => ArrowStyle::Standard,
                        1 => ArrowStyle::Fancy,
                        2 => ArrowStyle::Curved,
                        3 => ArrowStyle::Double,
                        _ => return,
                    };
                    sender.output_sender().emit(ToolbarEvent::ArrowStyleSelected(style));
                },
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
                install_tooltip_above: "Edit Annotation Size Factor",

                connect_clicked => StyleToolbarInput::ShowAnnotationDialog
            },
            gtk::Separator {},
            gtk::Label {
                set_focusable: false,
                set_hexpand: false,
                set_margin_start: 10,
                set_width_chars: 11,

                #[watch]
                set_text: &model.output_dimensions,
                install_tooltip_above: "Output dimensions (width x height)",
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
                install_tooltip_above: "Fill shape",
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
            StyleToolbarInput::DimensionsChanged((width, height)) => {
                self.output_dimensions = format!("{}x{}", width, height);
            }
            StyleToolbarInput::ToolChanged(tool) => {
                self.current_tool = tool;
            }
        }
    }

    fn init(
        _: Self::Init,
        _root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        // Size Action for selecting sizes
        let sender_tmp = sender.clone();
        let size_action: RelmAction<SizeAction> =
            RelmAction::new_stateful_with_target_value(&Size::Medium, move |_, state, value| {
                *state = value;
                sender_tmp
                    .output_sender()
                    .emit(ToolbarEvent::SizeSelected(*state));
            });

        // create model
        let model = StyleToolbar {
            visible: !APP_CONFIG.read().default_hide_toolbars(),
            annotation_size: APP_CONFIG.read().annotation_size_factor(),
            annotation_size_formatted: format!(
                "{0:.2}",
                APP_CONFIG.read().annotation_size_factor()
            ),
            annotation_dialog_controller: None,
            output_dimensions: String::new(),
            current_tool: APP_CONFIG.read().initial_tool(),
        };

        // create widgets
        let widgets = view_output!();

        let mut group = RelmActionGroup::<StyleToolbarActionGroup>::new();
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

                    install_tooltip: "Annotation Size Factor",
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

                    install_tooltip: "Reset Annotation Size Factor",
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
                    relm4::gtk::glib::Propagation::Stop
                }
                Key::Escape => {
                    sender.input(AnnotationSizeDialogInput::Cancel);
                    relm4::gtk::glib::Propagation::Stop
                }
                _ => relm4::gtk::glib::Propagation::Proceed,
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
