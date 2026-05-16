use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
mod stitch;

const BACKDROP_ALPHA: f64 = 0.55;
const BRACKET_LEN: f64 = 22.0;
const BRACKET_WIDTH: f64 = 3.0;
const PILL_GAP: f64 = 18.0;
const MIN_SELECTION: f64 = 8.0;
const CAPTURE_INTERVAL_MS: u64 = 100;
/// Expected per-cycle scroll in device pixels: ARROWS_PER_TICK (=5) *
/// ARROW_SCROLL_LOGICAL_PX (~40) * HiDPI scale (~2). Used as the second
/// reference offset in the capture-time dedup check (SAD@0 vs SAD@hint).
/// Tightness isn't important — it just has to be in the right ballpark
/// for "real scroll" so that SAD@hint is clearly lower than SAD@0 after
/// a normal scroll cycle.
const SCROLL_DELTA_DEVICE_PX_HINT: usize = 400;
const DRAG_THRESHOLD: f64 = 4.0;

/// Length of the L-shaped corner brackets (logical pixels). Matches
/// satty crop tool's BRACKET_LENGTH.
const CROP_BRACKET_LENGTH: f64 = 28.0;

/// Length of the parallel "fat bar" edge handle (logical pixels). Matches
/// satty crop tool's EDGE_HANDLE_LENGTH.
const EDGE_HANDLE_LENGTH: f64 = 36.0;

/// Stroke width for corner brackets and edge bars (logical pixels).
/// Matches satty crop tool's HANDLE_STROKE_WIDTH.
const CROP_STROKE_WIDTH: f64 = 5.0;

/// Radius of the central Move handle.
const MOVE_HANDLE_RADIUS: f64 = 18.0;

/// Minimum size the selection can be resized to. Prevents the rect from
/// flipping inside-out during a drag.
const MIN_SELECTION_SIZE: f64 = 32.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResizeHandle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
    /// Drag from the center of the selection to move the whole rect
    /// without resizing.
    Move,
}

impl ResizeHandle {
    /// Center of this handle for a given selection. Resize handles sit ON
    /// the selection edges (matching the crop tool); Move sits at the
    /// selection's center.
    fn center(self, sel: Selection) -> (f64, f64) {
        match self {
            ResizeHandle::TopLeft => (sel.x, sel.y),
            ResizeHandle::Top => (sel.x + sel.w / 2.0, sel.y),
            ResizeHandle::TopRight => (sel.x + sel.w, sel.y),
            ResizeHandle::Right => (sel.x + sel.w, sel.y + sel.h / 2.0),
            ResizeHandle::BottomRight => (sel.x + sel.w, sel.y + sel.h),
            ResizeHandle::Bottom => (sel.x + sel.w / 2.0, sel.y + sel.h),
            ResizeHandle::BottomLeft => (sel.x, sel.y + sel.h),
            ResizeHandle::Left => (sel.x, sel.y + sel.h / 2.0),
            ResizeHandle::Move => (sel.x + sel.w / 2.0, sel.y + sel.h / 2.0),
        }
    }

    /// Compute the new selection when this handle has been dragged. For
    /// resize handles, `mouse_x/y` is the new position of the dragged edge
    /// or corner. For Move, the entire rect is translated by the delta
    /// between `drag_origin` and `(mouse_x, mouse_y)`.
    fn apply(
        self,
        anchor: Selection,
        drag_origin: (f64, f64),
        mouse_x: f64,
        mouse_y: f64,
    ) -> Selection {
        if matches!(self, ResizeHandle::Move) {
            let dx = mouse_x - drag_origin.0;
            let dy = mouse_y - drag_origin.1;
            return Selection {
                x: anchor.x + dx,
                y: anchor.y + dy,
                w: anchor.w,
                h: anchor.h,
            };
        }
        let right = anchor.x + anchor.w;
        let bottom = anchor.y + anchor.h;
        let (x1, y1, x2, y2) = match self {
            ResizeHandle::TopLeft => (mouse_x, mouse_y, right, bottom),
            ResizeHandle::Top => (anchor.x, mouse_y, right, bottom),
            ResizeHandle::TopRight => (anchor.x, mouse_y, mouse_x, bottom),
            ResizeHandle::Right => (anchor.x, anchor.y, mouse_x, bottom),
            ResizeHandle::BottomRight => (anchor.x, anchor.y, mouse_x, mouse_y),
            ResizeHandle::Bottom => (anchor.x, anchor.y, right, mouse_y),
            ResizeHandle::BottomLeft => (mouse_x, anchor.y, right, mouse_y),
            ResizeHandle::Left => (mouse_x, anchor.y, right, bottom),
            ResizeHandle::Move => unreachable!(),
        };
        let lx = x1.min(x2);
        let ly = y1.min(y2);
        let w = (x2 - x1).abs().max(MIN_SELECTION_SIZE);
        let h = (y2 - y1).abs().max(MIN_SELECTION_SIZE);
        Selection { x: lx, y: ly, w, h }
    }
}

/// Half-thickness of the resize hit band around each edge of the
/// selection. Anywhere within this distance of an edge counts as
/// grabbing that edge.
const EDGE_HIT_SLACK: f64 = 12.0;

/// Distance from a corner anchor within which the hit prefers the
/// corner (diagonal resize) over an adjacent edge. Larger than
/// EDGE_HIT_SLACK so corners are easy to grab.
const CORNER_HIT_RADIUS: f64 = 20.0;

fn hit_test_handle(sel: Selection, x: f64, y: f64) -> Option<ResizeHandle> {
    // 1) Corners win if you're near one (so you get diagonal resize even
    // though the edge bands overlap there).
    for h in [
        ResizeHandle::TopLeft,
        ResizeHandle::TopRight,
        ResizeHandle::BottomRight,
        ResizeHandle::BottomLeft,
    ] {
        let (cx, cy) = h.center(sel);
        let r = CORNER_HIT_RADIUS;
        if (x - cx).powi(2) + (y - cy).powi(2) <= r * r {
            return Some(h);
        }
    }

    // 2) Move handle in the center (only inside the selection rect to
    // avoid overlapping the edge hit zones when the selection is small).
    let (mcx, mcy) = ResizeHandle::Move.center(sel);
    let mr = MOVE_HANDLE_RADIUS + 3.0;
    if (x - mcx).powi(2) + (y - mcy).powi(2) <= mr * mr {
        return Some(ResizeHandle::Move);
    }

    // 3) Edges: anywhere along an edge (between corners) within
    // EDGE_HIT_SLACK perpendicular distance grabs that edge.
    let within_x = x >= sel.x - EDGE_HIT_SLACK && x <= sel.x + sel.w + EDGE_HIT_SLACK;
    let within_y = y >= sel.y - EDGE_HIT_SLACK && y <= sel.y + sel.h + EDGE_HIT_SLACK;
    if within_x && (y - sel.y).abs() <= EDGE_HIT_SLACK {
        return Some(ResizeHandle::Top);
    }
    if within_x && (y - (sel.y + sel.h)).abs() <= EDGE_HIT_SLACK {
        return Some(ResizeHandle::Bottom);
    }
    if within_y && (x - sel.x).abs() <= EDGE_HIT_SLACK {
        return Some(ResizeHandle::Left);
    }
    if within_y && (x - (sel.x + sel.w)).abs() <= EDGE_HIT_SLACK {
        return Some(ResizeHandle::Right);
    }
    None
}

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
    /// Downsampled-grayscale view used by the dedup SAD check (and
    /// reusable by the stitcher to avoid downsampling twice).
    gray: stitch::GrayView,
}

struct OverlayState {
    phase: Phase,
    drag_origin: (f64, f64),
    drag_active: bool,
    selection: Selection,
    resize_handle: Option<ResizeHandle>,
    resize_anchor: Selection,
    frames: Vec<CapturedFrame>,
    capture_timer: Option<glib::SourceId>,
    auto_scroll_stop: Option<Arc<AtomicBool>>,
    auto_scroll_baseline_frames: usize,
    auto_scroll_quiet_ticks: u32,
    auto_scroll_monitor: Option<glib::SourceId>,
    /// True while a worker is actively scrolling — used to hide the
    /// inside-selection Auto-Scroll buttons and drop them from the input
    /// region so they're not in captured frames.
    auto_scroll_active: bool,
    /// Counter incremented by the worker after each completed keypress
    /// cycle (arrows sent + sleep for paint). While `auto_scroll_active`,
    /// `capture_tick` pushes exactly one frame per increment, so the
    /// stitcher can trust a fixed scroll delta per frame.
    auto_scroll_cycle_counter: Arc<AtomicU64>,
    /// Last cycle index for which we've already pushed a frame. Initialised
    /// to whatever counter is at the start of each Auto-Scroll click.
    last_captured_cycle: u64,
    /// Number of consecutive cycles where capture_tick rejected the frame
    /// as a duplicate (page didn't actually scroll). Used to trigger an
    /// early end-of-content stop without waiting for the slower 1.5s
    /// monitor heuristic.
    consecutive_no_scroll: u32,
}

/// Run the scroll-capture overlay. Returns `Ok(Some(pixbuf))` when the user
/// completes the capture (Done) and `Ok(None)` on Cancel/Esc. The pixbuf is
/// the stitched result of all captured frames, ready to feed into the
/// annotation canvas.
pub fn run() -> Result<Option<Pixbuf>> {
    let result: Rc<RefCell<Option<Pixbuf>>> = Rc::new(RefCell::new(None));

    let app = gtk::Application::builder()
        .application_id("dev.tensaku.Tensaku.scroll-capture")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();

    {
        let result = Rc::clone(&result);
        app.connect_activate(move |app| build_overlay(app, &result));
    }

    let exit_code = app.run_with_args::<&str>(&[]);
    if exit_code != gtk::glib::ExitCode::SUCCESS {
        return Err(anyhow!(
            "scroll-capture overlay exited with code {:?}",
            exit_code
        ));
    }
    Ok(result.borrow_mut().take())
}

fn build_overlay(app: &gtk::Application, result: &Rc<RefCell<Option<Pixbuf>>>) {
    let state = Rc::new(RefCell::new(OverlayState {
        phase: Phase::AwaitingDrag,
        drag_origin: (0.0, 0.0),
        drag_active: false,
        selection: Selection::default(),
        resize_handle: None,
        resize_anchor: Selection::default(),
        frames: Vec::new(),
        capture_timer: None,
        auto_scroll_stop: None,
        auto_scroll_baseline_frames: 0,
        auto_scroll_quiet_ticks: 0,
        auto_scroll_monitor: None,
        auto_scroll_active: false,
        auto_scroll_cycle_counter: Arc::new(AtomicU64::new(0)),
        last_captured_cycle: 0,
        consecutive_no_scroll: 0,
    }));

    let window = gtk::ApplicationWindow::new(app);
    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_keyboard_mode(KeyboardMode::Exclusive);
    window.set_namespace(Some("tensaku-scroll-capture"));
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

    // Auto-Scroll buttons positioned INSIDE the selection during Capturing.
    // Hidden until the user starts the capture; hidden again while a
    // worker is actively scrolling so they don't appear in captured frames.
    let vert_auto_scroll = build_inside_vert_auto_scroll();
    let horiz_auto_scroll = build_inside_horiz_auto_scroll();
    for btn in [&vert_auto_scroll, &horiz_auto_scroll] {
        btn.set_halign(gtk::Align::Start);
        btn.set_valign(gtk::Align::Start);
        btn.set_visible(false);
        overlay.add_overlay(btn);
    }

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
            // If we're past the initial drag and the cursor landed on a
            // resize handle, remember which handle so drag_update resizes
            // instead of starting a new selection.
            // Handles are interactive only in Selected. Once Capturing
            // starts, the selection is locked in (handles are hidden too
            // — see draw_backdrop).
            s.resize_handle = match s.phase {
                Phase::Selected => hit_test_handle(s.selection, x, y),
                _ => None,
            };
            if s.resize_handle.is_some() {
                s.resize_anchor = s.selection;
            }
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
                if s.resize_handle.is_none() {
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
            }
            // Resizing or moving an existing selection via a handle.
            if let Some(handle) = s.resize_handle {
                let drag_origin = s.drag_origin;
                let new_sel = handle.apply(
                    s.resize_anchor,
                    drag_origin,
                    drag_origin.0 + dx,
                    drag_origin.1 + dy,
                );
                s.selection = new_sel;
                drop(s);
                drawing_w.queue_draw();
                return;
            }
            // Otherwise, growing a fresh selection.
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
        let capturing_pill_w = capturing_pill.clone();
        let vert_btn_w = vert_auto_scroll.clone();
        let horiz_btn_w = horiz_auto_scroll.clone();
        let prompt_w = prompt.clone();
        drag.connect_drag_end(move |_, _dx, _dy| {
            let mut s = state.borrow_mut();
            if !s.drag_active {
                // Tap that missed a button or handle — leave state alone.
                s.resize_handle = None;
                return;
            }
            s.drag_active = false;
            // Finishing a resize: keep phase, just refresh pill + input
            // region against the new selection rect.
            if s.resize_handle.is_some() {
                s.resize_handle = None;
                let sel = s.selection;
                let phase = s.phase;
                let auto_scroll_active = s.auto_scroll_active;
                drop(s);
                drawing_w.queue_draw();
                match phase {
                    Phase::Selected => {
                        position_action_pill_and_input(
                            &window_w, &overlay_w, &action_pill_w, sel,
                        );
                    }
                    Phase::Capturing => {
                        position_capturing_pill_and_input(
                            &window_w,
                            &overlay_w,
                            &capturing_pill_w,
                            &vert_btn_w,
                            &horiz_btn_w,
                            sel,
                            auto_scroll_active,
                        );
                    }
                    _ => {}
                }
                return;
            }
            if s.selection.is_valid() {
                s.phase = Phase::Selected;
                let sel = s.selection;
                drop(s);
                action_pill_w.set_visible(true);
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

    // Cursor shape on handle hover — only in Selected (handles are hidden
    // during Capturing).
    let motion = gtk::EventControllerMotion::new();
    {
        let state = Rc::clone(&state);
        let drawing_w = drawing.clone();
        motion.connect_motion(move |_, x, y| {
            let phase = state.borrow().phase;
            if !matches!(phase, Phase::Selected) {
                drawing_w.set_cursor_from_name(Some("default"));
                return;
            }
            let sel = state.borrow().selection;
            let name = match hit_test_handle(sel, x, y) {
                Some(ResizeHandle::TopLeft) | Some(ResizeHandle::BottomRight) => "nwse-resize",
                Some(ResizeHandle::TopRight) | Some(ResizeHandle::BottomLeft) => "nesw-resize",
                Some(ResizeHandle::Top) | Some(ResizeHandle::Bottom) => "ns-resize",
                Some(ResizeHandle::Left) | Some(ResizeHandle::Right) => "ew-resize",
                Some(ResizeHandle::Move) => "move",
                None => "default",
            };
            drawing_w.set_cursor_from_name(Some(name));
        });
    }
    drawing.add_controller(motion);

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
        let vert_btn_w = vert_auto_scroll.clone();
        let horiz_btn_w = horiz_auto_scroll.clone();
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
                &vert_btn_w,
                &horiz_btn_w,
                &drawing_w,
            );
        });
    }

    // Wire capturing-pill buttons (Cancel / Auto-Scroll / Done).
    wire_capturing_pill(&state, &window, &capturing_pill, result);

    // Inside-selection Auto-Scroll buttons wire to start_auto_scroll_at.
    {
        let state_w = Rc::clone(&state);
        let window_w = window.clone();
        let overlay_w = overlay.clone();
        let capturing_pill_w = capturing_pill.clone();
        let vert_btn_w = vert_auto_scroll.clone();
        let horiz_btn_w = horiz_auto_scroll.clone();
        let btn = vert_auto_scroll.clone();
        btn.connect_clicked(move |b| {
            eprintln!("scroll-capture: vertical Auto-Scroll clicked");
            start_auto_scroll_at(
                &state_w,
                &window_w,
                &overlay_w,
                &capturing_pill_w,
                &vert_btn_w,
                &horiz_btn_w,
                b,
                auto_scroll::ScrollDirection::Down,
            );
        });
    }
    {
        let state_w = Rc::clone(&state);
        let window_w = window.clone();
        let overlay_w = overlay.clone();
        let capturing_pill_w = capturing_pill.clone();
        let vert_btn_w = vert_auto_scroll.clone();
        let horiz_btn_w = horiz_auto_scroll.clone();
        let btn = horiz_auto_scroll.clone();
        btn.connect_clicked(move |b| {
            eprintln!("scroll-capture: horizontal Auto-Scroll clicked");
            start_auto_scroll_at(
                &state_w,
                &window_w,
                &overlay_w,
                &capturing_pill_w,
                &vert_btn_w,
                &horiz_btn_w,
                b,
                auto_scroll::ScrollDirection::Right,
            );
        });
    }

    window.present();
}

fn start_capture(
    state: &Rc<RefCell<OverlayState>>,
    window: &gtk::ApplicationWindow,
    overlay: &gtk::Overlay,
    action_pill: &gtk::Box,
    capturing_pill: &gtk::Box,
    vert_btn: &gtk::Button,
    horiz_btn: &gtk::Button,
    drawing: &gtk::DrawingArea,
) {
    let sel = state.borrow().selection;
    {
        let mut s = state.borrow_mut();
        s.phase = Phase::Capturing;
        s.auto_scroll_active = false;
    }

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
    position_capturing_pill_and_input(
        window,
        overlay,
        capturing_pill,
        vert_btn,
        horiz_btn,
        sel,
        false,
    );
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
    // Capture ONLY while Auto-Scroll is active AND the worker has reported
    // a new completed scroll cycle since the last frame we captured.
    //
    // Frames are otherwise unsafe to stitch:
    // - Pre-click frames (auto-scroll buttons fully visible) would bake
    //   the buttons into the output.
    // - Post-end-of-content frames (buttons restored so the user can run
    //   another pass or click Done) would do the same.
    // - Mid-cycle frames (page repainting in the middle of an arrow burst)
    //   would land at uneven scroll positions and the fixed-delta
    //   stitcher would produce overlap / duplicate content.
    //
    // The worker increments `auto_scroll_cycle_counter` after each
    // keypress burst + paint-settle, so reading it tells us when a fresh
    // post-scroll frame is ready. This is the user's "capture only new
    // pixels" model — one frame per scroll, no overlap math.
    {
        let s = state.borrow();
        if !s.auto_scroll_active {
            return glib::ControlFlow::Continue;
        }
        let cur = s.auto_scroll_cycle_counter.load(Ordering::Relaxed);
        if cur <= s.last_captured_cycle {
            return glib::ControlFlow::Continue;
        }
    }
    let rect = capture::Rect {
        x: sel.x.round() as i32,
        y: sel.y.round() as i32,
        width: sel.w.round() as i32,
        height: sel.h.round() as i32,
    };
    match capture::capture_region(rect) {
        Ok(pixbuf) => {
            // Downsample to grayscale once. Used both for the dedup SAD
            // check below and (cached on the CapturedFrame) by the
            // stitcher later — saves a redundant downsample pass.
            let gray = stitch::downsample_to_gray(&pixbuf);
            let mut s = state.borrow_mut();
            // Two-point SAD test: at offset 0 the bands compare prev's
            // and cur's rows in place (no scroll); at offset
            // SCROLL_DELTA_DEVICE they compare what cur shows now vs
            // what prev showed one cycle earlier. Whichever is smaller
            // tells us which hypothesis fits the data better — "frames
            // are the same view" or "frames are one scroll cycle apart".
            // No fixed-threshold tuning needed; the answer is whatever
            // SAD itself prefers.
            let is_dup = match s.frames.last() {
                Some(prev) => {
                    let sad_zero = stitch::sad_at_offset(&prev.gray, &gray, 0);
                    let sad_full = stitch::sad_at_offset(
                        &prev.gray, &gray, SCROLL_DELTA_DEVICE_PX_HINT,
                    );
                    sad_zero <= sad_full
                }
                None => false,
            };
            if !is_dup {
                s.frames.push(CapturedFrame { pixbuf, gray });
                eprintln!("scroll-capture: kept frame {}", s.frames.len());
                s.consecutive_no_scroll = 0;
            } else if s.auto_scroll_active {
                s.consecutive_no_scroll += 1;
                eprintln!(
                    "scroll-capture: no-scroll cycle ({} consecutive)",
                    s.consecutive_no_scroll
                );
                // Two consecutive no-scroll cycles → page won't scroll
                // further. Signal the worker to stop sending arrow keys.
                // We deliberately don't take() the AtomicBool out of
                // state here: the monitor's normal exit path is the
                // single owner of the source-removal cleanup; from this
                // call site we only signal, and the monitor picks it up
                // on its next tick. (Earlier version took() the stop
                // here, then panicked at Done click trying to remove an
                // already-removed glib SourceId.)
                if s.consecutive_no_scroll >= 2
                    && let Some(stop) = &s.auto_scroll_stop
                {
                    stop.store(true, Ordering::Relaxed);
                    eprintln!(
                        "scroll-capture: end-of-content (2 no-scroll cycles), signalled worker"
                    );
                }
            }
            if s.auto_scroll_active {
                let cur = s.auto_scroll_cycle_counter.load(Ordering::Relaxed);
                s.last_captured_cycle = cur;
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
    result: &Rc<RefCell<Option<Pixbuf>>>,
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
                        stop_capture_with_window(&state, &window_w);
                        window_w.close();
                    });
                }
                1 => {
                    // Done — stop the timer, stitch captured frames into a
                    // single tall Pixbuf, store the result, close. main.rs
                    // picks up the result and opens the annotation canvas.
                    let window_w = window.clone();
                    let state = Rc::clone(state);
                    let result = Rc::clone(result);
                    let pill_w = pill.clone();
                    button.connect_clicked(move |_| {
                        stop_capture_with_window(&state, &window_w);
                        let frames: Vec<Pixbuf> = state
                            .borrow()
                            .frames
                            .iter()
                            .map(|f| f.pixbuf.clone())
                            .collect();
                        // Expected per-tick scroll: ARROWS_PER_TICK arrows ×
                        // ~40 logical px per arrow × HiDPI scale = px in the
                        // captured-frame coordinate space.
                        let scale = pill_w.scale_factor().max(1) as u32;
                        let expected_delta = (auto_scroll::ARROWS_PER_TICK
                            * auto_scroll::ARROW_SCROLL_LOGICAL_PX
                            * scale) as usize;
                        eprintln!(
                            "scroll-capture: Done — stitching {} frame(s) (expected delta {} px)...",
                            frames.len(), expected_delta
                        );
                        let t0 = std::time::Instant::now();
                        match stitch::stitch(&frames, expected_delta) {
                            Ok(pixbuf) => {
                                eprintln!(
                                    "scroll-capture: stitched output {}x{} in {:?}",
                                    pixbuf.width(), pixbuf.height(), t0.elapsed()
                                );
                                *result.borrow_mut() = Some(pixbuf);
                            }
                            Err(e) => {
                                eprintln!("scroll-capture: stitch failed: {e}");
                            }
                        }
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

fn stop_capture_with_window(state: &Rc<RefCell<OverlayState>>, window: &gtk::ApplicationWindow) {
    // Restore keyboard mode in case Cancel/Done is pressed mid-auto-scroll.
    if state.borrow().auto_scroll_active {
        window.set_keyboard_mode(KeyboardMode::OnDemand);
    }
    stop_capture(state);
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
    let mut s = state.borrow_mut();
    s.phase = Phase::Selected;
    s.auto_scroll_active = false;
}

/// Click handler for the inside-selection Auto-Scroll buttons. The user's
/// cursor is already on the clicked button (inside the selection rect), so
/// there's no `motion_absolute` jump like the old outside-pill path: we
/// just hide both buttons, drop them from the input region (which lets the
/// underlying surface receive events there), and after the next idle
/// synthesise a middle-click + start the keypress loop via the worker.
#[allow(clippy::too_many_arguments)]
fn start_auto_scroll_at(
    state: &Rc<RefCell<OverlayState>>,
    window: &gtk::ApplicationWindow,
    overlay: &gtk::Overlay,
    capturing_pill: &gtk::Box,
    vert_btn: &gtk::Button,
    horiz_btn: &gtk::Button,
    clicked_btn: &gtk::Button,
    direction: auto_scroll::ScrollDirection,
) {
    if state.borrow().auto_scroll_stop.is_some() {
        return;
    }

    // Park the virtual pointer at the right-edge mid-height of the
    // selection (in browser content this is the scrollbar gutter — a
    // non-interactive region with no hover-driven repaints). The
    // previous behaviour parked the cursor at the clicked button's
    // centre, which after the buttons hid landed on whatever underlying
    // text/link/card was there and triggered hover effects mid-scroll
    // — those animations confused SAD on a few pairs (low confidence,
    // wrong delta). Right-edge mid-height is INSIDE the selection (so
    // still in the pass-through region) but typically empty.
    let scale = clicked_btn.scale_factor().max(1);
    let sel_now = state.borrow().selection;
    // Park on the right side, ~30 logical px from the right edge, AND
    // near the bottom of the selection (~60 px above the bottom edge).
    // - Right side: typically the content gutter, so most of the time
    //   the cursor sits over whitespace rather than centred text.
    // - Lower in the capture zone: a Chrome link-hover URL preview
    //   overlay appears at the BOTTOM of the viewport, so if a link
    //   does roll under the cursor and Chrome fires the preview, the
    //   preview will mostly land BELOW the cursor's row. Captured
    //   frames are taller above the cursor than below, so the overlay
    //   intersects fewer appended rows in the stitched output. (10 px
    //   on the scrollbar itself broke keyboard focus on Chrome.)
    let park_x_logical = (sel_now.x + sel_now.w - 30.0).max(sel_now.x + 1.0);
    let park_y_logical = (sel_now.y + sel_now.h - 60.0).max(sel_now.y + 1.0);
    let cursor_x = (park_x_logical as i32) * scale;
    let cursor_y = (park_y_logical as i32) * scale;
    let _ = clicked_btn;

    // Hide both buttons IMMEDIATELY (synchronously) so the next 100 ms
    // capture_tick can't snapshot the screen with them still rendered.
    // The deferred idle handler in `position_capturing_pill_and_input`
    // then commits the input-region update without re-showing them
    // (auto_scroll_active=true).
    vert_btn.set_visible(false);
    horiz_btn.set_visible(false);
    state.borrow_mut().auto_scroll_active = true;
    let sel = state.borrow().selection;
    position_capturing_pill_and_input(
        window,
        overlay,
        capturing_pill,
        vert_btn,
        horiz_btn,
        sel,
        true,
    );

    // Release keyboard focus from our layer-shell surface so arrow keys
    // sent by the virtual keyboard are routed to whichever surface has
    // pointer focus (i.e. the underlying app after the virtual-pointer
    // motion below). Without this, our overlay still owns keyboard focus
    // because the user just clicked one of our buttons — and Hyprland
    // doesn't transfer keyboard focus on virtual-pointer motion alone.
    // Symptom we hit before this fix: first auto-scroll click is a no-op
    // (keys go to our overlay), subsequent clicks work only after the
    // user jiggles the real mouse, which actually moves pointer+keyboard
    // focus to the underlying surface. We restore OnDemand when the
    // worker exits (see monitor + stop_capture).
    window.set_keyboard_mode(KeyboardMode::None);

    let state_w = Rc::clone(state);
    let window_w = window.clone();
    let overlay_w = overlay.clone();
    let pill_w = capturing_pill.clone();
    let vert_btn_w = vert_btn.clone();
    let horiz_btn_w = horiz_btn.clone();
    glib::idle_add_local_once(move || {
        let stop = Arc::new(AtomicBool::new(false));
        // Sync the cycle counter to "0 = no cycles yet" for this run, so
        // capture_tick only pushes a frame once the worker reports its
        // first completed keypress cycle.
        let cycle_counter = Arc::new(AtomicU64::new(0));
        if let Err(e) = auto_scroll::spawn_worker(
            Arc::clone(&stop),
            Arc::clone(&cycle_counter),
            cursor_x,
            cursor_y,
            direction,
        ) {
            eprintln!("scroll-capture: auto-scroll failed to start: {e}");
            // Roll back the active flag and restore buttons.
            state_w.borrow_mut().auto_scroll_active = false;
            position_capturing_pill_and_input(
                &window_w, &overlay_w, &pill_w, &vert_btn_w, &horiz_btn_w, sel, false,
            );
            return;
        }
        let baseline = state_w.borrow().frames.len();
        {
            let mut s = state_w.borrow_mut();
            s.auto_scroll_stop = Some(stop);
            s.auto_scroll_baseline_frames = baseline;
            s.auto_scroll_quiet_ticks = 0;
            s.auto_scroll_cycle_counter = cycle_counter;
            s.last_captured_cycle = 0;
            s.consecutive_no_scroll = 0;
        }

        let monitor = {
            let state = Rc::clone(&state_w);
            let pill = pill_w.clone();
            let window = window_w.clone();
            let overlay = overlay_w.clone();
            let vert_btn = vert_btn_w.clone();
            let horiz_btn = horiz_btn_w.clone();
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
                s.auto_scroll_active = false;
                let sel = s.selection;
                drop(s);
                // Restore keyboard focus on our layer-shell surface so
                // Cancel/Done buttons + Esc work again.
                window.set_keyboard_mode(KeyboardMode::OnDemand);
                end_of_content_ui(&window, &overlay, &pill, &vert_btn, &horiz_btn, sel);
                glib::ControlFlow::Break
            })
        };
        state_w.borrow_mut().auto_scroll_monitor = Some(monitor);
    });
}

fn end_of_content_ui(
    window: &gtk::ApplicationWindow,
    overlay: &gtk::Overlay,
    capturing_pill: &gtk::Box,
    vert_btn: &gtk::Button,
    horiz_btn: &gtk::Button,
    sel: Selection,
) {
    // Highlight Done (index 1 in the new 2-button pill).
    let mut child = capturing_pill.first_child();
    let mut idx = 0;
    while let Some(c) = child {
        let next = c.next_sibling();
        if idx == 1 {
            c.add_css_class("scroll-capture-done-highlight");
        }
        idx += 1;
        child = next;
    }
    // Bring the inside Auto-Scroll buttons back so the user can run
    // another pass or click Done.
    position_capturing_pill_and_input(
        window, overlay, capturing_pill, vert_btn, horiz_btn, sel, false,
    );
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

    let done = gtk::Button::with_label("\u{2713}  Done");
    done.add_css_class("scroll-capture-button");
    done.add_css_class("scroll-capture-primary");
    pill.append(&done);

    pill
}

/// Vertical Auto-Scroll button — small dark pill with ▼ icon, anchored
/// bottom-center inside the selection. Click sends Down-arrow keypresses.
///
/// Forces a fixed size_request so positioning + the surface input-region
/// rect agree on the button's bounds even before its first allocation.
/// Without this, `measure()` on a never-shown button can return values
/// smaller than the eventual allocation (CSS not applied yet), which both
/// off-centers the pill and leaves part of it outside the input region —
/// causing clicks to fall through to the underlying app.
fn build_inside_vert_auto_scroll() -> gtk::Button {
    let btn = gtk::Button::with_label("\u{25BC}  Auto-Scroll");
    btn.add_css_class("scroll-capture-button");
    btn.add_css_class("scroll-capture-auto");
    btn.add_css_class("scroll-capture-inside-auto");
    btn.set_size_request(VERT_AUTO_SCROLL_W as i32, VERT_AUTO_SCROLL_H as i32);
    btn
}

/// Horizontal Auto-Scroll button — circular ▶ icon, anchored right-center
/// inside the selection. Click sends Right-arrow keypresses.
fn build_inside_horiz_auto_scroll() -> gtk::Button {
    let btn = gtk::Button::with_label("\u{25B6}");
    btn.add_css_class("scroll-capture-button");
    btn.add_css_class("scroll-capture-auto");
    btn.add_css_class("scroll-capture-inside-auto");
    btn.add_css_class("scroll-capture-inside-auto-horiz");
    btn.set_size_request(HORIZ_AUTO_SCROLL_W as i32, HORIZ_AUTO_SCROLL_H as i32);
    btn
}

const VERT_AUTO_SCROLL_W: f64 = 150.0;
const VERT_AUTO_SCROLL_H: f64 = 40.0;
const HORIZ_AUTO_SCROLL_W: f64 = 44.0;
const HORIZ_AUTO_SCROLL_H: f64 = 44.0;

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
        set_pill_input_region(&window, x, y, pw, ph, sel, true);
    });
}

fn set_pill_input_region(
    window: &gtk::ApplicationWindow,
    pill_x: f64,
    pill_y: f64,
    pill_w: f64,
    pill_h: f64,
    sel: Selection,
    include_handles: bool,
) {
    let Some(surface) = window.surface() else { return };
    let pad: i32 = 6;
    let pill_rect = cairo::RectangleInt::new(
        (pill_x as i32) - pad,
        (pill_y as i32) - pad,
        (pill_w as i32) + 2 * pad,
        (pill_h as i32) + 2 * pad,
    );
    let region = cairo::Region::create_rectangle(&pill_rect);

    if include_handles {
        // Edge bands and Move handle make the selection editable. Skipped
        // during Capturing so the selection rect is fully pass-through and
        // captured frames never include our overlay's UI.
        let band = EDGE_HIT_SLACK as i32;
        let sx = sel.x as i32;
        let sy = sel.y as i32;
        let sw = sel.w as i32;
        let sh = sel.h as i32;
        let bands = [
            cairo::RectangleInt::new(sx - band, sy - band, sw + 2 * band, 2 * band),
            cairo::RectangleInt::new(sx - band, sy + sh - band, sw + 2 * band, 2 * band),
            cairo::RectangleInt::new(sx - band, sy - band, 2 * band, sh + 2 * band),
            cairo::RectangleInt::new(sx + sw - band, sy - band, 2 * band, sh + 2 * band),
        ];
        for b in &bands {
            region.union_rectangle(b).ok();
        }

        let (mcx, mcy) = ResizeHandle::Move.center(sel);
        let mr = MOVE_HANDLE_RADIUS as i32 + 6;
        region
            .union_rectangle(&cairo::RectangleInt::new(
                mcx as i32 - mr,
                mcy as i32 - mr,
                2 * mr,
                2 * mr,
            ))
            .ok();
    }

    surface.set_input_region(&region);
}

/// Distance the inside Auto-Scroll buttons sit from the selection's
/// bottom / right edge.
const INSIDE_AUTO_SCROLL_INSET: f64 = 40.0;

fn position_capturing_pill_and_input(
    window: &gtk::ApplicationWindow,
    overlay: &gtk::Overlay,
    pill: &gtk::Box,
    vert_btn: &gtk::Button,
    horiz_btn: &gtk::Button,
    sel: Selection,
    auto_scroll_active: bool,
) {
    // Place the capturing pill (Cancel/Done) OUTSIDE the selection. The
    // capture region exactly matches the selection rect, so any pixel our
    // overlay renders inside that rect ends up baked into every captured
    // frame. Below the selection if there's room; otherwise above.
    //
    // While `auto_scroll_active` is false, also position + show the inside
    // Auto-Scroll buttons (vertical and horizontal) and include their bounds
    // in the input region. While active, hide them and exclude them so they
    // never appear in captured frames.
    let window = window.clone();
    let overlay = overlay.clone();
    let pill = pill.clone();
    let vert_btn = vert_btn.clone();
    let horiz_btn = horiz_btn.clone();
    glib::idle_add_local_once(move || {
        let (pw, ph) = measured_pill_size(&pill);
        let x = (sel.x + (sel.w - pw) / 2.0).max(8.0);
        let overlay_h = overlay.allocated_height() as f64;
        let below_y = sel.y + sel.h + PILL_GAP;
        let above_y = sel.y - ph - PILL_GAP;
        let y = if below_y + ph + 8.0 <= overlay_h {
            below_y
        } else if above_y >= 8.0 {
            above_y
        } else {
            // No room outside the selection — fall back to the gap below
            // anyway (captures will pick up the pill in that corner).
            (overlay_h - ph - 8.0).max(8.0)
        };
        pill.set_margin_start(x as i32);
        pill.set_margin_top(y as i32);

        let Some(surface) = window.surface() else { return };
        let pad: i32 = 6;
        let pill_rect = cairo::RectangleInt::new(
            (x as i32) - pad,
            (y as i32) - pad,
            (pw as i32) + 2 * pad,
            (ph as i32) + 2 * pad,
        );
        let region = cairo::Region::create_rectangle(&pill_rect);

        if auto_scroll_active {
            vert_btn.set_visible(false);
            horiz_btn.set_visible(false);
        } else {
            // Vertical Auto-Scroll: bottom-center inside the selection.
            let (vw, vh) = (VERT_AUTO_SCROLL_W, VERT_AUTO_SCROLL_H);
            let vx = (sel.x + (sel.w - vw) / 2.0)
                .max(sel.x + 4.0)
                .min((sel.x + sel.w - vw - 4.0).max(sel.x + 4.0));
            let vy = (sel.y + sel.h - INSIDE_AUTO_SCROLL_INSET - vh)
                .max(sel.y + 4.0);
            vert_btn.set_margin_start(vx as i32);
            vert_btn.set_margin_top(vy as i32);
            vert_btn.set_visible(true);
            // Pad slightly so antialiased edges + size_request rounding
            // don't leave any sub-pixel slack outside the input region.
            let pad: i32 = 4;
            region
                .union_rectangle(&cairo::RectangleInt::new(
                    vx as i32 - pad,
                    vy as i32 - pad,
                    vw as i32 + 2 * pad,
                    vh as i32 + 2 * pad,
                ))
                .ok();

            // Horizontal Auto-Scroll: right-center inside the selection.
            let (hw, hh) = (HORIZ_AUTO_SCROLL_W, HORIZ_AUTO_SCROLL_H);
            let hx = (sel.x + sel.w - INSIDE_AUTO_SCROLL_INSET - hw)
                .max(sel.x + 4.0);
            let hy = (sel.y + (sel.h - hh) / 2.0)
                .max(sel.y + 4.0)
                .min((sel.y + sel.h - hh - 4.0).max(sel.y + 4.0));
            horiz_btn.set_margin_start(hx as i32);
            horiz_btn.set_margin_top(hy as i32);
            horiz_btn.set_visible(true);
            region
                .union_rectangle(&cairo::RectangleInt::new(
                    hx as i32 - pad,
                    hy as i32 - pad,
                    hw as i32 + 2 * pad,
                    hh as i32 + 2 * pad,
                ))
                .ok();

            eprintln!(
                "scroll-capture: auto-scroll buttons placed vert=({},{},{}x{}) horiz=({},{},{}x{})",
                vx as i32, vy as i32, vw as i32, vh as i32,
                hx as i32, hy as i32, hw as i32, hh as i32,
            );
        }
        surface.set_input_region(&region);
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
        // Snap to integer logical coords so Cairo's anti-aliasing doesn't
        // partially-clear the boundary rows: a partially-cleared row keeps
        // some of our dark backdrop, which becomes a faint dark line at
        // every frame seam in the stitched output.
        cr.set_operator(cairo::Operator::Clear);
        cr.rectangle(
            sel.x.round(),
            sel.y.round(),
            sel.w.round(),
            sel.h.round(),
        );
        let _ = cr.fill();

        cr.set_operator(cairo::Operator::Over);
        // Subtle outline at the selection edge for visual definition.
        // SKIPPED during Capturing: even though the stroke is mathematically
        // half a pixel outside the selection's pixel boundary, Cairo's
        // anti-aliasing bleeds a tiny fraction of the outline's alpha into
        // the boundary row of the selection. That tinted row gets included
        // in every captured frame and shows up as a visible horizontal seam
        // line in the stitched output at every frame boundary.
        if !matches!(s.phase, Phase::Capturing) {
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.55);
            cr.set_line_width(1.0);
            cr.rectangle(sel.x - 0.5, sel.y - 0.5, sel.w + 1.0, sel.h + 1.0);
            let _ = cr.stroke();
        }

        match s.phase {
            // Selected: full handle set (brackets + edge bars + Move) so
            // the user can edit the selection.
            Phase::Selected => draw_handles(cr, sel),
            // Capturing: handles intentionally HIDDEN. They'd otherwise
            // end up baked into every captured frame. The thin outline
            // drawn above is enough visual feedback for "still capturing
            // this rect".
            Phase::Capturing => {}
            // Mid-drag: minimal corner-bracket affordance.
            _ => {
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
                cr.set_line_width(BRACKET_WIDTH);
                draw_corner_brackets(cr, sel);
            }
        }
    }
    let _ = cr.restore();
}

fn draw_handles(cr: &cairo::Context, sel: Selection) {
    cr.set_operator(cairo::Operator::Over);
    cr.set_source_rgba(1.0, 1.0, 1.0, 1.0);
    cr.set_line_width(CROP_STROKE_WIDTH);
    cr.set_line_cap(cairo::LineCap::Square);
    cr.set_line_join(cairo::LineJoin::Miter);

    // Corner L-brackets — arms extend INWARD from each corner, like the
    // crop tool's brackets.
    let l = CROP_BRACKET_LENGTH;
    let x0 = sel.x;
    let y0 = sel.y;
    let x1 = sel.x + sel.w;
    let y1 = sel.y + sel.h;
    // Top-left
    cr.move_to(x0 + l, y0);
    cr.line_to(x0, y0);
    cr.line_to(x0, y0 + l);
    let _ = cr.stroke();
    // Top-right
    cr.move_to(x1 - l, y0);
    cr.line_to(x1, y0);
    cr.line_to(x1, y0 + l);
    let _ = cr.stroke();
    // Bottom-right
    cr.move_to(x1 - l, y1);
    cr.line_to(x1, y1);
    cr.line_to(x1, y1 - l);
    let _ = cr.stroke();
    // Bottom-left
    cr.move_to(x0 + l, y1);
    cr.line_to(x0, y1);
    cr.line_to(x0, y1 - l);
    let _ = cr.stroke();

    // Edge "fat bar" handles — parallel segments centered on each edge
    // midpoint, lying along the edge direction.
    let half = EDGE_HANDLE_LENGTH / 2.0;
    let mx = sel.x + sel.w / 2.0;
    let my = sel.y + sel.h / 2.0;
    // Top edge — horizontal bar at y0
    cr.move_to(mx - half, y0);
    cr.line_to(mx + half, y0);
    let _ = cr.stroke();
    // Bottom edge
    cr.move_to(mx - half, y1);
    cr.line_to(mx + half, y1);
    let _ = cr.stroke();
    // Left edge — vertical bar at x0
    cr.move_to(x0, my - half);
    cr.line_to(x0, my + half);
    let _ = cr.stroke();
    // Right edge
    cr.move_to(x1, my - half);
    cr.line_to(x1, my + half);
    let _ = cr.stroke();

    // Move handle: filled circle with a 4-way arrow glyph at the center.
    let (cx, cy) = ResizeHandle::Move.center(sel);
    let r = MOVE_HANDLE_RADIUS;
    cr.arc(cx, cy, r, 0.0, std::f64::consts::TAU);
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.6);
    let _ = cr.fill_preserve();
    cr.set_source_rgba(1.0, 1.0, 1.0, 1.0);
    cr.set_line_width(2.5);
    let _ = cr.stroke();

    // 4-way arrow glyph inside.
    let arm = r * 0.55;
    let head = r * 0.18;
    cr.set_line_width(2.0);
    cr.move_to(cx, cy - arm);
    cr.line_to(cx, cy + arm);
    cr.move_to(cx - arm, cy);
    cr.line_to(cx + arm, cy);
    let _ = cr.stroke();
    for (ax, ay, hx1, hy1, hx2, hy2) in [
        (cx, cy - arm, cx - head, cy - arm + head, cx + head, cy - arm + head),
        (cx, cy + arm, cx - head, cy + arm - head, cx + head, cy + arm - head),
        (cx - arm, cy, cx - arm + head, cy - head, cx - arm + head, cy + head),
        (cx + arm, cy, cx + arm - head, cy - head, cx + arm - head, cy + head),
    ] {
        cr.move_to(hx1, hy1);
        cr.line_to(ax, ay);
        cr.line_to(hx2, hy2);
    }
    let _ = cr.stroke();
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
