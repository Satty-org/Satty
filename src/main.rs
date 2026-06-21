use configuration::{APP_CONFIG, Configuration};
use std::io::Read;
use std::ops::Deref;
use std::process::exit;
use std::sync::LazyLock;
use std::{fs, ptr};
use std::{io, time::Duration};

use relm4::gtk::gdk_pixbuf::{Pixbuf, PixbufLoader};
use relm4::gtk::gio::{Application, ApplicationFlags};
use relm4::gtk::prelude::*;

use relm4::gtk::gdk::Rectangle;

use relm4::{
    Component, ComponentController, ComponentParts, ComponentSender, Controller, RelmApp,
    gtk::{self, CssProvider, Window, gdk::DisplayManager, gdk::FullscreenMode, gdk::Toplevel},
};

use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use anyhow::{Context, Result, anyhow};
use satty_cli::command_line::{Fullscreen, Resize};

use sketch_board::SketchBoardOutput;
use ui::toolbars::{StyleToolbar, StyleToolbarInput, ToolsToolbar, ToolsToolbarInput};
use xdg::BaseDirectories;

mod configuration;
mod femtovg_area;
mod icons;
mod ime;
mod math;
mod notification;
mod sketch_board;
mod style;
mod tools;
mod ui;

use crate::math::Vec2D;
use crate::sketch_board::{MonitorViewSpec, SketchBoard, SketchBoardInput};
use crate::tools::Tools;

pub static START_TIME: LazyLock<chrono::DateTime<chrono::Local>> =
    LazyLock::new(chrono::Local::now);

macro_rules! generate_profile_output {
    ($e: expr) => {
        if (APP_CONFIG.read().profile_startup()) {
            eprintln!(
                "{:5} ms time elapsed: {}",
                (chrono::Local::now() - *START_TIME).num_milliseconds(),
                $e
            );
        }
    };
}

struct App {
    image_dimensions: (i32, i32),
    sketch_board: Controller<SketchBoard>,
    tools_toolbar: Controller<ToolsToolbar>,
    style_toolbar: Controller<StyleToolbar>,
    outer_box: gtk::Box,
    overlay: gtk::Overlay,
    // Whether the toolbars currently live in the overlay (fullscreen) rather than the outer box.
    // Tracked so FullscreenChanged stays idempotent: it can fire more than once for the same state
    // (e.g. our explicit call for layer-shell fullscreen="all" plus the window's "fullscreened"
    // notify), and re-running the move would hit a gtk_box_remove assertion on an already-moved
    // widget.
    toolbars_overlaid: bool,
}

#[derive(Debug)]
enum AppInput {
    Realized,
    SetToolbarsDisplay(bool),
    ToggleToolbarsDisplay,
    ToolSwitchShortcut(Tools),
    ColorSwitchShortcut(u64),
    ScaleFactorChanged,
    FullscreenChanged(bool),
    DimensionsUpdate(Option<(i32, i32)>),
    ToolEditingChanged(bool),
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
        let scale = config.input_scale().unwrap_or(1.0);
        let fullscreen = config.fullscreen();
        let resize = config.resize();
        let floating_hack = config.floating_hack();

        let image_width = (self.image_dimensions.0 as f32 / scale) as f64;
        let image_height = (self.image_dimensions.1 as f32 / scale) as f64;

        eprintln!(
            "Fullscreen {:?} | Resize {:?} | Floatinghack {:?}",
            fullscreen, resize, floating_hack
        );

        // On Wayland fullscreen="all" is realized via per-monitor layer-shell surfaces (set up in
        // init), which size themselves to each output. Skip the native sizing/fullscreen path.
        if layershell_all_active() {
            return;
        }

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
            None => eprintln!("Cannot apply style"),
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
            AppInput::ScaleFactorChanged => {
                self.sketch_board
                    .sender()
                    .emit(SketchBoardInput::ScaleFactorChanged);
            }
            AppInput::FullscreenChanged(fullscreen) => {
                // Idempotent: skip when the toolbars are already in the requested place. Without
                // this guard a repeated FullscreenChanged(true) re-runs the move and trips a
                // gtk_box_remove assertion on the (already moved) toolbar widgets.
                if fullscreen != self.toolbars_overlaid {
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
                    self.toolbars_overlaid = fullscreen;
                }
            }
            AppInput::DimensionsUpdate(dimensions) => {
                let d = dimensions.unwrap_or(self.image_dimensions);
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::DimensionsChanged(d));
            }
            AppInput::ToolEditingChanged(editing) => {
                self.tools_toolbar
                    .sender()
                    .emit(ToolsToolbarInput::SetToolEditing(editing));
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
                    SketchBoardOutput::DimensionsUpdate(dimensions) => {
                        AppInput::DimensionsUpdate(dimensions)
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

        // Model
        let model = App {
            sketch_board,
            tools_toolbar,
            style_toolbar,
            image_dimensions,
            outer_box,
            overlay,
            toolbars_overlaid: false,
        };

        // Initialize style toolbar with full image dimensions
        model
            .style_toolbar
            .sender()
            .emit(StyleToolbarInput::DimensionsChanged(image_dimensions));

        let widgets = view_output!();

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

        // fullscreen="all" on Wayland: span all monitors via per-monitor layer-shell surfaces.
        if layershell_all_active() {
            setup_layershell_all(&root, model.sketch_board.sender(), image_dimensions);
            // toolbars overlay the canvas (layer-shell surfaces don't emit "fullscreened")
            sender.input(AppInput::FullscreenChanged(true));
        }

        generate_profile_output!("app init end");

        relm4::gtk::glib::idle_add_local_once(move || {
            generate_profile_output!("main loop idle");
        });

        ComponentParts { model, widgets }
    }
}

fn is_wayland() -> bool {
    DisplayManager::get()
        .default_display()
        .map(|d| d.type_().name() == "GdkWaylandDisplay")
        .unwrap_or(false)
}

/// True when fullscreen="all" should be realized by spanning all monitors with per-monitor
/// layer-shell surfaces (Wayland only; X11 keeps GDK's native all-monitor fullscreen).
pub(crate) fn layershell_all_active() -> bool {
    APP_CONFIG.read().fullscreen() == Some(Fullscreen::All)
        && is_wayland()
        && gtk4_layer_shell::is_supported()
}

/// Span fullscreen="all" across every monitor on Wayland. The App root window becomes the primary
/// layer-shell surface; SketchBoard creates one more layer-shell surface per remaining monitor.
/// Each surface shows its own slice of the screenshot at native scale.
fn setup_layershell_all(
    root: &Window,
    sketch_board_sender: &relm4::Sender<SketchBoardInput>,
    image_dimensions: (i32, i32),
) {
    let Some(display) = DisplayManager::get().default_display() else {
        eprintln!("fullscreen=all: no default display");
        return;
    };
    let monitor_model = display.monitors();

    // (monitor, connector, geometry, scale_factor) for each connected monitor
    let mut monitors: Vec<(gtk::gdk::Monitor, String, Rectangle, i32)> = Vec::new();
    for i in 0..monitor_model.n_items() {
        if let Some(mon) = monitor_model
            .item(i)
            .and_then(|obj| obj.downcast::<gtk::gdk::Monitor>().ok())
        {
            let Some(connector) = mon.connector().map(|c| c.to_string()) else {
                continue;
            };
            let geometry = mon.geometry();
            let scale = mon.scale_factor();
            monitors.push((mon, connector, geometry, scale));
        }
    }
    if monitors.is_empty() {
        eprintln!("fullscreen=all: no monitors found");
        return;
    }

    // bounding box of the whole layout, in logical coordinates
    let min_x = monitors.iter().map(|m| m.2.x()).min().unwrap_or(0);
    let min_y = monitors.iter().map(|m| m.2.y()).min().unwrap_or(0);
    let max_right = monitors
        .iter()
        .map(|m| m.2.x() + m.2.width())
        .max()
        .unwrap_or(0);
    let max_bottom = monitors
        .iter()
        .map(|m| m.2.y() + m.2.height())
        .max()
        .unwrap_or(0);
    let layout_w = (max_right - min_x).max(1) as f32;
    let layout_h = (max_bottom - min_y).max(1) as f32;

    // how the screenshot maps onto the layout (≈ 1.0 for an unscaled grim capture of all outputs)
    let sx = image_dimensions.0 as f32 / layout_w;
    let sy = image_dimensions.1 as f32 / layout_h;

    // the toolbars live on the primary monitor: prefer the one containing the layout origin (0,0)
    let primary_idx = monitors
        .iter()
        .position(|m| {
            let g = m.2;
            g.x() <= 0 && 0 < g.x() + g.width() && g.y() <= 0 && 0 < g.y() + g.height()
        })
        .unwrap_or(0);

    // configure the App root window as the primary layer-shell surface
    let primary_monitor = &monitors[primary_idx].0;
    root.init_layer_shell();
    root.set_namespace(Some("satty"));
    root.set_layer(Layer::Overlay);
    root.set_monitor(Some(primary_monitor));
    for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
        root.set_anchor(edge, true);
    }
    root.set_exclusive_zone(-1);
    root.set_keyboard_mode(KeyboardMode::OnDemand);

    let specs: Vec<MonitorViewSpec> = monitors
        .iter()
        .enumerate()
        .map(|(i, (_, connector, geometry, scale))| MonitorViewSpec {
            connector: connector.clone(),
            image_origin: Vec2D::new(
                (geometry.x() - min_x) as f32 * sx,
                (geometry.y() - min_y) as f32 * sy,
            ),
            image_per_device_px: sx / (*scale).max(1) as f32,
            is_primary: i == primary_idx,
        })
        .collect();

    eprintln!(
        "fullscreen=all: spanning {} monitors (layout {}x{}, image {}x{})",
        specs.len(),
        layout_w as i32,
        layout_h as i32,
        image_dimensions.0,
        image_dimensions.1
    );

    sketch_board_sender.emit(SketchBoardInput::SetupAllMonitors(specs));
}

fn read_css_overrides() -> Option<String> {
    let dirs = BaseDirectories::with_prefix(env!("CARGO_PKG_NAME"));
    let path = dirs.get_config_file("overrides.css")?;

    if !path.exists() {
        eprintln!(
            "CSS overrides file {} does not exist, using builtin CSS only.",
            &path.display()
        );
        return None;
    }

    match fs::read_to_string(&path) {
        Ok(content) => Some(content),
        Err(e) => {
            eprintln!(
                "failed to read CSS overrides from {} with error: {}",
                &path.display(),
                e
            );
            None
        }
    }
}

fn load_gl() -> Result<()> {
    // Load GL pointers from epoxy (GL context management library used by GTK).
    #[cfg(target_os = "macos")]
    let library = unsafe { libloading::os::unix::Library::new("libepoxy.0.dylib") }?;
    #[cfg(all(unix, not(target_os = "macos")))]
    let library = unsafe { libloading::os::unix::Library::new("libepoxy.so.0") }?;
    #[cfg(windows)]
    let library = libloading::os::windows::Library::open_already_loaded("libepoxy-0.dll")
        .or_else(|_| libloading::os::windows::Library::open_already_loaded("epoxy-0.dll"))?;

    epoxy::load_with(|name| {
        unsafe { library.get::<_>(name.as_bytes()) }
            .map(|symbol| *symbol)
            .unwrap_or(ptr::null())
    });

    Ok(())
}

fn run_satty() -> Result<()> {
    // load OpenGL
    load_gl()?;
    generate_profile_output!("loaded gl");

    // load app config
    let config = APP_CONFIG.read();

    generate_profile_output!("loading image");
    // load input image
    let image = if config.input_filename() == "-" {
        let mut buf = Vec::<u8>::new();
        io::stdin().lock().read_to_end(&mut buf)?;
        let pb_loader = PixbufLoader::new();
        pb_loader.write(&buf)?;
        pb_loader.close()?;
        pb_loader
            .pixbuf()
            .ok_or(anyhow!("Conversion to Pixbuf failed"))?
    } else {
        Pixbuf::from_file(config.input_filename()).context("couldn't load image")?
    };

    generate_profile_output!("image loaded, starting gui");
    // start GUI
    let app = relm4::main_application();
    let app_id = match config.app_id() {
        Some(app_id) if Application::id_is_valid(app_id) => Some(app_id.deref()),
        o => {
            if let Some(app_id) = o {
                eprintln!("Invalid app id: {}, using fallback", app_id);
            }
            Some("com.gabm.satty")
        }
    };
    app.set_application_id(app_id);
    // set flag to allow to run multiple instances
    app.set_flags(ApplicationFlags::NON_UNIQUE);
    // create relm app and run
    let app = RelmApp::from_app(app).with_args(vec![]);
    relm4_icons::initialize_icons(
        icons::icon_names::GRESOURCE_BYTES,
        icons::icon_names::RESOURCE_PREFIX,
    );
    app.run::<App>(image);
    Ok(())
}

fn main() -> Result<()> {
    let _ = *START_TIME;
    // populate the APP_CONFIG from commandline and
    // config file. this might exit, if an error occurred.
    Configuration::load();
    if APP_CONFIG.read().man() {
        print!(include_str!(concat!(env!("OUT_DIR"), "/satty.1")));
        exit(0);
    }
    if APP_CONFIG.read().license() {
        print!(include_str!("../LICENSE"));
        exit(0);
    }
    if APP_CONFIG.read().profile_startup() {
        eprintln!(
            "startup timestamp was {}",
            START_TIME.format("%s.%f %Y-%m-%d %H:%M:%S")
        );
    }
    generate_profile_output!("configuration loaded");

    // run the application
    match run_satty() {
        Err(e) => {
            eprintln!("Error: {e}");
            Err(e)
        }
        Ok(v) => Ok(v),
    }
}
