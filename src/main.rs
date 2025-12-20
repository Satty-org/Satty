use std::io::Read;
use std::rc::Rc;
use std::sync::LazyLock;
use std::{fs, ptr};
use std::{io, time::Duration};

use configuration::{Configuration, APP_CONFIG};
use daemon::RequestConfig;
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

mod configuration;
mod daemon;
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

/// Initialization data for the App component
pub struct AppInit {
    pub image: Pixbuf,
    pub config: Rc<RequestConfig>,
}

struct App {
    config: Rc<RequestConfig>,
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
    ColorSwitchShortcut(u64),
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
        // Handle window sizing based on monitor size
        if let Some(monitor_size) = Self::get_monitor_size(root) {
            let reduced_monitor_width = monitor_size.width() as f64 * 0.8;
            let reduced_monitor_height = monitor_size.height() as f64 * 0.8;

            let image_width = self.image_dimensions.0 as f64;
            let image_height = self.image_dimensions.1 as f64;

            // create a window that uses 80% of the available space max
            // if necessary, scale down image
            if reduced_monitor_width > image_width && reduced_monitor_height > image_height {
                // set window to exact size
                root.set_default_size(self.image_dimensions.0, self.image_dimensions.1);
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
        } else {
            root.set_default_size(self.image_dimensions.0, self.image_dimensions.1);
        }

        root.set_resizable(false);

        if self.config.fullscreen {
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
    type Init = AppInit;
    type Input = AppInput;
    type Output = ();
    type CommandOutput = AppCommandOutput;

    view! {
        main_window = gtk::Window {
            set_decorated: !model.config.no_window_decoration,
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
            AppInput::ColorSwitchShortcut(index) => {
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::ColorButtonSelected(
                        ui::toolbars::ColorButtons::Palette(index),
                    ));
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
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        Self::apply_style();

        let AppInit { image, config } = init;
        let image_dimensions = (image.width(), image.height());

        // SketchBoard - pass config for per-window settings
        let sketch_board =
            SketchBoard::builder()
                .launch((image, config.clone()))
                .forward(sender.input_sender(), |t| match t {
                    SketchBoardOutput::ToggleToolbarsDisplay => AppInput::ToggleToolbarsDisplay,
                    SketchBoardOutput::ToolSwitchShortcut(tool) => {
                        AppInput::ToolSwitchShortcut(tool)
                    }
                    SketchBoardOutput::ColorSwitchShortcut(index) => {
                        AppInput::ColorSwitchShortcut(index)
                    }
                });

        // Toolbars - pass config for per-window settings
        let tools_toolbar = ToolsToolbar::builder()
            .launch(config.clone())
            .forward(sketch_board.sender(), SketchBoardInput::ToolbarEvent);

        let style_toolbar = StyleToolbar::builder()
            .launch(config.clone())
            .forward(sketch_board.sender(), SketchBoardInput::ToolbarEvent);

        // Model
        let model = App {
            config: config.clone(),
            sketch_board,
            tools_toolbar,
            style_toolbar,
            image_dimensions,
        };

        let widgets = view_output!();

        if config.focus_toggles_toolbars {
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
    let global_config = APP_CONFIG.read();

    generate_profile_output!("loading image");
    // load input image
    let image = if global_config.input_filename() == "-" {
        let mut buf = Vec::<u8>::new();
        io::stdin().lock().read_to_end(&mut buf)?;
        let pb_loader = PixbufLoader::new();
        pb_loader.write(&buf)?;
        pb_loader.close()?;
        pb_loader
            .pixbuf()
            .ok_or(anyhow!("Conversion to Pixbuf failed"))?
    } else {
        Pixbuf::from_file(global_config.input_filename()).context("couldn't load image")?
    };
    drop(global_config); // Release lock before creating RequestConfig

    // Create per-window configuration from global config
    let config = Rc::new(RequestConfig::from_global());

    generate_profile_output!("image loaded, starting gui");
    // start GUI
    let app = relm4::main_application();
    app.set_application_id(Some("com.gabm.satty"));
    // set flag to allow to run multiple instances
    app.set_flags(ApplicationFlags::NON_UNIQUE);
    // create relm app and run (with empty args to avoid GTK parsing our flags)
    let app = RelmApp::from_app(app).with_args(vec![]);
    relm4_icons::initialize_icons(
        icons::icon_names::GRESOURCE_BYTES,
        icons::icon_names::RESOURCE_PREFIX,
    );
    app.run::<App>(AppInit { image, config });
    Ok(())
}

/// Run in client mode: send request to daemon, fallback to normal if daemon not running
fn run_client() -> Result<()> {
    use base64::Engine;
    use daemon::{get_socket_path, DaemonClient, DaemonRequest, ResponseStatus};

    let socket_path = get_socket_path();
    let client = DaemonClient::new(&socket_path);

    // Check if daemon is running
    if !client.is_daemon_running() {
        eprintln!("Daemon not running, falling back to normal startup");
        return run_satty();
    }

    let config = APP_CONFIG.read();

    // Build request from current configuration
    let mut request = DaemonRequest::new(config.input_filename());
    request.output_filename = config.output_filename().cloned();
    request.copy_command = config.copy_command().cloned();
    request.fullscreen = Some(config.fullscreen());
    request.early_exit = Some(config.early_exit());
    request.corner_roundness = Some(config.corner_roundness());
    request.annotation_size_factor = Some(config.annotation_size_factor());
    request.default_hide_toolbars = Some(config.default_hide_toolbars());
    request.no_window_decoration = Some(config.no_window_decoration());

    // Handle stdin mode: read and base64 encode
    if config.input_filename() == "-" {
        let mut buf = Vec::new();
        io::stdin().lock().read_to_end(&mut buf)?;
        request.stdin_data = Some(base64::engine::general_purpose::STANDARD.encode(&buf));
    }

    // Send request to daemon
    match client.send_request(&request) {
        Ok(response) => {
            match response.status {
                ResponseStatus::Ok => {
                    if let Some(window_id) = response.window_id {
                        generate_profile_output!(format!("window {} opened via daemon", window_id));
                    }
                    Ok(())
                }
                ResponseStatus::Error => {
                    let msg = response.message.unwrap_or_else(|| "Unknown error".into());
                    eprintln!("Daemon error: {}", msg);
                    Err(anyhow!("Daemon error: {}", msg))
                }
            }
        }
        Err(e) => {
            eprintln!("Failed to communicate with daemon: {}", e);
            eprintln!("Falling back to normal startup");
            run_satty()
        }
    }
}

/// Run in daemon mode: initialize GTK, listen for requests, create windows on demand
fn run_daemon() -> Result<()> {
    use daemon::{get_socket_path, is_daemon_running, remove_stale_socket, DaemonServer};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    // Check if daemon is already running
    if is_daemon_running() {
        eprintln!("Daemon is already running");
        std::process::exit(1);
    }

    // Remove stale socket if any
    remove_stale_socket()?;

    // Load OpenGL
    load_gl()?;
    generate_profile_output!("daemon: loaded gl");

    // Initialize icons (before any GTK windows)
    relm4_icons::initialize_icons(
        icons::icon_names::GRESOURCE_BYTES,
        icons::icon_names::RESOURCE_PREFIX,
    );

    // Initialize GTK application
    let app = gtk::Application::new(Some("com.gabm.satty.daemon"), ApplicationFlags::NON_UNIQUE);

    // Channel for passing requests from socket thread to main thread
    let (tx, rx) = std::sync::mpsc::channel::<(daemon::DaemonRequest, std::sync::mpsc::Sender<daemon::DaemonResponse>)>();
    let rx = Arc::new(std::sync::Mutex::new(rx));

    // Window counter
    let window_counter = Arc::new(AtomicU64::new(0));

    // On activate, set up the socket listener and request handler
    let rx_clone = rx.clone();
    let window_counter_clone = window_counter.clone();
    app.connect_activate(move |app| {
        // Hold the application so it doesn't quit when no windows are open
        let guard = app.hold();
        // Store the guard - we need to keep it alive
        // Use a static or leak it since we want the daemon to run forever
        std::mem::forget(guard);

        // Pre-warm GTK by creating, briefly presenting, and closing a hidden window
        // This initializes internal GTK structures that would otherwise slow down the first real window
        let dummy_image = Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, 1, 1)
            .expect("Failed to create prewarm image");
        let dummy_config = Rc::new(RequestConfig::default());
        let mut prewarm_app = App::builder().launch(AppInit {
            image: dummy_image,
            config: dummy_config,
        });
        let prewarm_window = prewarm_app.widget();
        prewarm_window.set_application(Some(app));
        // Hide the window initially, present briefly to trigger GTK init, then close
        prewarm_window.set_visible(false);
        prewarm_window.present();
        // Process a few GTK events to complete initialization
        while gtk::glib::MainContext::default().iteration(false) {}
        prewarm_window.close();
        prewarm_app.detach_runtime();

        eprintln!("Daemon activated, setting up request handler...");

        // Start socket server in separate thread
        let socket_path = get_socket_path();
        let tx = tx.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
            rt.block_on(async move {
                let server = match DaemonServer::new(&socket_path).await {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("Failed to create daemon server: {}", e);
                        return;
                    }
                };
                eprintln!("Daemon listening on {:?}", server.socket_path());

                loop {
                    match server.accept().await {
                        Ok((request, mut connection)) => {
                            // Create sync channel for response
                            let (resp_tx, resp_rx) = std::sync::mpsc::channel();

                            if tx.send((request, resp_tx)).is_err() {
                                eprintln!("Main thread exited, stopping socket server");
                                break; // Main thread exited
                            }

                            // Wait for response and send back to client
                            tokio::spawn(async move {
                                if let Ok(response) = resp_rx.recv() {
                                    let _ = connection.send_response(&response).await;
                                }
                            });
                        }
                        Err(e) => {
                            // Ignore "early eof" errors from connection checks
                            let err_str = e.to_string();
                            if !err_str.contains("early eof") {
                                eprintln!("Error accepting connection: {}", e);
                            }
                        }
                    }
                }
            });
        });

        // Poll for incoming requests using GLib timeout (more reliable than idle for long-running)
        let rx = rx_clone.clone();
        let window_counter = window_counter_clone.clone();
        let app_weak = app.downgrade();

        glib::timeout_add_local(std::time::Duration::from_millis(10), move || {
            // Check if app still exists
            let Some(app) = app_weak.upgrade() else {
                return glib::ControlFlow::Break;
            };

            // Try to receive a request (non-blocking)
            let maybe_request = {
                let rx = rx.lock().unwrap();
                rx.try_recv().ok()
            };

            if let Some((request, response_tx)) = maybe_request {
                // Validate request
                if let Err(e) = request.validate() {
                    eprintln!("Request validation failed: {}", e);
                    let _ = response_tx.send(daemon::DaemonResponse::error(e.to_string()));
                    return glib::ControlFlow::Continue;
                }

                // Load image
                let image = match load_image_from_request(&request) {
                    Ok(img) => img,
                    Err(e) => {
                        eprintln!("Failed to load image: {}", e);
                        let _ = response_tx.send(daemon::DaemonResponse::error(e.to_string()));
                        return glib::ControlFlow::Continue;
                    }
                };

                // Create per-window configuration from request
                // Each window gets its own config, eliminating race conditions
                let config = Rc::new(RequestConfig::from_request(&request));

                // Create window
                let window_id = window_counter.fetch_add(1, Ordering::SeqCst) + 1;

                // Send response BEFORE window.present() so client can exit faster
                let _ = response_tx.send(daemon::DaemonResponse::ok(window_id));

                // Create a new window with the App component
                spawn_annotation_window(&app, image, config);
            }

            glib::ControlFlow::Continue
        });
    });

    // Connect shutdown handler
    app.connect_shutdown(|_| {
        eprintln!("Daemon shutting down, cleaning up socket...");
        let _ = daemon::remove_stale_socket();
        eprintln!("Socket cleaned up");
    });

    // Set up signal handling for graceful shutdown
    let app_for_signal = app.clone();
    glib::spawn_future_local(async move {
        use tokio::signal::unix::{signal, SignalKind};

        let mut sigterm = signal(SignalKind::terminate()).expect("Failed to create SIGTERM handler");
        let mut sigint = signal(SignalKind::interrupt()).expect("Failed to create SIGINT handler");

        tokio::select! {
            _ = sigterm.recv() => {
                eprintln!("Received SIGTERM, initiating graceful shutdown...");
            }
            _ = sigint.recv() => {
                eprintln!("Received SIGINT, initiating graceful shutdown...");
            }
        }

        app_for_signal.quit();
    });

    generate_profile_output!("daemon: starting GTK main loop");

    // Run the GTK application (this blocks until quit)
    // Pass empty args to avoid GTK parsing our arguments
    app.run_with_args::<&str>(&[]);

    Ok(())
}

/// Load image from a daemon request
fn load_image_from_request(request: &daemon::DaemonRequest) -> Result<Pixbuf> {
    use base64::Engine;

    if request.filename == "-" {
        // Load from base64 stdin data
        let data = request.stdin_data.as_ref()
            .ok_or_else(|| anyhow!("No stdin data provided"))?;
        let decoded = base64::engine::general_purpose::STANDARD.decode(data)
            .context("Failed to decode base64 image data")?;

        let pb_loader = PixbufLoader::new();
        pb_loader.write(&decoded)?;
        pb_loader.close()?;
        pb_loader.pixbuf().ok_or_else(|| anyhow!("Conversion to Pixbuf failed"))
    } else {
        // Validate and load from file
        let validated_path = daemon::validate_image_path(&request.filename)
            .map_err(|e| anyhow!("Invalid image path: {}", e))?;

        Pixbuf::from_file(&validated_path).context("Couldn't load image")
    }
}

/// Spawn a new annotation window with the given image and per-window configuration
fn spawn_annotation_window(gtk_app: &gtk::Application, image: Pixbuf, config: Rc<RequestConfig>) {
    // Launch the App component with per-window configuration
    let init = AppInit { image, config };
    let mut app_component = App::builder().launch(init);

    // Get the window widget and associate it with our GTK Application
    let window = app_component.widget();
    window.set_application(Some(gtk_app));
    window.present();

    // Detach the controller so it doesn't get dropped and close the window
    app_component.detach_runtime();
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

    let config = APP_CONFIG.read();

    // Dispatch based on mode
    let result = if config.daemon_mode() {
        drop(config); // Release the lock before running
        run_daemon()
    } else if config.show_mode() {
        drop(config);
        run_client()
    } else {
        drop(config);
        run_satty()
    };

    match result {
        Err(e) => {
            eprintln!("Error: {e}");
            Err(e)
        }
        Ok(v) => Ok(v),
    }
}
