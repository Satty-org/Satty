use std::io::Read;
use std::sync::LazyLock;
use std::{fs, ptr};
use std::{io, time::Duration};

use configuration::{Configuration, APP_CONFIG};
use gdk_pixbuf::gio::ApplicationFlags;
use gdk_pixbuf::{Pixbuf, PixbufLoader};
use gtk::prelude::*;

use relm4::gtk::gdk::Rectangle;

use relm4::{
    gtk::{self, gdk::DisplayManager, CssProvider, Window},
    Component, ComponentController, ComponentParts, ComponentSender, Controller, RelmApp,
};

use anyhow::{anyhow, Context, Result};

use sketch_board::SketchBoardOutput;
use ui::toolbars::{StyleToolbar, StyleToolbarInput, ToolsToolbar, ToolsToolbarInput};
use xdg::BaseDirectories;

mod command_line;
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

use crate::sketch_board::{SketchBoard, SketchBoardInput};
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
}

#[derive(Debug)]
enum AppInput {
    Realized,
    SetToolbarsDisplay(bool),
    ToggleToolbarsDisplay,
    ToolSwitchShortcut(Tools),
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
        let scale = APP_CONFIG.read().monitor_scale();

        let monitor_size = match Self::get_monitor_size(root) {
            Some(s) => s,
            None => {
                root.set_default_size(
                    (self.image_dimensions.0 as f32 / scale) as i32,
                    (self.image_dimensions.1 as f32 / scale) as i32,
                );
                return;
            }
        };

        let reduced_monitor_width = monitor_size.width() as f64 * 0.8;
        let reduced_monitor_height = monitor_size.height() as f64 * 0.8;

        let image_width = (self.image_dimensions.0 as f32 / scale) as f64;
        let image_height = (self.image_dimensions.1 as f32 / scale) as f64;

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

        root.set_resizable(false);

        if APP_CONFIG.read().fullscreen() {
            root.fullscreen();
        }

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

    fn apply_style() {
        let css_provider = CssProvider::new();
        css_provider.load_from_data(
            "
            .root {
                min-width: 50rem;
                min-height: 10rem;
            }
            .toolbar {color: #f9f9f9 ; background: #00000099;}
            .toast {
                color: #f9f9f9;
                background: #00000099;
                border-radius: 6px;
                margin-top: 50px;
            }
            .toolbar-bottom {border-radius: 6px 6px 0px 0px;}
            .toolbar-top {border-radius: 0px 0px 6px 6px;}
            ",
        );
        if let Some(overrides) = read_css_overrides() {
            css_provider.load_from_data(&overrides);
        }
        match DisplayManager::get().default_display() {
            Some(display) => {
                gtk::style_context_add_provider_for_display(&display, &css_provider, 1)
            }
            None => println!("Cannot apply style"),
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

            connect_show[sender] => move |_| {
                generate_profile_output!("gui show event");
                sender.input(AppInput::Realized);
            },

            gtk::Overlay {
                add_overlay = model.tools_toolbar.widget(),

                add_overlay = model.style_toolbar.widget(),

                model.sketch_board.widget(),
            }
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
                });

        // Toolbars
        let tools_toolbar = ToolsToolbar::builder()
            .launch(())
            .forward(sketch_board.sender(), SketchBoardInput::ToolbarEvent);

        let style_toolbar = StyleToolbar::builder()
            .launch(())
            .forward(sketch_board.sender(), SketchBoardInput::ToolbarEvent);

        // Model
        let model = App {
            sketch_board,
            tools_toolbar,
            style_toolbar,
            image_dimensions,
        };

        let widgets = view_output!();

        if APP_CONFIG.read().focus_toggles_toolbars() {
            let motion_controller = gtk::EventControllerMotion::builder().build();
            let sender_clone = sender.clone();

            motion_controller.connect_enter(move |_, _, _| {
                sender.input(AppInput::SetToolbarsDisplay(true));
            });
            motion_controller.connect_leave(move |_| {
                sender_clone.input(AppInput::SetToolbarsDisplay(false));
            });

            root.add_controller(motion_controller);
        }

        generate_profile_output!("app init end");

        glib::idle_add_local_once(move || {
            generate_profile_output!("main loop idle");
        });

        ComponentParts { model, widgets }
    }
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
    app.set_application_id(Some("com.gabm.satty"));
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
