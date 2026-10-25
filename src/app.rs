use crate::START_TIME;
use crate::configuration::APP_CONFIG;
use crate::logging::{LogSeverity, log, log_with_severity};
use crate::math::Vec2D;
use crate::sketch_board::{
    SketchBoard, SketchBoardInput, SketchBoardOutput, pixbuf_from_drop_value,
};
use crate::style::{Color, Size, StyleFields};
use crate::tools::Tools;
use crate::ui::toolbars::{StyleToolbar, StyleToolbarInput, ToolsToolbar, ToolsToolbarInput};
use crate::{generate_profile_output, icons, ui};
use relm4::adw::gdk;
use relm4::adw::gdk::{DisplayManager, FullscreenMode, Rectangle, Toplevel};
use relm4::adw::gio::{Application, ApplicationFlags};
use relm4::adw::prelude::ApplicationExt;
use relm4::gtk::gdk_pixbuf::Pixbuf;
use relm4::gtk::prelude::{
    BoxExt, Cast, DisplayExt, EventControllerExt, GtkWindowExt, MonitorExt, NativeExt, ObjectExt,
    StaticType, ToplevelExt, WidgetExt,
};
use relm4::gtk::{CssProvider, Window};
use relm4::{
    Component, ComponentController, ComponentParts, ComponentSender, Controller, RelmApp, adw, gtk,
};
use satty_cli::command_line::{Fullscreen, LogLevel, Resize};
use std::fs;
use std::ops::Deref;
use std::time::Duration;
use xdg::BaseDirectories;

pub static APP_BROKER: relm4::MessageBroker<AppInput> = relm4::MessageBroker::new();

struct App {
    image_dimensions: (i32, i32),
    sketch_board: Controller<SketchBoard>,
    tools_toolbar: Controller<ToolsToolbar>,
    style_toolbar: Controller<StyleToolbar>,
    outer_box: gtk::Box,
    overlay: gtk::Overlay,
    toast_overlay: adw::ToastOverlay,
}

#[derive(Debug)]
pub enum AppInput {
    Realized,
    SetToolbarsDisplay(bool),
    ToggleToolbarsDisplay,
    ToolSwitchShortcut(Tools),
    ColorSwitchShortcut(u64),
    SetColor(Color),
    SetSize(Size),
    SetSizePresets([Size; 3]),
    SetStyleScope(Tools, StyleFields),
    FocusSizeShortcut,
    ScaleFactorChanged,
    SetFill(bool),
    SetRoundCaps(bool),
    FullscreenChanged(bool),
    DimensionsUpdate(Vec2D),
    LineDimensionsUpdate(Vec2D),
    ToolEditingChanged(bool),
    ShowToast(String),
}

#[derive(Debug)]
enum AppCommandOutput {
    ResetResizable,
}

impl App {
    fn get_monitor_size(root: &Window) -> Option<Rectangle> {
        root.surface().and_then(|surface| {
            DisplayManager::get()
                .default_display()
                .and_then(|display| display.monitor_at_surface(&surface))
                .map(|monitor| monitor.geometry())
        })
    }

    fn resize_window_initial(&self, root: &Window, sender: ComponentSender<Self>) {
        let config = APP_CONFIG.read();
        // scale and image_width and image_height below are just used to determine window
        // size within this function if we need it. This does not affect image scaling as such.
        let scale = config.input_scale().unwrap_or(1.0);
        let fullscreen = config.fullscreen();
        let resize = config.resize();
        let floating_hack = config.floating_hack();

        let image_width = (self.image_dimensions.0 as f32 * scale) as f64;
        let image_height = (self.image_dimensions.1 as f32 * scale) as f64;

        #[cfg(debug_assertions)]
        log(
            &format!(
                "Fullscreen {:?} | Resize {:?} | Floatinghack {:?}",
                fullscreen, resize, floating_hack
            ),
            LogLevel::Debug,
        );

        if fullscreen == Some(Fullscreen::All)
            && let Some(surface) = root.surface()
            && let Ok(toplevel) = surface.downcast::<Toplevel>()
        {
            toplevel.set_fullscreen_mode(FullscreenMode::AllMonitors);
        }

        let monitor_size_opt = Self::get_monitor_size(root);
        match resize {
            Some(Resize::Smart) if monitor_size_opt.is_some() => {
                let monitor_size = monitor_size_opt.unwrap();
                let reduced_monitor_width = monitor_size.width() as f64 * 0.8;
                let reduced_monitor_height = monitor_size.height() as f64 * 0.8;

                // create a window that uses 80% of the available space max
                // if necessary, scale down image
                if reduced_monitor_width > image_width && reduced_monitor_height > image_height {
                    // set window to exact size
                    root.set_default_size(image_width as i32, image_height as i32);
                } else {
                    // scale down and use windowed mode
                    let aspect_ratio = image_width / image_height;

                    // resize
                    let mut new_width = reduced_monitor_width;
                    let mut new_height = new_width / aspect_ratio;

                    // if new_height is still bigger than monitor height, then scale on monitor height
                    if new_height > reduced_monitor_height {
                        new_height = reduced_monitor_height;
                        new_width = new_height * aspect_ratio;
                    }

                    root.set_default_size(new_width as i32, new_height as i32);
                }
            }
            Some(Resize::Size { width, height }) => {
                root.set_default_size(width, height);
            }
            _ => {
                root.set_default_size(image_width as i32, image_height as i32);
            }
        }

        if floating_hack {
            root.set_resizable(false);
        }

        match fullscreen {
            Some(Fullscreen::All) | Some(Fullscreen::CurrentScreen) => {
                root.fullscreen();
            }
            _ => {}
        }

        if floating_hack {
            // this is a horrible hack to let sway recognize the window as "not resizable" and
            // place it floating mode. We then re-enable resizing to let if fit fullscreen (if requested)
            sender.command(|out, shutdown| {
                shutdown
                    .register(async move {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                        out.emit(AppCommandOutput::ResetResizable);
                    })
                    .drop_on_shutdown()
            });
        }
    }

    fn apply_style() {
        let css_provider = CssProvider::new();
        css_provider.load_from_data(include_str!("assets/default.css"));

        let css_provider_override = if let Some(overrides) = read_css_overrides() {
            let css_provider2 = CssProvider::new();
            css_provider2.load_from_data(&overrides);
            Some(css_provider2)
        } else {
            None
        };

        match DisplayManager::get().default_display() {
            Some(display) => {
                gtk::style_context_add_provider_for_display(
                    &display,
                    &css_provider,
                    gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
                );
                if let Some(css_provider2) = css_provider_override {
                    gtk::style_context_add_provider_for_display(
                        &display,
                        &css_provider2,
                        gtk::STYLE_PROVIDER_PRIORITY_USER,
                    );
                }
            }
            None => log_with_severity(
                "Cannot apply style to display",
                LogLevel::Error,
                LogSeverity::High,
            ),
        }
    }
}

#[relm4::component]
impl Component for App {
    type Init = Pixbuf;
    type Input = AppInput;
    type Output = ();
    type CommandOutput = AppCommandOutput;

    view! {
        main_window = gtk::Window {
            set_decorated: !APP_CONFIG.read().no_window_decoration(),
            set_default_size: (500, 500),
            add_css_class: "root",
            set_title: match APP_CONFIG.read().title() {
                Some(s) => Some(s.as_ref()),
                None => None
            },

            #[local_ref]
            toast_overlay_clone -> adw::ToastOverlay {
                #[local_ref]
                outer_box_clone -> gtk::Box {
                    add_css_class: "outer_box",
                    append = model.tools_toolbar.widget(),
                    #[local_ref]
                    overlay_clone -> gtk::Overlay {
                        add_css_class: "overlay",
                        model.sketch_board.widget(),
                    },
                    append = model.style_toolbar.widget(),
                },
           },

            connect_show[sender] => move |_| {
                generate_profile_output!("gui show event");
                sender.input(AppInput::Realized);
            },
        }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
        match message {
            AppInput::Realized => self.resize_window_initial(root, sender),
            AppInput::SetToolbarsDisplay(visible) => {
                self.tools_toolbar
                    .sender()
                    .emit(ToolsToolbarInput::SetVisibility(visible));
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::SetVisibility(visible));
            }
            AppInput::ToggleToolbarsDisplay => {
                self.tools_toolbar
                    .sender()
                    .emit(ToolsToolbarInput::ToggleVisibility);
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::ToggleVisibility);
            }
            AppInput::ToolSwitchShortcut(tool) => {
                self.tools_toolbar
                    .sender()
                    .emit(ToolsToolbarInput::SwitchSelectedTool(tool));
            }
            AppInput::ColorSwitchShortcut(index) => {
                let palette_len = APP_CONFIG.read().color_palette().palette().len() as u64;
                let color_button = if index < palette_len {
                    ui::toolbars::ColorButtons::Palette(index)
                } else {
                    ui::toolbars::ColorButtons::Custom
                };
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::ColorButtonSelected(color_button));
            }
            AppInput::SetColor(color) => {
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::SetColor(color));
            }
            AppInput::SetFill(fill_enabled) => {
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::SetFill(fill_enabled));
            }
            AppInput::SetRoundCaps(round_caps_enabled) => {
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::SetRoundCaps(round_caps_enabled));
            }
            AppInput::SetSize(size) => {
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::SetSize(size));
            }
            AppInput::SetSizePresets(presets) => {
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::SetSizePresets(presets));
            }
            AppInput::SetStyleScope(tool, own) => {
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::SetStyleScope(tool, own));
            }
            AppInput::FocusSizeShortcut => {
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::FocusSize);
            }
            AppInput::ScaleFactorChanged => {
                self.sketch_board
                    .sender()
                    .emit(SketchBoardInput::ScaleFactorChanged);
            }
            AppInput::FullscreenChanged(fullscreen) => {
                let tools = self.tools_toolbar.widget();
                let style = self.style_toolbar.widget();
                if fullscreen {
                    self.outer_box.remove(tools);
                    self.outer_box.remove(style);
                    self.overlay.add_overlay(tools);
                    self.overlay.add_overlay(style);
                } else {
                    self.overlay.remove_overlay(tools);
                    self.overlay.remove_overlay(style);
                    self.outer_box.prepend(tools);
                    self.outer_box.append(style);
                }
            }
            AppInput::DimensionsUpdate(dimensions) => {
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::DimensionsChanged(dimensions));
            }
            AppInput::LineDimensionsUpdate(delta) => {
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::LineDimensionsChanged(delta));
            }
            AppInput::ToolEditingChanged(editing) => {
                self.tools_toolbar
                    .sender()
                    .emit(ToolsToolbarInput::SetToolEditing(editing));
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::SetToolEditing(editing));
            }
            AppInput::ShowToast(msg) => {
                self.toast_overlay.add_toast(adw::Toast::new(&msg));
            }
        }
    }

    fn update_cmd(
        &mut self,
        command: AppCommandOutput,
        _: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match command {
            AppCommandOutput::ResetResizable => root.set_resizable(true),
        }
    }

    fn init(
        image: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        Self::apply_style();
        let image_dimensions = (image.width(), image.height());

        // SketchBoard
        let sketch_board =
            SketchBoard::builder()
                .launch(image)
                .forward(sender.input_sender(), |t| match t {
                    SketchBoardOutput::ToggleToolbarsDisplay => AppInput::ToggleToolbarsDisplay,
                    SketchBoardOutput::ToolSwitchShortcut(tool) => {
                        AppInput::ToolSwitchShortcut(tool)
                    }
                    SketchBoardOutput::ColorSwitchShortcut(index) => {
                        AppInput::ColorSwitchShortcut(index)
                    }
                    SketchBoardOutput::SetColor(color) => AppInput::SetColor(color),
                    SketchBoardOutput::SetFill(fill_enabled) => AppInput::SetFill(fill_enabled),
                    SketchBoardOutput::SetRoundCaps(round_caps_enabled) => {
                        AppInput::SetRoundCaps(round_caps_enabled)
                    }
                    SketchBoardOutput::SetSize(size) => AppInput::SetSize(size),
                    SketchBoardOutput::FocusSizeShortcut => AppInput::FocusSizeShortcut,
                    SketchBoardOutput::SetStyleScope(tool, own) => {
                        AppInput::SetStyleScope(tool, own)
                    }
                    SketchBoardOutput::SetSizePresets(presets) => AppInput::SetSizePresets(presets),
                    SketchBoardOutput::DimensionsUpdate(dimensions) => {
                        AppInput::DimensionsUpdate(dimensions)
                    }
                    SketchBoardOutput::LineDimensionsUpdate(delta) => {
                        AppInput::LineDimensionsUpdate(delta)
                    }
                    SketchBoardOutput::ToolEditingChanged(editing) => {
                        AppInput::ToolEditingChanged(editing)
                    }
                });

        // Toolbars
        let tools_toolbar = ToolsToolbar::builder()
            .launch(())
            .forward(sketch_board.sender(), SketchBoardInput::ToolbarEvent);

        let style_toolbar = StyleToolbar::builder()
            .launch(())
            .forward(sketch_board.sender(), SketchBoardInput::ToolbarEvent);

        let outer_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let outer_box_clone = outer_box.clone();
        let overlay = gtk::Overlay::new();
        let overlay_clone = overlay.clone();
        let toast_overlay = adw::ToastOverlay::new();
        let toast_overlay_clone = toast_overlay.clone();

        // Model
        let model = App {
            sketch_board,
            tools_toolbar,
            style_toolbar,
            image_dimensions,
            outer_box,
            overlay,
            toast_overlay,
        };

        // Initialize style toolbar with full image dimensions
        model
            .style_toolbar
            .sender()
            .emit(StyleToolbarInput::DimensionsChanged(Vec2D {
                x: image_dimensions.0 as f32,
                y: image_dimensions.1 as f32,
            }));

        let widgets = view_output!();

        let popover_click_controller = gtk::GestureClick::new();
        popover_click_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let window = root.clone();
        popover_click_controller.connect_pressed(move |_, _, x, y| {
            let target = window.pick(x, y, gtk::PickFlags::DEFAULT);
            ui::close_active_tool_popover_if_outside(target.as_ref());
        });
        root.add_controller(popover_click_controller);

        if APP_CONFIG.read().focus_toggles_toolbars() {
            let motion_controller = gtk::EventControllerMotion::builder().build();

            let sender_clone = sender.clone();
            motion_controller.connect_enter(move |_, _, _| {
                sender_clone.input(AppInput::SetToolbarsDisplay(true));
            });

            let sender_clone = sender.clone();
            motion_controller.connect_leave(move |_| {
                sender_clone.input(AppInput::SetToolbarsDisplay(false));
            });

            root.add_controller(motion_controller);
        }

        let sender_clone = sender.clone();
        root.connect_map(move |r| {
            let sender_clone = sender_clone.clone();
            if let Some(surface) = r.surface() {
                surface.connect_notify_local(Some("scale-factor"), move |_, _| {
                    sender_clone.input(AppInput::ScaleFactorChanged);
                });
            }
        });

        let sender_clone = sender.clone();
        root.connect_notify(Some("fullscreened"), move |window, _| {
            if window.is_fullscreen() {
                sender_clone.input(AppInput::FullscreenChanged(true));
            } else {
                sender_clone.input(AppInput::FullscreenChanged(false));
            }
        });

        // on Wayland the drag position is reported as (0,0) until the first
        // motion event and gtk picks the drop chain there, which may be a
        // toolbar; the drop target sits on the window so that it is part of
        // every pick chain
        let drop_target = gtk::DropTarget::new(gtk::glib::Type::INVALID, gdk::DragAction::COPY);
        drop_target.set_types(&[gdk::FileList::static_type(), gdk::Texture::static_type()]);
        {
            let window = root.clone();
            let sketch_area = model.sketch_board.widget().clone();
            let sender = model.sketch_board.sender().clone();
            // Some sources advertise formats that GTK can only convert after
            // accepting the drag; unsupported payloads are rejected on drop.
            drop_target.connect_accept(|_, _| true);
            // the default handlers report no action before the data is
            // available, which makes the compositor cancel the drag
            drop_target.connect_enter(|_, _, _| gdk::DragAction::COPY);
            drop_target.connect_motion(|_, _, _| gdk::DragAction::COPY);
            drop_target.connect_drop(move |_, value, x, y| {
                let Some(pixbuf) = pixbuf_from_drop_value(value) else {
                    return false;
                };
                let position = window
                    .translate_coordinates(&sketch_area, x, y)
                    .map(|(x, y)| Vec2D::new(x as f32, y as f32));
                sender.emit(SketchBoardInput::ImageSelected(pixbuf, position));
                true
            });
        }
        root.add_controller(drop_target);

        crate::generate_profile_output!("app init end");

        relm4::gtk::glib::idle_add_local_once(move || {
            crate::generate_profile_output!("main loop idle");
        });

        ComponentParts { model, widgets }
    }
}

fn read_css_overrides() -> Option<String> {
    let dirs = BaseDirectories::with_prefix(env!("CARGO_PKG_NAME"));
    let path = dirs.get_config_file("overrides.css")?;

    if !path.exists() {
        log(
            &format!(
                "CSS overrides file {} does not exist, using builtin CSS only.",
                path.display()
            ),
            LogLevel::Debug,
        );
        return None;
    }

    match fs::read_to_string(&path) {
        Ok(content) => Some(content),
        Err(e) => {
            log(
                &format!(
                    "failed to read CSS overrides from {} with error: {}",
                    path.display(),
                    e
                ),
                LogLevel::Error,
            );
            None
        }
    }
}

pub(crate) fn run_satty_app(image: Pixbuf) {
    generate_profile_output!("starting gui");
    // start GUI
    let app = relm4::main_application();
    let conf = APP_CONFIG.read();
    let app_id = match conf.app_id() {
        Some(app_id) if Application::id_is_valid(app_id) => Some(app_id.deref()),
        o => {
            if let Some(app_id) = o {
                log(
                    &format!("Invalid app id: {}, using fallback", app_id),
                    LogLevel::Error,
                );
            }
            Some("com.gabm.satty")
        }
    };
    app.set_application_id(app_id);
    // set flag to allow to run multiple instances
    app.set_flags(ApplicationFlags::NON_UNIQUE);
    // create relm app and run
    let app = RelmApp::from_app(app)
        .with_args(vec![])
        .with_broker(&APP_BROKER);
    relm4_icons::initialize_icons(
        icons::icon_names::GRESOURCE_BYTES,
        icons::icon_names::RESOURCE_PREFIX,
    );

    app.run::<App>(image)
}
