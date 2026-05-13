use configuration::{APP_CONFIG, Configuration};
use std::io::Read;
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

use anyhow::{Context, Result, anyhow};
use satty_cli::command_line::{Fullscreen, Resize};

use sketch_board::SketchBoardOutput;
use ui::toolbars::{
    RobustTooltipExt, StyleToolbar, StyleToolbarInput, ToolbarEvent, ToolsToolbar,
    ToolsToolbarInput,
};
use ui::welcome::{WelcomeDialog, WelcomeDialogInit, WelcomeDialogOutput};
use ui::zoom_indicator::{ZoomIndicator, ZoomIndicatorInput, ZoomIndicatorOutput};
use xdg::BaseDirectories;

mod configuration;
mod display;
mod femtovg_area;
mod hyprland;
mod icons;
mod ime;
mod math;
mod notification;
mod sketch_board;
mod state;
mod style;
mod text_bands;
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
    zoom_indicator: Controller<ZoomIndicator>,
    outer_box: gtk::Box,
    overlay: gtk::Overlay,
    bottom_row: gtk::CenterBox,
    /// Set when state has no persisted `annotation_size_factor` and the
    /// welcome dialog still needs to run. Consumed by the `Realized`
    /// handler so the dialog appears once the main window is on-screen
    /// (and clears so it doesn't relaunch on subsequent realizes).
    welcome_pending: bool,
    /// Detected display scale to pre-fill the welcome dialog with.
    /// Falls back to 1.0 if no signal is available.
    detected_scale: f32,
    /// Whether `detected_scale` came from an actual probe (currently
    /// just Hyprland via `hyprctl`) or just the fallback. Controls the
    /// wording of the dialog's hint text.
    scale_detected: bool,
    /// Holds the welcome dialog controller alive while it's showing.
    /// Cleared on Saved so the window's widgets can be dropped.
    welcome_controller: Option<Controller<WelcomeDialog>>,
    /// "Snap to edges" checkbox for the crop tool, mounted in the
    /// bottom-left cluster alongside the zoom indicator. Visible only
    /// when the crop tool is active — uses the standard placement.
    snap_to_edges_check: gtk::CheckButton,
    /// Hint label that appears next to the snap checkbox while crop
    /// is active. Keeps the affordance discoverable like a polished desktop tool.
    snap_to_edges_hint: gtk::Label,
    /// Horizontal cluster used as `bottom_row.start_widget`: zoom
    /// indicator + snap checkbox + "Hold Ctrl…" hint. Stored so the
    /// relm4 view! macro can reference it without re-creating widgets
    /// on every update.
    start_cluster: gtk::Box,
    /// "Revert to Original" button mounted in `bottom_row.end_widget`.
    /// Outside the StyleToolbar so its visibility toggling doesn't
    /// shift the centered toolbar's width.
    revert_button: gtk::Button,
    /// Output dimensions label (`WIDTHxHEIGHT`) shown in the
    /// bottom-right corner — opposite the zoom indicator. Lives
    /// outside the StyleToolbar so it stays visible during Crop
    /// mode (the StyleToolbar hides while cropping).
    output_dimensions_label: gtk::Label,
    /// Container Box for `bottom_row.end_widget`. Wraps the output
    /// dimensions label + the Revert button so both can occupy the
    /// CenterBox's single end slot.
    end_cluster: gtk::Box,
    /// Active drawing tool. App tracks it locally so revert-button
    /// visibility can combine "we have a crop" with "we're in crop
    /// mode" — Revert only appears on the dedicated crop bottom bar.
    current_tool: Tools,
    /// Whether a crop region currently exists (committed or in-edit).
    /// Combined with `current_tool == Crop` to gate the Revert button.
    has_crop: bool,
    /// Horizontal scrollbar overlaid on the bottom of the canvas.
    /// Hidden until the image is zoomed in past the canvas's width;
    /// otherwise there's nothing to scroll.
    scrollbar_h: gtk::Scrollbar,
    /// Vertical scrollbar overlaid on the right side of the canvas.
    scrollbar_v: gtk::Scrollbar,
    /// Set true while `sync_scrollbars` is programmatically updating
    /// the scrollbar adjustments. Read by the `value_changed`
    /// callbacks so they ignore those changes — otherwise the
    /// "renderer pans → sync scrollbar → callback → ask renderer to
    /// pan → renderer pans …" feedback would loop indefinitely.
    /// `Rc<Cell<bool>>` because the callbacks are independent
    /// closures captured separately from the App model.
    applying_scrollbar: std::rc::Rc<std::cell::Cell<bool>>,
    /// Center-of-canvas toast surfaced when the user double-taps a
    /// tool shortcut and the cycle promotes a new variant (e.g.
    /// "Arrow: Curved"). The label sits inside a Revealer so it
    /// fades in/out; the SourceId tracks the live hide-timer so
    /// rapid-fire cycles restart the countdown instead of stacking
    /// callbacks.
    cycle_toast_label: gtk::Label,
    cycle_toast_revealer: gtk::Revealer,
    cycle_toast_timer: std::rc::Rc<std::cell::RefCell<Option<gtk::glib::SourceId>>>,
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
    /// Sketch board changed the active tool's size — forward to the
    /// style toolbar so its slider mirrors the new value.
    ToolSizeChanged(crate::style::Size),
    /// The canvas's intrinsic content size changed (crop commit /
    /// re-enter crop edit / revert). Re-fit the window around the
    /// new content with the same padding-and-90 %-cap logic the
    /// startup path uses.
    ContentSizeChanged { width: f32, height: f32 },
    /// Background-image dimensions changed (startup, rotate, resize).
    /// Forwarded to ToolsToolbar so the "Image size: W × H px"
    /// MenuButton label and resize-popover entries stay in sync.
    ImageDimensionsChanged { width: i32, height: i32 },
    /// Fill-Shape toggled from outside the StyleToolbar (`F`
    /// keyboard shortcut). Forwarded so the toolbar's button
    /// state updates in lockstep with sketch_board's `style.fill`.
    FillShapesChanged(bool),
    /// Crop rect dimensions during a drag / typed set — pushed
    /// only to the top toolbar's W/H entries (the bottom-right
    /// output-dims readout doesn't watch this; it tracks the
    /// committed output, not the in-edit rect).
    CropEditDimensions { width: i32, height: i32 },
    /// Open the Preferences dialog (gear button or Ctrl+,).
    OpenPreferences,
    /// First-run welcome dialog Save handler. Persists the chosen
    /// `annotation_size_factor`, pushes it into `APP_CONFIG`, and
    /// notifies the style toolbar so its display matches.
    WelcomeDialogSaved(f32),
    /// "Snap to edges" checkbox toggled. Forwards as a toolbar event
    /// so sketch_board can route it into `CropTool::set_snap_to_edges`
    /// and persist via `state::save_snap_to_edges`.
    SnapToEdgesToggled(bool),
    /// "Revert to Original" clicked. Lives outside the StyleToolbar
    /// now (in `bottom_row.end_widget`) so its appearance/disappearance
    /// doesn't shift the centered StyleToolbar's width.
    RevertCropClicked,
    /// Renderer reports a new effective scale; forwarded to the
    /// zoom-indicator widget so its label stays in sync with scroll-zoom.
    ZoomChanged(f32),
    /// Crop presence (edit OR committed) — drives the bottom toolbar's
    /// "Revert to Original" button visibility.
    CropPresenceChanged(bool),
    /// Renderer pan state changed (wheel scroll or programmatic
    /// reset). Updates the overlaid scrollbars' visibility + values.
    PanChanged(sketch_board::PanInfo),
    /// User dragged one of the scrollbars. The boolean is true for
    /// the horizontal axis, false for vertical. The f32 is the new
    /// adjustment value (canvas pixels of scroll offset).
    ScrollbarChanged(bool, f32),
    /// Selected drawable's style — forwarded from sketch_board so
    /// the StyleToolbar (size slider, etc.) can sync to whatever
    /// shape the user just selected. `None` means cleared / multi.
    SelectionStyleChanged(Option<style::Style>),
    /// Per-property "do all the multi-selected drawables share this
    /// value?" report from sketch_board, forwarded to the toolbar so
    /// its sliders can either reflect a shared value (Some) or
    /// disable themselves (None) when the selection is mixed. The
    /// smoothness slider additionally hides entirely when the
    /// selection contains a non-brush drawable
    /// (`SmoothLevelMulti::NotApplicable`).
    SelectionMultiAgreement {
        size: Option<style::Size>,
        smooth: sketch_board::SmoothLevelMulti,
        annotation: Option<f32>,
    },
    /// Tool-specific style was cycled in sketch_board via the
    /// double-tap keyboard shortcut. The StyleToolbar's menu /
    /// dropdown for that tool follows so the on-screen affordance
    /// agrees with the variant now in use.
    ArrowStyleCycled(crate::tools::ArrowStyle),
    BlurStyleCycled(crate::tools::BlurStyle),
    TextBackgroundCycled(crate::tools::TextBackground),
    HighlighterStyleCycled(crate::tools::HighlighterStyle),
    /// Show a transient toast announcing the just-cycled variant.
    /// Drives `cycle_toast_revealer` and (re)schedules the hide
    /// timer. Independent of the structured `*Cycled` events so
    /// other UI surfaces don't have to know about presentation.
    ShowCycleToast(String),
    /// Selected text drawable's background style — forwarded into
    /// the StyleToolbar so its dropdown re-seeds. Distinct from
    /// `TextBackgroundCycled` (which fires a toast + reapplies)
    /// because this is a UI-sync-only event.
    SelectionTextBackgroundChanged(crate::tools::TextBackground),
    SelectionArrowStyleChanged(crate::tools::ArrowStyle),
    SelectionBlurStyleChanged(crate::tools::BlurStyle),
    /// Selection-sync for Brush: the just-selected drawable's smoothing
    /// level. App forwards into `StyleToolbarInput::SetBrushPostSmooth`
    /// (the silent slider-snap path) so the slider mirrors the selected
    /// annotation without re-applying anything.
    SelectionBrushPostSmoothChanged(usize),
    /// Slider snapback events fired by sketch_board's ToolSelected
    /// handler when re-entering Spotlight or Highlighter — discard
    /// the previous session's in-flight slider value and read the
    /// saved default off `state.toml`.
    SpotlightDarknessReset(f32),
    HighlighterOpacityReset(f32),
    BrushPostSmoothReset(usize),
}

#[derive(Debug)]
enum AppCommandOutput {
    ResetResizable,
}

impl App {
    /// Revert is shown only on the dedicated crop bottom bar — i.e.
    /// when there's a crop AND the crop tool is active. Switching to
    /// any other tool hides it (the crop persists; the user reverts
    /// by switching back to Crop). Without this, Revert would clutter
    /// the regular drawing-mode toolbar after the first commit.
    fn update_revert_visibility(&self) {
        self.revert_button
            .set_visible(self.has_crop && self.current_tool == Tools::Crop);
    }

    /// Sync the canvas scrollbars to the renderer's current pan state.
    /// Visible only on axes where the scaled image exceeds the
    /// canvas (`upper > page_size`); otherwise there's nothing to
    /// scroll, so showing the bar would be pure noise.
    ///
    /// We flip `applying_scrollbar` while writing the adjustments so
    /// the `value_changed` callbacks ignore the programmatic updates
    /// — without this, the "renderer pans → scrollbar value updates
    /// → callback asks renderer to pan to that value → renderer
    /// pans" cycle would loop indefinitely.
    fn sync_scrollbars(&self, info: sketch_board::PanInfo) {
        self.applying_scrollbar.set(true);
        let configure = |bar: &gtk::Scrollbar,
                         drag: f32,
                         image_scaled: f32,
                         canvas: f32| {
            let needs = image_scaled > canvas + 0.5;
            bar.set_visible(needs);
            if !needs {
                return;
            }
            let adj = bar.adjustment();
            let excess = (image_scaled - canvas).max(0.0);
            // Scrollbar value = how far we've scrolled from the
            // top/left of the content. drag is the centered-pan
            // offset (positive moves image right/down within the
            // canvas), so value = excess/2 − drag.
            let value = (excess / 2.0 - drag).clamp(0.0, excess);
            adj.set_lower(0.0);
            adj.set_upper(image_scaled as f64);
            adj.set_page_size(canvas as f64);
            adj.set_step_increment((canvas as f64 * 0.1).max(1.0));
            adj.set_page_increment(canvas as f64);
            adj.set_value(value as f64);
        };
        configure(
            &self.scrollbar_h,
            info.drag_x,
            info.image_w_scaled,
            info.canvas_w,
        );
        configure(
            &self.scrollbar_v,
            info.drag_y,
            info.image_h_scaled,
            info.canvas_h,
        );
        self.applying_scrollbar.set(false);
    }

    fn get_monitor_size(root: &Window) -> Option<Rectangle> {
        root.surface().and_then(|surface| {
            DisplayManager::get()
                .default_display()
                .and_then(|display| display.monitor_at_surface(&surface))
                .map(|monitor| monitor.geometry())
        })
    }

    /// Effective DPI scale to display dimensions in — divides capture-
    /// native pixels into CSS-px equivalents so a 1498 × 218 image
    /// taken on a 2× HiDPI screen reads as 749 × 109. Explicit
    /// `input_scale` wins; otherwise we pick the larger of the
    /// widget's `scale_factor()` and the monitor's `scale_factor()`
    /// to handle Wayland fractional-scaling outputs where root can
    /// report 1.
    fn display_scale_divisor(root: &Window) -> i32 {
        let input_scale = APP_CONFIG.read().input_scale();
        if input_scale != 1.0 {
            return input_scale.round().max(1.0) as i32;
        }
        let root_scale = root.scale_factor().max(1);
        let monitor_scale = root
            .surface()
            .and_then(|s| {
                DisplayManager::get()
                    .default_display()
                    .and_then(|d| d.monitor_at_surface(&s))
            })
            .map(|m| m.scale_factor())
            .unwrap_or(1)
            .max(1);
        root_scale.max(monitor_scale)
    }

    /// Compute the window size to wrap `content_w × content_h` pixels
    /// (in CSS px, already DPR-corrected) with the standard image
    /// padding + toolbar chrome, capped to 90 % of `monitor` on each
    /// axis so the window never dominates the desktop. Shared between
    /// the initial-resize path and the `ContentSizeChanged` handler
    /// (which fires on crop commit / re-enter / revert).
    fn window_size_for_content(
        content_w: f64,
        content_h: f64,
        monitor: Option<Rectangle>,
    ) -> (i32, i32) {
        const IMAGE_PAD_CSS: f64 = 40.0;
        const TOOLBAR_CHROME_CSS: f64 = 120.0;
        let padded_w = content_w + 2.0 * IMAGE_PAD_CSS;
        let padded_h = content_h + 2.0 * IMAGE_PAD_CSS + TOOLBAR_CHROME_CSS;
        let (final_w, final_h) = if let Some(m) = monitor {
            let max_w = m.width() as f64 * 0.90;
            let max_h = m.height() as f64 * 0.90;
            (padded_w.min(max_w), padded_h.min(max_h))
        } else {
            (padded_w, padded_h)
        };
        (final_w as i32, final_h as i32)
    }

    fn resize_window_initial(&self, root: &Window, sender: ComponentSender<Self>) {
        let fullscreen = APP_CONFIG.read().fullscreen();
        let resize = APP_CONFIG.read().resize();
        let floating_hack = APP_CONFIG.read().floating_hack();

        // Convert image dimensions from PIXELS (capture-native, what
        // grim hands us — device px on HiDPI displays) to the GTK
        // window's CSS-px coordinate system. `input_scale` is the
        // explicit user override; when it's left at the default 1.0
        // we fall back to the widget's reported scale factor — and
        // for Wayland fractional-scaling outputs (where GTK4 reports
        // scale_factor=1 and relies on wp-fractional-scale) we also
        // probe the monitor object for its scale.
        let input_scale = APP_CONFIG.read().input_scale();
        let root_scale = root.scale_factor().max(1) as f64;
        let monitor_scale = root
            .surface()
            .and_then(|s| {
                DisplayManager::get()
                    .default_display()
                    .and_then(|d| d.monitor_at_surface(&s))
            })
            .map(|m| m.scale_factor() as f64)
            .unwrap_or(1.0)
            .max(1.0);
        let scale = if input_scale != 1.0 {
            input_scale as f64
        } else {
            root_scale.max(monitor_scale)
        };
        let image_width = self.image_dimensions.0 as f64 / scale;
        let image_height = self.image_dimensions.1 as f64 / scale;

        eprintln!(
            "Fullscreen {:?} | Resize {:?} | Floatinghack {:?}",
            fullscreen, resize, floating_hack
        );

        if fullscreen == Some(Fullscreen::All)
            && let Some(surface) = root.surface()
            && let Ok(toplevel) = surface.downcast::<Toplevel>()
        {
            toplevel.set_fullscreen_mode(FullscreenMode::AllMonitors);
        }

        let monitor_size_opt = Self::get_monitor_size(root);
        // Padding around the image (matches CANVAS_PADDING_CSS in the
        // renderer) + a generous estimate for the top/bottom toolbar
        // chrome. The renderer scales the image to fit whatever canvas
        // size GTK gives it, so an over-estimate just means a little
        // extra breathing room — under-estimating causes the image to
        // render at <100% even when it should fit at 1:1.
        const IMAGE_PAD_CSS: f64 = 40.0;
        const TOOLBAR_CHROME_CSS: f64 = 120.0;
        let padded_image_w = image_width + 2.0 * IMAGE_PAD_CSS;
        let padded_image_h = image_height + 2.0 * IMAGE_PAD_CSS + TOOLBAR_CHROME_CSS;

        let size_with_screen_cap = |max_w: f64, max_h: f64| -> (f64, f64) {
            // If padded image fits within caps, use it as-is so the
            // image renders at 1:1 with full padding. Otherwise clamp
            // to the cap on whichever axis is constrained — the
            // renderer will drop padding on that axis and scale the
            // image down to fit.
            let final_w = padded_image_w.min(max_w);
            let final_h = padded_image_h.min(max_h);
            (final_w, final_h)
        };

        // Force a real resize, not just a default-size hint. By the
        // time `connect_show` fires the window is already mapped at
        // the construction-time `set_default_size: (500, 500)`, and
        // Wayland compositors generally ignore later `set_default_size`
        // calls on a mapped surface — they only honor the initial
        // configure. So we hard-pin the size via `set_size_request`
        // to force the next configure round-trip to use these
        // dimensions, then clear the request 50 ms later so the user
        // can still drag the window's edges to resize. Same pattern
        // we use in `ContentSizeChanged` (crop commit / revert) —
        // documented there at length.
        let pin_size = |w: i32, h: i32| {
            root.set_default_size(w, h);
            root.set_size_request(w, h);
            let root_clone = root.clone();
            gtk::glib::timeout_add_local_once(
                std::time::Duration::from_millis(50),
                move || {
                    root_clone.set_size_request(-1, -1);
                },
            );
        };

        match resize {
            Some(Resize::Smart) if monitor_size_opt.is_some() => {
                let monitor_size = monitor_size_opt.unwrap();
                // Cap at 90% of the screen so the window doesn't
                // dominate the desktop. Match the user-facing spec:
                // "the satty window should render at 90% of screen
                // height" when the image doesn't fit at 100%.
                let max_w = monitor_size.width() as f64 * 0.90;
                let max_h = monitor_size.height() as f64 * 0.90;
                let (w, h) = size_with_screen_cap(max_w, max_h);
                pin_size(w as i32, h as i32);
            }
            Some(Resize::Size { width, height }) => {
                pin_size(width, height);
            }
            _ => {
                // Default path (no `--resize` flag): same 90%-cap +
                // padded behavior as Smart so users get the
                // breathing-room layout without an explicit config.
                if let Some(monitor_size) = monitor_size_opt {
                    let max_w = monitor_size.width() as f64 * 0.90;
                    let max_h = monitor_size.height() as f64 * 0.90;
                    let (w, h) = size_with_screen_cap(max_w, max_h);
                    pin_size(w as i32, h as i32);
                } else {
                    pin_size(padded_image_w as i32, padded_image_h as i32);
                }
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
                // Priority `1` was below GTK's default theme (600), so any
                // rule that conflicted with Adwaita's (e.g. button
                // `min-height`) silently lost. Use the documented
                // STYLE_PROVIDER_PRIORITY_APPLICATION (800) so our rules
                // outrank the theme but stay under user-level overrides.
                gtk::style_context_add_provider_for_display(
                    &display,
                    &css_provider,
                    gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
                );
                if let Some(css_provider2) = css_provider_override {
                    // User overrides win over our defaults — keep the
                    // ladder at +1.
                    gtk::style_context_add_provider_for_display(
                        &display,
                        &css_provider2,
                        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
                    );
                }
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
                    add_overlay: &model.scrollbar_h,
                    add_overlay: &model.scrollbar_v,
                },
                #[local_ref]
                bottom_row_clone -> gtk::CenterBox {
                    add_css_class: "bottom_row",
                    set_valign: gtk::Align::End,
                    set_halign: gtk::Align::Fill,
                    set_hexpand: true,
                    set_start_widget: Some(&model.start_cluster),
                    set_center_widget: Some(model.style_toolbar.widget()),
                    set_end_widget: Some(&model.end_cluster),
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
            AppInput::Realized => {
                self.resize_window_initial(root, sender.clone());
                // Make sure the canvas owns keyboard focus on startup —
                // GTK can otherwise auto-focus a toolbar widget (the
                // unified color picker MenuButton was a repeat offender),
                // breaking single-key shortcuts until the user clicks
                // the canvas or tabs past the toolbar.
                self.sketch_board
                    .sender()
                    .emit(SketchBoardInput::FocusCanvas);
                // First-run: launch the welcome dialog now that the
                // main window is on-screen so we have a parent to
                // attach it to. The dialog re-routes close requests
                // through Save so the user always exits with a
                // committed value.
                if self.welcome_pending {
                    self.welcome_pending = false;
                    let connector = WelcomeDialog::builder()
                        .transient_for(root)
                        .launch(WelcomeDialogInit {
                            detected_scale: self.detected_scale,
                            detected: self.scale_detected,
                        });
                    let controller =
                        connector.forward(sender.input_sender(), |out| match out {
                            WelcomeDialogOutput::Saved(value) => {
                                AppInput::WelcomeDialogSaved(value)
                            }
                        });
                    self.welcome_controller = Some(controller);
                }
            }
            AppInput::WelcomeDialogSaved(value) => {
                state::save_annotation_size_factor(value);
                APP_CONFIG.write().set_annotation_size_factor(value);
                // Update the toolbar display + emit the standard
                // AnnotationSizeChanged event so sketch_board picks it
                // up (the existing dialog uses the same handler).
                self.style_toolbar.sender().emit(
                    StyleToolbarInput::AnnotationDialogFinished(Some(value)),
                );
                self.welcome_controller = None;
            }
            AppInput::SnapToEdgesToggled(value) => {
                // Route into the standard ToolbarEvent path so
                // sketch_board's handler persists state + updates the
                // CropTool. Keeps the snap state in one place rather
                // than having the checkbox drive it from main.rs and
                // ALSO the toolbar (which it isn't part of anyway).
                self.sketch_board.sender().emit(
                    SketchBoardInput::ToolbarEvent(ToolbarEvent::SnapToEdgesChanged(value)),
                );
            }
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
                // Style toolbar adjusts which tool-specific controls are shown
                // (e.g. arrow-style dropdown) based on the active tool.
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::ToolChanged(tool));
                // Show the snap-to-edges checkbox + hint only while
                // cropping, matching the standard UX.
                let is_crop = tool == Tools::Crop;
                self.snap_to_edges_check.set_visible(is_crop);
                self.snap_to_edges_hint.set_visible(is_crop);
                self.current_tool = tool;
                self.update_revert_visibility();
            }
            AppInput::ColorSwitchShortcut(index) => {
                // When the user has hidden the default palette, the
                // 1–9, 0 shortcut keys pick from the first column of
                // saved-custom colors instead — that column is now
                // the picker's "primary" set.
                let button = if APP_CONFIG.read().hide_default_palette() {
                    ui::toolbars::ColorButtons::CustomSaved(index)
                } else {
                    ui::toolbars::ColorButtons::Palette(index)
                };
                self.tools_toolbar
                    .sender()
                    .emit(ToolsToolbarInput::ColorButtonSelected(button));
            }
            AppInput::ScaleFactorChanged => {
                self.sketch_board
                    .sender()
                    .emit(SketchBoardInput::ScaleFactorChanged);
            }
            AppInput::FullscreenChanged(fullscreen) => {
                let tools = self.tools_toolbar.widget();
                if fullscreen {
                    self.outer_box.remove(tools);
                    self.outer_box.remove(&self.bottom_row);
                    self.overlay.add_overlay(tools);
                    self.overlay.add_overlay(&self.bottom_row);
                } else {
                    self.overlay.remove_overlay(tools);
                    self.overlay.remove_overlay(&self.bottom_row);
                    self.outer_box.prepend(tools);
                    self.outer_box.append(&self.bottom_row);
                }
            }
            AppInput::DimensionsUpdate(dimensions) => {
                let d = dimensions.unwrap_or(self.image_dimensions);
                // Show the dimensions in the same coordinate system
                // the user perceives — divide by the display's DPR
                // so a 1498×218 image captured on a 2× HiDPI screen
                // reads as 749×109 (the visual size of the region
                // they framed), not the doubled device-pixel count.
                let scale = Self::display_scale_divisor(root);
                self.output_dimensions_label
                    .set_text(&format!("{} x {}", d.0 / scale, d.1 / scale));
                // (NB: the crop-mode toolbar W/H entries get their
                // live values via `CropEditDimensions` instead, so
                // they update on every drag without making the
                // bottom-right output-dims readout thrash too —
                // the readout fires only on commit / revert /
                // un-commit, when the OUTPUT actually changes.)
            }
            AppInput::CropEditDimensions { width, height } => {
                self.tools_toolbar.sender().emit(
                    ToolsToolbarInput::CropDimensionsChanged { width, height },
                );
            }
            AppInput::OpenPreferences => {
                ui::preferences::open(root);
            }
            AppInput::ZoomChanged(scale) => {
                self.zoom_indicator
                    .sender()
                    .emit(ZoomIndicatorInput::SetCurrentZoom(scale));
            }
            AppInput::CropPresenceChanged(present) => {
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::CropPresenceChanged(present));
                self.has_crop = present;
                self.update_revert_visibility();
            }
            AppInput::RevertCropClicked => {
                self.sketch_board.sender().emit(
                    SketchBoardInput::ToolbarEvent(ToolbarEvent::RevertCrop),
                );
            }
            AppInput::PanChanged(info) => {
                self.sync_scrollbars(info);
            }
            AppInput::ScrollbarChanged(is_horizontal, value) => {
                self.sketch_board
                    .sender()
                    .emit(SketchBoardInput::ScrollbarSet(is_horizontal, value));
            }
            AppInput::SelectionStyleChanged(style) => {
                if let Some(style) = style {
                    self.style_toolbar
                        .sender()
                        .emit(StyleToolbarInput::SyncFromSelection(style));
                } else {
                    // Selection went empty — pop the toolbar back
                    // to the active tool's saved default size.
                    self.style_toolbar
                        .sender()
                        .emit(StyleToolbarInput::SyncToToolDefault);
                }
            }
            AppInput::SelectionMultiAgreement {
                size,
                smooth,
                annotation,
            } => {
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::SyncMultiAgreement {
                        size,
                        smooth,
                        annotation,
                    });
            }
            AppInput::ToolSizeChanged(size) => {
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::SetCurrentSize(size));
            }
            AppInput::ContentSizeChanged { width, height } => {
                // Fit the window around the new content — applied via
                // `set_default_size`, which Wayland's compositor will
                // generally honor as a resize request. Same
                // input_scale → device-pixel-ratio fallback as the
                // initial-resize path: convert capture-native pixels
                // into the window's CSS-px coord system.
                let input_scale = APP_CONFIG.read().input_scale();
                let scale = if input_scale != 1.0 {
                    input_scale as f64
                } else {
                    root.scale_factor().max(1) as f64
                };
                let scaled_w = width as f64 / scale;
                let scaled_h = height as f64 / scale;
                let monitor = Self::get_monitor_size(root);
                let (w, h) =
                    Self::window_size_for_content(scaled_w, scaled_h, monitor);
                root.set_default_size(w, h);
                // GTK4's `set_default_size` reliably sizes a fresh
                // window but is mostly a hint once the window is
                // mapped — most compositors only honor the initial
                // configure, not later default-size changes. Force a
                // hard re-allocation by pinning the size via
                // `set_size_request` so the compositor's next
                // configure round-trip uses these dimensions, then
                // clear the request once the resize has settled so
                // the user can still drag the window's edges to
                // resize manually afterwards.
                root.set_size_request(w, h);
                let root_clone = root.clone();
                gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(50),
                    move || {
                        root_clone.set_size_request(-1, -1);
                    },
                );
            }
            AppInput::ImageDimensionsChanged { width, height } => {
                // Update the underlying image_dimensions field so the
                // crop-mode toolbar's "Image size" label and resize
                // popover both reflect the live value. Also push
                // through to ToolsToolbar.
                self.image_dimensions = (width, height);
                self.tools_toolbar.sender().emit(
                    ToolsToolbarInput::ImageDimensionsChanged { width, height },
                );
            }
            AppInput::FillShapesChanged(fill) => {
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::SetFillShapes(fill));
            }
            AppInput::ArrowStyleCycled(style) => {
                // Update the toolbar's local mirror + MenuButton
                // preview. The handler also re-emits ArrowStyleSelected
                // upstream, which lands back in sketch_board and
                // idempotently re-applies the same style — the cost
                // is a redundant state.toml write, never an infinite
                // loop (sketch_board doesn't re-emit on apply).
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::SetArrowStyle {
                        style,
                        emit_upstream: true,
                    });
            }
            AppInput::BlurStyleCycled(style) => {
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::SetBlurStyle {
                        style,
                        emit_upstream: true,
                    });
            }
            AppInput::TextBackgroundCycled(bg) => {
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::SetTextBackground {
                        bg,
                        emit_upstream: true,
                    });
            }
            AppInput::HighlighterStyleCycled(style) => {
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::SetHighlighterStyle(style));
            }
            AppInput::SelectionTextBackgroundChanged(bg) => {
                // Silent dropdown update — no toast, no reapply.
                // Just push the value into the StyleToolbar so its
                // dropdown shows the selected drawable's current
                // background.
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::SetTextBackground {
                        bg,
                        emit_upstream: false,
                    });
            }
            AppInput::SelectionArrowStyleChanged(style) => {
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::SetArrowStyle {
                        style,
                        emit_upstream: false,
                    });
            }
            AppInput::SelectionBlurStyleChanged(style) => {
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::SetBlurStyle {
                        style,
                        emit_upstream: false,
                    });
            }
            AppInput::SelectionBrushPostSmoothChanged(value) => {
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::SetBrushPostSmooth(value));
            }
            AppInput::SpotlightDarknessReset(value) => {
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::SetSpotlightDarkness(value));
            }
            AppInput::HighlighterOpacityReset(value) => {
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::SetHighlighterOpacity(value));
            }
            AppInput::BrushPostSmoothReset(value) => {
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::SetBrushPostSmooth(value));
            }
            AppInput::ShowCycleToast(text) => {
                // Refresh the label, reveal, and reset the hide timer
                // so rapid-fire cycles slide the deadline forward
                // instead of stacking callbacks. `timeout_add_local_once`
                // returns a `SourceId` that's `Drop`-cancelled on
                // remove, so the new timer cleanly replaces the old.
                self.cycle_toast_label.set_text(&text);
                self.cycle_toast_revealer.set_reveal_child(true);
                if let Some(id) = self.cycle_toast_timer.borrow_mut().take() {
                    id.remove();
                }
                let revealer = self.cycle_toast_revealer.clone();
                let timer_slot = self.cycle_toast_timer.clone();
                let id = gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(1200),
                    move || {
                        revealer.set_reveal_child(false);
                        *timer_slot.borrow_mut() = None;
                    },
                );
                *self.cycle_toast_timer.borrow_mut() = Some(id);
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
                    SketchBoardOutput::ZoomChanged(scale) => AppInput::ZoomChanged(scale),
                    SketchBoardOutput::CropPresenceChanged(present) => {
                        AppInput::CropPresenceChanged(present)
                    }
                    SketchBoardOutput::PanChanged(info) => AppInput::PanChanged(info),
                    SketchBoardOutput::SelectionStyleChanged(style) => {
                        AppInput::SelectionStyleChanged(style)
                    }
                    SketchBoardOutput::SelectionMultiAgreement {
                        size,
                        smooth,
                        annotation,
                    } => AppInput::SelectionMultiAgreement {
                        size,
                        smooth,
                        annotation,
                    },
                    SketchBoardOutput::ToolSizeChanged(size) => {
                        AppInput::ToolSizeChanged(size)
                    }
                    SketchBoardOutput::ContentSizeChanged { width, height } => {
                        AppInput::ContentSizeChanged { width, height }
                    }
                    SketchBoardOutput::ImageDimensionsChanged { width, height } => {
                        AppInput::ImageDimensionsChanged { width, height }
                    }
                    SketchBoardOutput::FillShapesChanged(fill) => {
                        AppInput::FillShapesChanged(fill)
                    }
                    SketchBoardOutput::CropEditDimensions { width, height } => {
                        AppInput::CropEditDimensions { width, height }
                    }
                    SketchBoardOutput::OpenPreferences => AppInput::OpenPreferences,
                    SketchBoardOutput::ArrowStyleCycled(style) => {
                        AppInput::ArrowStyleCycled(style)
                    }
                    SketchBoardOutput::BlurStyleCycled(style) => {
                        AppInput::BlurStyleCycled(style)
                    }
                    SketchBoardOutput::TextBackgroundCycled(bg) => {
                        AppInput::TextBackgroundCycled(bg)
                    }
                    SketchBoardOutput::HighlighterStyleCycled(style) => {
                        AppInput::HighlighterStyleCycled(style)
                    }
                    SketchBoardOutput::ShowCycleToast(text) => {
                        AppInput::ShowCycleToast(text)
                    }
                    SketchBoardOutput::SelectionTextBackgroundChanged(bg) => {
                        AppInput::SelectionTextBackgroundChanged(bg)
                    }
                    SketchBoardOutput::SelectionArrowStyleChanged(s) => {
                        AppInput::SelectionArrowStyleChanged(s)
                    }
                    SketchBoardOutput::SelectionBlurStyleChanged(s) => {
                        AppInput::SelectionBlurStyleChanged(s)
                    }
                    SketchBoardOutput::SelectionBrushPostSmoothChanged(v) => {
                        AppInput::SelectionBrushPostSmoothChanged(v)
                    }
                    SketchBoardOutput::SpotlightDarknessReset(v) => {
                        AppInput::SpotlightDarknessReset(v)
                    }
                    SketchBoardOutput::HighlighterOpacityReset(v) => {
                        AppInput::HighlighterOpacityReset(v)
                    }
                    SketchBoardOutput::BrushPostSmoothReset(v) => {
                        AppInput::BrushPostSmoothReset(v)
                    }
                });

        // Toolbars
        let tools_toolbar = ToolsToolbar::builder()
            .launch(())
            .forward(sketch_board.sender(), SketchBoardInput::ToolbarEvent);

        let style_toolbar = StyleToolbar::builder()
            .launch(())
            .forward(sketch_board.sender(), SketchBoardInput::ToolbarEvent);

        let zoom_indicator = ZoomIndicator::builder().launch(1.0).forward(
            sketch_board.sender(),
            |out| match out {
                ZoomIndicatorOutput::Command(cmd) => SketchBoardInput::ZoomCommand(cmd),
                ZoomIndicatorOutput::FocusCanvas => SketchBoardInput::FocusCanvas,
            },
        );

        let outer_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        // Hard minimum width for the whole window. Below this the
        // bottom StyleToolbar's fixed-width slots (mirror spacer,
        // size slider, tool cluster) start to clip and the top
        // toolbar's right-side buttons fall off the visible edge.
        // GTK forwards this to the compositor as the toplevel's
        // min-size hint.
        outer_box.set_width_request(1060);
        let outer_box_clone = outer_box.clone();
        let overlay = gtk::Overlay::new();
        let overlay_clone = overlay.clone();
        let bottom_row = gtk::CenterBox::new();
        // Fixed row height — keeps the bar identical in regular and
        // crop modes (StyleToolbar gets hidden in Crop, but the row's
        // height stays put so the canvas doesn't shift). 55 px gives
        // the compact 34-px buttons a comfortable margin and leaves
        // room for the size slider's detent labels.
        bottom_row.set_height_request(55);
        let bottom_row_clone = bottom_row.clone();

        // Canvas scrollbars. The adjustments are managed dynamically
        // from the `PanChanged` handler (which updates upper /
        // page_size / value when the zoom or pan changes). We start
        // them hidden — they reveal themselves automatically the
        // moment the image's scaled width or height exceeds the
        // canvas. Both are overlaid on the canvas Overlay (not in the
        // outer Box) so they sit on top of the drawing surface and
        // hug its edges rather than carving out vertical space from
        // the bottom toolbar.
        let h_adj = gtk::Adjustment::new(0.0, 0.0, 1.0, 1.0, 10.0, 1.0);
        let v_adj = gtk::Adjustment::new(0.0, 0.0, 1.0, 1.0, 10.0, 1.0);
        let scrollbar_h = gtk::Scrollbar::new(gtk::Orientation::Horizontal, Some(&h_adj));
        scrollbar_h.set_visible(false);
        scrollbar_h.set_valign(gtk::Align::End);
        scrollbar_h.set_halign(gtk::Align::Fill);
        let scrollbar_v = gtk::Scrollbar::new(gtk::Orientation::Vertical, Some(&v_adj));
        scrollbar_v.set_visible(false);
        scrollbar_v.set_valign(gtk::Align::Fill);
        scrollbar_v.set_halign(gtk::Align::End);
        let applying_scrollbar = std::rc::Rc::new(std::cell::Cell::new(false));
        {
            let sender_clone = sender.clone();
            let applying = applying_scrollbar.clone();
            h_adj.connect_value_changed(move |adj| {
                if applying.get() {
                    return;
                }
                sender_clone.input(AppInput::ScrollbarChanged(true, adj.value() as f32));
            });
        }
        {
            let sender_clone = sender.clone();
            let applying = applying_scrollbar.clone();
            v_adj.connect_value_changed(move |adj| {
                if applying.get() {
                    return;
                }
                sender_clone.input(AppInput::ScrollbarChanged(false, adj.value() as f32));
            });
        }

        // Snap-to-edges cluster: lives in bottom_row.start_widget
        // alongside the zoom indicator. The checkbox + hint label 
        // only show while the crop tool is active so they don't 
        // add noise during regular annotation. Initial value pulled 
        // from state (defaults to true).
        let snap_initial = state::load_snap_to_edges().unwrap_or(true);
        let snap_to_edges_check = gtk::CheckButton::builder()
            .label("Snap to edges")
            .active(snap_initial)
            .focusable(false)
            .visible(false)
            .build();
        // Custom CSS class trims the indicator + label down to match the
        // compact bottom-row chrome — defaults are sized for full-window
        // dialogs and read as oversized in the slim crop bar.
        snap_to_edges_check.add_css_class("snap-toggle");
        let snap_to_edges_hint = gtk::Label::builder()
            .label("Hold Ctrl to disable snapping.")
            .visible(false)
            .margin_start(8)
            .build();
        snap_to_edges_hint.add_css_class("dim-label");
        snap_to_edges_hint.add_css_class("snap-hint");
        {
            let sender_clone = sender.clone();
            snap_to_edges_check.connect_toggled(move |btn| {
                sender_clone.input(AppInput::SnapToEdgesToggled(btn.is_active()));
            });
        }
        // Single horizontal cluster — zoom indicator, snap checkbox,
        // and the "Hold Ctrl…" hint sit in one row. The crop tool now
        // has its own dedicated bottom bar (StyleToolbar hides when
        // `current_tool == Crop`), so the cluster growing wider on
        // crop entry can't push the central toolbar around — there's
        // nothing in the center to push.
        let start_cluster = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        start_cluster.append(zoom_indicator.widget());
        start_cluster.append(&snap_to_edges_check);
        start_cluster.append(&snap_to_edges_hint);

        // "Revert to Original" lives outside the centered StyleToolbar
        // so its visibility toggling doesn't shift the toolbar's
        // width. Mounted as `bottom_row.end_widget`; CropPresenceChanged
        // flips visibility from main.rs's update.
        //
        // `valign: Center` + margin_top/bottom keeps it from
        // stretching to the bottom row's full height — without these,
        // the CenterBox's end-widget slot would expand it vertically.
        let revert_button = gtk::Button::builder()
            .label("Revert to Original")
            .focusable(false)
            .hexpand(false)
            .visible(false)
            .valign(gtk::Align::Center)
            .margin_top(8)
            .margin_bottom(8)
            .margin_end(8)
            .tooltip_text("Remove the crop and show the full image")
            .build();

        // Bottom-right end cluster — output dimensions label sits
        // opposite the zoom indicator (bottom-left), and the Revert
        // button tucks alongside it when a crop is present. The
        // dimensions label stays visible during Crop mode so the
        // user can see the cropped output size live as they drag.
        let output_dimensions_label = gtk::Label::builder()
            .focusable(false)
            .hexpand(false)
            // 13 chars fits "WWWW x HHHHH" comfortably (the new spaced
            // "WxH" form is three chars wider than the prior tight one).
            .width_chars(13)
            .margin_end(12)
            .valign(gtk::Align::Center)
            .build();
        output_dimensions_label.add_css_class("dim-label");
        // Custom hover-tooltip (750 ms delay) instead of GTK's built-in
        // — matches the rest of the toolbar chrome.
        output_dimensions_label.install_tooltip_above("Output dimensions (width × height)");
        let end_cluster = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .valign(gtk::Align::Center)
            .build();
        // Revert sits first (left), output dimensions stays on the
        // right edge of the bottom row. Without this swap the
        // dimensions label visually jumped between the two views as
        // Revert appeared / disappeared with crop presence.
        end_cluster.append(&revert_button);
        end_cluster.append(&output_dimensions_label);
        {
            let sender_clone = sender.clone();
            revert_button.connect_clicked(move |_| {
                sender_clone.input(AppInput::RevertCropClicked);
            });
        }

        // Determine whether the user has already committed an
        // annotation_size_factor. If not, we'll surface the welcome
        // dialog on Realized. The detected scale is captured here so
        // the dialog can pre-fill its SpinButton — we look at Hyprland
        // first since it's the only Wayland compositor exposing
        // fractional scales reliably; everything else falls back to
        // 1.0× and the user picks their own value.
        let welcome_pending = state::load_annotation_size_factor().is_none();
        let detected = display::detect_hyprland_scale();
        let detected_scale = detected.unwrap_or(1.0);
        let scale_detected = detected.is_some();

        // Center-of-canvas toast for cycle announcements. Built once,
        // added to the canvas overlay below the scrollbars so they
        // still hit-test on top. Label text is set per-event; the
        // Revealer fades the label in/out via Crossfade.
        let cycle_toast_label = gtk::Label::builder()
            .label("")
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        cycle_toast_label.add_css_class("cycle-toast-label");
        let cycle_toast_revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .transition_duration(150)
            .reveal_child(false)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            // The Revealer steals pointer events when it sits on top
            // of the canvas, which would block click-to-draw mid-toast.
            // Pass-through everything so the toast is purely visual.
            .can_target(false)
            .build();
        cycle_toast_revealer.set_child(Some(&cycle_toast_label));
        overlay.add_overlay(&cycle_toast_revealer);
        let cycle_toast_timer =
            std::rc::Rc::new(std::cell::RefCell::new(None::<gtk::glib::SourceId>));

        // Model
        let model = App {
            sketch_board,
            tools_toolbar,
            style_toolbar,
            zoom_indicator,
            image_dimensions,
            outer_box,
            overlay,
            bottom_row,
            welcome_pending,
            detected_scale,
            scale_detected,
            welcome_controller: None,
            snap_to_edges_check,
            snap_to_edges_hint,
            start_cluster,
            revert_button,
            output_dimensions_label,
            end_cluster,
            current_tool: APP_CONFIG.read().initial_tool(),
            has_crop: false,
            scrollbar_h,
            scrollbar_v,
            applying_scrollbar,
            cycle_toast_label,
            cycle_toast_revealer,
            cycle_toast_timer,
        };

        // Seed the bottom-right output dimensions label with the
        // full image size so something's visible immediately —
        // sketch_board republishes via DimensionsUpdate whenever the
        // crop changes. Format must match the spaced "W x H" form
        // used in the DimensionsUpdate handler above. Divide by DPR
        // here too so the seeded value isn't doubled compared to
        // what the user sees rendered on screen.
        let display_scale = Self::display_scale_divisor(&root);
        model.output_dimensions_label.set_text(&format!(
            "{} x {}",
            image_dimensions.0 / display_scale,
            image_dimensions.1 / display_scale,
        ));

        // Seed the ToolsToolbar's image_dimensions mirror so the
        // crop-mode "Image size: W × H px" MenuButton label shows
        // the correct values from launch (not 0×0). Subsequent
        // changes flow via SketchBoardOutput::ImageDimensionsChanged.
        // Push the display DPR FIRST so the seeding code below
        // applies it (otherwise the entries get image-pixel values
        // until the first manual change).
        model
            .tools_toolbar
            .sender()
            .emit(ToolsToolbarInput::SetDisplayScale(display_scale));
        model.tools_toolbar.sender().emit(
            ToolsToolbarInput::ImageDimensionsChanged {
                width: image_dimensions.0,
                height: image_dimensions.1,
            },
        );

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

        // Hyprland Super+wheel override. Omarchy's tiling-v2.conf
        // binds `SUPER, mouse_up` / `mouse_down` to workspace
        // switching at the compositor level — those binds fire
        // before GTK sees the wheel event, so Satty's scroll handler
        // never gets Super+wheel without an opt-out. We snapshot
        // those specific binds at app init via `hyprctl binds -j`,
        // then on focus-in we `hyprctl keyword unbind` them so the
        // wheel falls through to GTK, and on focus-out / destroy we
        // re-issue `hyprctl keyword bind ...` from the snapshot so
        // workspace switching works again everywhere else. The user's
        // hyprland.conf is never touched — this is purely a runtime
        // overlay, and the snapshot is empty on non-Hyprland hosts so
        // every hyprctl call below is a clean no-op. If Satty hard-
        // crashes mid-focus the user can recover with `hyprctl
        // reload` (which re-parses their conf and re-installs the
        // binds we'd unbound).
        let saved_binds = std::rc::Rc::new(crate::hyprland::read_super_mouse_binds());
        let focus_controller = gtk::EventControllerFocus::new();
        focus_controller.connect_enter(|_| {
            crate::hyprland::unbind_super_mouse();
        });
        let binds_for_leave = saved_binds.clone();
        focus_controller.connect_leave(move |_| {
            crate::hyprland::rebind_all(&binds_for_leave);
        });
        root.add_controller(focus_controller);
        // Belt and suspenders: rebind on window destroy too. Either
        // signal alone can miss in some shutdown paths (Wayland
        // close before focus-leave; SIGTERM bypassing GTK's destroy
        // chain). Both together cover every normal exit. SIGKILL or
        // a hard crash can still leave the binds suspended — manual
        // recovery is `hyprctl reload`.
        let binds_for_destroy = saved_binds.clone();
        root.connect_destroy(move |_| {
            crate::hyprland::rebind_all(&binds_for_destroy);
        });

        generate_profile_output!("app init end");

        relm4::gtk::glib::idle_add_local_once(move || {
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

    // Fold the persisted annotation_size_factor (if any) into
    // APP_CONFIG before launching the GUI so toolbar / sketch_board
    // components see the saved value at init time. When nothing is
    // persisted the welcome dialog runs on first realize and the
    // user's chosen value flows back here through the same path.
    if let Some(saved) = state::load_annotation_size_factor() {
        APP_CONFIG.write().set_annotation_size_factor(saved);
    }

    // Snapshot the config values we need into owned locals, then
    // drop the read guard before app.run. `app.run` blocks for the
    // entire app lifetime; holding the read guard across it would
    // deadlock any `APP_CONFIG.write()` made during the run (e.g.
    // the welcome dialog persisting `annotation_size_factor`).
    let (input_filename, app_id_pref): (String, Option<String>) = {
        let config = APP_CONFIG.read();
        (
            config.input_filename().to_string(),
            config.app_id().map(|s| s.to_string()),
        )
    };

    generate_profile_output!("loading image");
    // load input image
    let image = if input_filename == "-" {
        let mut buf = Vec::<u8>::new();
        io::stdin().lock().read_to_end(&mut buf)?;
        let pb_loader = PixbufLoader::new();
        pb_loader.write(&buf)?;
        pb_loader.close()?;
        pb_loader
            .pixbuf()
            .ok_or(anyhow!("Conversion to Pixbuf failed"))?
    } else {
        Pixbuf::from_file(&input_filename).context("couldn't load image")?
    };

    generate_profile_output!("image loaded, starting gui");
    // Pre-compute the text-band detection once on the loaded image.
    // The Highlighter tool reads this cache via `text_bands::bands()`
    // for both the snap-on-drag and hover-preview features. Done
    // before relm4 launches so the bands are available the first
    // time the user hovers the canvas — the scan is fast enough
    // (~20 ms on a 4K capture) that the user can't tell it ran.
    text_bands::init_from_pixbuf(&image);
    generate_profile_output!("text bands detected");
    // start GUI
    let app = relm4::main_application();
    let app_id = match app_id_pref.as_deref() {
        Some(id) if Application::id_is_valid(id) => Some(id),
        Some(id) => {
            eprintln!("Invalid app id: {}, using fallback", id);
            Some("com.gabm.satty")
        }
        None => Some("com.gabm.satty"),
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
