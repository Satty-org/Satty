use std::cell::RefCell;
use std::hash::Hasher;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Result, anyhow};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use relm4::gtk;
use relm4::gtk::cairo;
use relm4::gtk::gdk_pixbuf::Pixbuf;
use relm4::gtk::glib;
use relm4::gtk::prelude::*;

use crate::capture;

pub mod auto_scroll;

const BACKDROP_ALPHA: f64 = 0.55;
const BRACKET_LEN: f64 = 22.0;
const BRACKET_WIDTH: f64 = 3.0;
const PILL_GAP: f64 = 18.0;
const MIN_SELECTION: f64 = 8.0;
const CAPTURE_INTERVAL_MS: u64 = 100;
const STRIPE_ROWS: i32 = 6;
const DRAG_THRESHOLD: f64 = 4.0;

#[derive(Default, Clone, Copy, Debug)]
struct Selection {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl Selection {
    fn is_valid(&self) -> bool {
        self.w >= MIN_SELECTION && self.h >= MIN_SELECTION
    }
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Phase {
    AwaitingDrag,
    Dragging,
    Selected,
    Capturing,
}

struct CapturedFrame {
    pixbuf: Pixbuf,
    stripe_hash: u64,
}

struct OverlayState {
    phase: Phase,
    drag_origin: (f64, f64),
    drag_active: bool,
    selection: Selection,
    frames: Vec<CapturedFrame>,
    capture_timer: Option<glib::SourceId>,
    auto_scroll_stop: Option<Arc<AtomicBool>>,
    auto_scroll_baseline_frames: usize,
    auto_scroll_quiet_ticks: u32,
    auto_scroll_monitor: Option<glib::SourceId>,
}

pub fn run() -> Result<()> {
    let app = gtk::Application::builder()
        .application_id("com.gabm.satty.scroll-capture")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();

    app.connect_activate(build_overlay);

    // Don't let GTK try to parse satty's CLI args.
    let exit_code = app.run_with_args::<&str>(&[]);
    if exit_code != gtk::glib::ExitCode::SUCCESS {
        return Err(anyhow!(
            "scroll-capture overlay exited with code {:?}",
            exit_code
        ));
    }
    Ok(())
}

fn build_overlay(app: &gtk::Application) {
    let state = Rc::new(RefCell::new(OverlayState {
        phase: Phase::AwaitingDrag,
        drag_origin: (0.0, 0.0),
        drag_active: false,
        selection: Selection::default(),
        frames: Vec::new(),
        capture_timer: None,
        auto_scroll_stop: None,
        auto_scroll_baseline_frames: 0,
        auto_scroll_quiet_ticks: 0,
        auto_scroll_monitor: None,
    }));

    let window = gtk::ApplicationWindow::new(app);
    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_keyboard_mode(KeyboardMode::Exclusive);
    window.set_namespace(Some("satty-scroll-capture"));
    for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
        window.set_anchor(edge, true);
    }
    // -1 = ignore other layer-shell exclusive zones (e.g. waybar) so we cover
    // the entire output edge-to-edge.
    window.set_exclusive_zone(-1);
    window.add_css_class("scroll-capture-overlay");

    install_css(app);

    let overlay = gtk::Overlay::new();
    window.set_child(Some(&overlay));

    let drawing = gtk::DrawingArea::new();
    drawing.set_hexpand(true);
    drawing.set_vexpand(true);
    overlay.set_child(Some(&drawing));

    // Pill widgets go directly into the gtk::Overlay (not into a gtk::Fixed):
    // Fixed allocates itself 0x0 since children are transform-positioned,
    // which leaves transformed children outside the pick rect even though
    // they render fine. Overlay children sized via halign+valign+margins are
    // pickable by the same allocation that draws them.
    let prompt = build_prompt_pill();
    let action_pill = build_action_pill();
    let capturing_pill = build_capturing_pill();

    for pill in [&prompt, &action_pill, &capturing_pill] {
        pill.set_halign(gtk::Align::Start);
        pill.set_valign(gtk::Align::Start);
        overlay.add_overlay(pill);
    }
    action_pill.set_visible(false);
    capturing_pill.set_visible(false);

    // Drawing function pulls from state on each invalidation.
    {
        let state = Rc::clone(&state);
        drawing.set_draw_func(move |_, cr, w, h| {
            let s = state.borrow();
            draw_backdrop(cr, w as f64, h as f64, &s);
        });
    }

    // Drag-select gesture.
    let drag = gtk::GestureDrag::new();
    {
        let state = Rc::clone(&state);
        let drawing_w = drawing.clone();
        let prompt_w = prompt.clone();
        let action_pill_w = action_pill.clone();
        drag.connect_drag_begin(move |_, x, y| {
            // Record origin only. Phase/selection changes happen lazily in
            // drag_update once the user crosses DRAG_THRESHOLD of motion. A
            // tap (zero/tiny motion) is a no-op, so missing a pill button by
            // a few px doesn't reset existing state.
            let mut s = state.borrow_mut();
            s.drag_origin = (x, y);
            s.drag_active = false;
            let _ = (&prompt_w, &action_pill_w, &drawing_w);
        });
    }
    {
        let state = Rc::clone(&state);
        let drawing_w = drawing.clone();
        let prompt_w = prompt.clone();
        let action_pill_w = action_pill.clone();
        let capturing_pill_w = capturing_pill.clone();
        drag.connect_drag_update(move |_, dx, dy| {
            let mut s = state.borrow_mut();
            if !s.drag_active {
                if dx.abs() < DRAG_THRESHOLD && dy.abs() < DRAG_THRESHOLD {
                    return;
                }
                // Threshold crossed — commit to a real drag.
                s.drag_active = true;
                s.phase = Phase::Dragging;
                drop(s);
                prompt_w.set_visible(false);
                action_pill_w.set_visible(false);
                capturing_pill_w.set_visible(false);
                let mut s = state.borrow_mut();
                let (ox, oy) = s.drag_origin;
                let x = ox.min(ox + dx);
                let y = oy.min(oy + dy);
                s.selection = Selection { x, y, w: dx.abs(), h: dy.abs() };
                drop(s);
                drawing_w.queue_draw();
                return;
            }
            let (ox, oy) = s.drag_origin;
            let x = ox.min(ox + dx);
            let y = oy.min(oy + dy);
            s.selection = Selection { x, y, w: dx.abs(), h: dy.abs() };
            drop(s);
            drawing_w.queue_draw();
        });
    }
    {
        let state = Rc::clone(&state);
        let drawing_w = drawing.clone();
        let overlay_w = overlay.clone();
        let window_w = window.clone();
        let action_pill_w = action_pill.clone();
        let prompt_w = prompt.clone();
        drag.connect_drag_end(move |_, _dx, _dy| {
            let mut s = state.borrow_mut();
            if !s.drag_active {
                // Tap that missed a button: leave state alone so the existing
                // pill (if any) remains and the user can try again.
                return;
            }
            s.drag_active = false;
            if s.selection.is_valid() {
                s.phase = Phase::Selected;
                let sel = s.selection;
                drop(s);
                action_pill_w.set_visible(true);
                // Drop keyboard exclusivity and restrict the input region to
                // just the pill, so the user can scroll-reposition the
                // underlying content before clicking Start Capture.
                window_w.set_keyboard_mode(KeyboardMode::OnDemand);
                position_action_pill_and_input(&window_w, &overlay_w, &action_pill_w, sel);
                prompt_w.set_visible(false);
            } else {
                s.phase = Phase::AwaitingDrag;
                s.selection = Selection::default();
                drop(s);
                prompt_w.set_visible(true);
                action_pill_w.set_visible(false);
            }
            drawing_w.queue_draw();
        });
    }
    drawing.add_controller(drag);

    // Center the prompt once we know the surface size.
    {
        let prompt_w = prompt.clone();
        drawing.connect_resize(move |_, w, h| {
            let (pw, ph) = pill_natural_size(&prompt_w);
            let x = ((w as f64 - pw) / 2.0).max(0.0);
            let y = ((h as f64 - ph) / 2.0).max(0.0);
            prompt_w.set_margin_start(x as i32);
            prompt_w.set_margin_top(y as i32);
        });
    }

    // Esc cancels.
    let keys = gtk::EventControllerKey::new();
    {
        let window_w = window.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                window_w.close();
                gtk::glib::Propagation::Stop
            } else {
                gtk::glib::Propagation::Proceed
            }
        });
    }
    window.add_controller(keys);

    // Wire pre-capture pill buttons (Cancel / Start Capture).
    {
        let window_w = window.clone();
        let cancel: gtk::Button = action_pill
            .first_child()
            .and_then(|c| c.downcast().ok())
            .expect("action pill missing cancel button");
        cancel.connect_clicked(move |_| window_w.close());
    }
    {
        let state = Rc::clone(&state);
        let window_w = window.clone();
        let action_pill_w = action_pill.clone();
        let capturing_pill_w = capturing_pill.clone();
        let overlay_w = overlay.clone();
        let drawing_w = drawing.clone();
        let start: gtk::Button = action_pill
            .last_child()
            .and_then(|c| c.downcast().ok())
            .expect("action pill missing start-capture button");
        start.connect_clicked(move |_| {
            start_capture(
                &state,
                &window_w,
                &overlay_w,
                &action_pill_w,
                &capturing_pill_w,
                &drawing_w,
            );
        });
    }

    // Wire capturing-pill buttons (Cancel / Auto-Scroll / Done).
    wire_capturing_pill(&state, &window, &capturing_pill);

    window.present();
}

fn start_capture(
    state: &Rc<RefCell<OverlayState>>,
    window: &gtk::ApplicationWindow,
    overlay: &gtk::Overlay,
    action_pill: &gtk::Box,
    capturing_pill: &gtk::Box,
    drawing: &gtk::DrawingArea,
) {
    let sel = state.borrow().selection;
    state.borrow_mut().phase = Phase::Capturing;

    // Drop keyboard exclusivity. On Hyprland (and possibly other wlroots
    // compositors), an Exclusive-keyboard layer surface appears to consume
    // pointer events too — set_input_region's restriction is ignored and
    // everything inside the surface bounds gets captured. OnDemand mode
    // routes pointer events according to the surface's input region as we
    // expect. Esc only works while the overlay has focus (i.e. right after
    // a click or while hovering a pill button) — Cancel/Done are the primary
    // exits anyway.
    window.set_keyboard_mode(KeyboardMode::OnDemand);

    action_pill.set_visible(false);
    capturing_pill.set_visible(true);
    position_capturing_pill_and_input(window, overlay, capturing_pill, sel);
    drawing.queue_draw();

    // Start the capture timer.
    let timer = glib::timeout_add_local(Duration::from_millis(CAPTURE_INTERVAL_MS), {
        let state = Rc::clone(state);
        move || capture_tick(&state, sel)
    });
    state.borrow_mut().capture_timer = Some(timer);
}

fn capture_tick(state: &Rc<RefCell<OverlayState>>, sel: Selection) -> glib::ControlFlow {
    if state.borrow().phase != Phase::Capturing {
        return glib::ControlFlow::Break;
    }
    let rect = capture::Rect {
        x: sel.x.round() as i32,
        y: sel.y.round() as i32,
        width: sel.w.round() as i32,
        height: sel.h.round() as i32,
    };
    match capture::capture_region(rect) {
        Ok(pixbuf) => {
            let hash = stripe_hash(&pixbuf);
            let mut s = state.borrow_mut();
            let last_hash = s.frames.last().map(|f| f.stripe_hash);
            if last_hash != Some(hash) {
                s.frames.push(CapturedFrame {
                    pixbuf,
                    stripe_hash: hash,
                });
                eprintln!("scroll-capture: kept frame {}", s.frames.len());
            }
        }
        Err(e) => {
            eprintln!("scroll-capture: capture_region failed: {e}");
        }
    }
    glib::ControlFlow::Continue
}

fn wire_capturing_pill(
    state: &Rc<RefCell<OverlayState>>,
    window: &gtk::ApplicationWindow,
    pill: &gtk::Box,
) {
    let mut child = pill.first_child();
    let mut idx = 0;
    while let Some(c) = child {
        let next = c.next_sibling();
        if let Ok(button) = c.downcast::<gtk::Button>() {
            match idx {
                0 => {
                    // Cancel
                    let window_w = window.clone();
                    let state = Rc::clone(state);
                    button.connect_clicked(move |_| {
                        stop_capture(&state);
                        window_w.close();
                    });
                }
                1 => {
                    // Auto-Scroll: spawn worker that sends wheel events via
                    // wlr-virtual-pointer, plus a monitor timer that watches
                    // for end-of-content (no new retained frames for ~1.5s).
                    let state = Rc::clone(state);
                    let capturing_pill = pill.clone();
                    let window_w = window.clone();
                    button.connect_clicked(move |_| {
                        start_auto_scroll(&state, &window_w, &capturing_pill);
                    });
                }
                2 => {
                    // Done — stop the timer, log frame count, close. Phase 5
                    // will replace this with stitch + handoff into the canvas.
                    let window_w = window.clone();
                    let state = Rc::clone(state);
                    button.connect_clicked(move |_| {
                        stop_capture(&state);
                        let n = state.borrow().frames.len();
                        eprintln!("scroll-capture: Done — {n} frame(s) captured");
                        window_w.close();
                    });
                }
                _ => {}
            }
            idx += 1;
        }
        child = next;
    }
}

fn stop_capture(state: &Rc<RefCell<OverlayState>>) {
    let timer = state.borrow_mut().capture_timer.take();
    if let Some(t) = timer {
        t.remove();
    }
    let monitor = state.borrow_mut().auto_scroll_monitor.take();
    if let Some(m) = monitor {
        m.remove();
    }
    if let Some(stop) = state.borrow_mut().auto_scroll_stop.take() {
        stop.store(true, Ordering::Relaxed);
    }
    state.borrow_mut().phase = Phase::Selected;
}

fn start_auto_scroll(
    state: &Rc<RefCell<OverlayState>>,
    window: &gtk::ApplicationWindow,
    capturing_pill: &gtk::Box,
) {
    if state.borrow().auto_scroll_stop.is_some() {
        return;
    }
    let scale = capturing_pill.scale_factor().max(1);
    let pill_top_logical = capturing_pill.margin_top();
    let target_y_logical = (pill_top_logical - 16).max(0);
    let sel = state.borrow().selection;
    let cursor_x = ((sel.x + sel.w / 2.0) as i32) * scale;
    let cursor_y = target_y_logical * scale;

    let stop = Arc::new(AtomicBool::new(false));
    if let Err(e) = auto_scroll::spawn_worker(Arc::clone(&stop), cursor_x, cursor_y) {
        eprintln!("scroll-capture: auto-scroll failed to start: {e}");
        return;
    }
    let _ = window;
    let baseline = state.borrow().frames.len();
    {
        let mut s = state.borrow_mut();
        s.auto_scroll_stop = Some(stop);
        s.auto_scroll_baseline_frames = baseline;
        s.auto_scroll_quiet_ticks = 0;
    }

    let monitor = {
        let state = Rc::clone(state);
        let pill = capturing_pill.clone();
        let window = window.clone();
        glib::timeout_add_local(Duration::from_millis(500), move || {
            let mut s = state.borrow_mut();
            let Some(stop) = s.auto_scroll_stop.clone() else {
                return glib::ControlFlow::Break;
            };
            let cur = s.frames.len();
            if cur > s.auto_scroll_baseline_frames {
                s.auto_scroll_quiet_ticks = 0;
                s.auto_scroll_baseline_frames = cur;
                return glib::ControlFlow::Continue;
            }
            s.auto_scroll_quiet_ticks += 1;
            // 3 monitor ticks × 500ms = 1.5s of no new frames retained.
            if s.auto_scroll_quiet_ticks < 3 {
                return glib::ControlFlow::Continue;
            }
            stop.store(true, Ordering::Relaxed);
            s.auto_scroll_stop = None;
            s.auto_scroll_monitor = None;
            drop(s);
            let _ = window;
            end_of_content_ui(&pill);
            glib::ControlFlow::Break
        })
    };
    state.borrow_mut().auto_scroll_monitor = Some(monitor);
}

fn end_of_content_ui(capturing_pill: &gtk::Box) {
    let mut child = capturing_pill.first_child();
    let mut idx = 0;
    while let Some(c) = child {
        let next = c.next_sibling();
        match idx {
            1 => c.set_visible(false), // Auto-Scroll button hidden
            2 => c.add_css_class("scroll-capture-done-highlight"), // Done emphasized
            _ => {}
        }
        idx += 1;
        child = next;
    }
}


fn build_prompt_pill() -> gtk::Box {
    let pill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    pill.add_css_class("scroll-capture-pill");
    pill.add_css_class("scroll-capture-prompt");
    let label = gtk::Label::new(Some("Drag to capture the scrolling part of the screen."));
    label.add_css_class("scroll-capture-prompt-label");
    pill.append(&label);
    pill
}

fn build_action_pill() -> gtk::Box {
    let pill = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    pill.add_css_class("scroll-capture-pill");
    pill.add_css_class("scroll-capture-actions");

    let cancel = gtk::Button::with_label("\u{2715}  Cancel");
    cancel.add_css_class("scroll-capture-button");
    cancel.add_css_class("scroll-capture-cancel");
    pill.append(&cancel);

    let start = gtk::Button::with_label("\u{2192}  Start Capture");
    start.add_css_class("scroll-capture-button");
    start.add_css_class("scroll-capture-primary");
    pill.append(&start);

    pill
}

fn build_capturing_pill() -> gtk::Box {
    let pill = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    pill.add_css_class("scroll-capture-pill");
    pill.add_css_class("scroll-capture-actions");

    let cancel = gtk::Button::with_label("\u{2715}  Cancel");
    cancel.add_css_class("scroll-capture-button");
    cancel.add_css_class("scroll-capture-cancel");
    pill.append(&cancel);

    let auto_scroll = gtk::Button::with_label("\u{25B6}  Auto-Scroll");
    auto_scroll.add_css_class("scroll-capture-button");
    auto_scroll.add_css_class("scroll-capture-auto");
    pill.append(&auto_scroll);

    let done = gtk::Button::with_label("\u{2713}  Done");
    done.add_css_class("scroll-capture-button");
    done.add_css_class("scroll-capture-primary");
    pill.append(&done);

    pill
}

fn pill_natural_size(pill: &gtk::Box) -> (f64, f64) {
    let (_, w_nat, _, _) = pill.measure(gtk::Orientation::Horizontal, -1);
    let (_, h_nat, _, _) = pill.measure(gtk::Orientation::Vertical, w_nat);
    (w_nat as f64, h_nat as f64)
}

fn position_action_pill_and_input(
    window: &gtk::ApplicationWindow,
    overlay: &gtk::Overlay,
    pill: &gtk::Box,
    sel: Selection,
) {
    let window = window.clone();
    let overlay = overlay.clone();
    let pill = pill.clone();
    glib::idle_add_local_once(move || {
        let (pw, ph) = measured_pill_size(&pill);
        let x = sel.x + (sel.w - pw) / 2.0;
        let y = (sel.y + sel.h + PILL_GAP)
            .min(overlay.allocated_height() as f64 - ph - 8.0)
            .max(8.0);
        let x = x.max(8.0);
        pill.set_margin_start(x as i32);
        pill.set_margin_top(y as i32);
        set_pill_input_region(&window, x, y, pw, ph);
    });
}

fn set_pill_input_region(window: &gtk::ApplicationWindow, x: f64, y: f64, w: f64, h: f64) {
    if let Some(surface) = window.surface() {
        let pad: i32 = 6;
        let rect = cairo::RectangleInt::new(
            (x as i32) - pad,
            (y as i32) - pad,
            (w as i32) + 2 * pad,
            (h as i32) + 2 * pad,
        );
        let region = cairo::Region::create_rectangle(&rect);
        surface.set_input_region(&region);
        eprintln!(
            "scroll-capture: input region set to ({},{}) {}x{}",
            rect.x(), rect.y(), rect.width(), rect.height()
        );
    }
}

fn position_capturing_pill_and_input(
    window: &gtk::ApplicationWindow,
    overlay: &gtk::Overlay,
    pill: &gtk::Box,
    sel: Selection,
) {
    let window = window.clone();
    let overlay = overlay.clone();
    let pill = pill.clone();
    glib::idle_add_local_once(move || {
        let (pw, ph) = measured_pill_size(&pill);
        let x = sel.x + (sel.w - pw) / 2.0;
        // Inside the selection, bottom-centered, so clicking Auto-Scroll parks
        // the cursor inside the scrollable region for virtual-pointer events.
        let inside_y = sel.y + sel.h - ph - PILL_GAP;
        let y = if inside_y < sel.y + 8.0 {
            sel.y + sel.h + PILL_GAP
        } else {
            inside_y
        };
        let y = y
            .min(overlay.allocated_height() as f64 - ph - 8.0)
            .max(8.0);
        let x = x.max(8.0);
        pill.set_margin_start(x as i32);
        pill.set_margin_top(y as i32);
        set_pill_input_region(&window, x, y, pw, ph);
    });
}

fn measured_pill_size(pill: &gtk::Box) -> (f64, f64) {
    // Prefer the actual allocation when layout has run; fall back to the
    // measure() natural size if not yet allocated.
    let aw = pill.allocated_width();
    let ah = pill.allocated_height();
    if aw > 1 && ah > 1 {
        return (aw as f64, ah as f64);
    }
    let (_, w_nat, _, _) = pill.measure(gtk::Orientation::Horizontal, -1);
    let (_, h_nat, _, _) = pill.measure(gtk::Orientation::Vertical, w_nat);
    (w_nat as f64, h_nat as f64)
}

fn stripe_hash(pixbuf: &Pixbuf) -> u64 {
    let h = pixbuf.height();
    let w = pixbuf.width();
    let rowstride = pixbuf.rowstride() as usize;
    let pixels = unsafe { pixbuf.pixels() };
    let mid = (h / 2).max(0);
    let band_top = (mid - STRIPE_ROWS / 2).max(0) as usize;
    let band_bot = (mid + STRIPE_ROWS / 2).min(h - 1).max(0) as usize;
    let bytes_per_row = (w as usize) * 4;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for y in band_top..=band_bot {
        let start = y * rowstride;
        let end = start + bytes_per_row;
        if end <= pixels.len() {
            hasher.write(&pixels[start..end]);
        }
    }
    hasher.finish()
}

fn draw_backdrop(cr: &cairo::Context, w: f64, h: f64, s: &OverlayState) {
    let _ = cr.save();
    cr.set_operator(cairo::Operator::Source);

    cr.set_source_rgba(0.0, 0.0, 0.0, BACKDROP_ALPHA);
    cr.rectangle(0.0, 0.0, w, h);
    let _ = cr.fill();

    let active_rect = match s.phase {
        Phase::Dragging | Phase::Selected | Phase::Capturing => Some(s.selection),
        Phase::AwaitingDrag => None,
    };

    if let Some(sel) = active_rect {
        // Punch the selection clear so the underlying screen shows through.
        cr.set_operator(cairo::Operator::Clear);
        cr.rectangle(sel.x, sel.y, sel.w, sel.h);
        let _ = cr.fill();

        // Brackets in the OVER op so they composite on top of nothing.
        cr.set_operator(cairo::Operator::Over);
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
        cr.set_line_width(BRACKET_WIDTH);
        draw_corner_brackets(cr, sel);
    }
    let _ = cr.restore();
}

fn draw_corner_brackets(cr: &cairo::Context, sel: Selection) {
    let l = BRACKET_LEN;
    let half = BRACKET_WIDTH / 2.0;
    let x0 = sel.x;
    let y0 = sel.y;
    let x1 = sel.x + sel.w;
    let y1 = sel.y + sel.h;

    // top-left
    cr.move_to(x0 - half, y0 + l);
    cr.line_to(x0 - half, y0 - half);
    cr.line_to(x0 + l, y0 - half);
    // top-right
    cr.move_to(x1 - l, y0 - half);
    cr.line_to(x1 + half, y0 - half);
    cr.line_to(x1 + half, y0 + l);
    // bottom-right
    cr.move_to(x1 + half, y1 - l);
    cr.line_to(x1 + half, y1 + half);
    cr.line_to(x1 - l, y1 + half);
    // bottom-left
    cr.move_to(x0 + l, y1 + half);
    cr.line_to(x0 - half, y1 + half);
    cr.line_to(x0 - half, y1 - l);

    let _ = cr.stroke();
}

fn install_css(_app: &gtk::Application) {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(include_str!("style.css"));
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
