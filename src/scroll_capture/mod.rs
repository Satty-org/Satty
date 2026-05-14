use std::cell::RefCell;
use std::rc::Rc;

use anyhow::{Result, anyhow};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use relm4::gtk;
use relm4::gtk::cairo;
use relm4::gtk::prelude::*;

const BACKDROP_ALPHA: f64 = 0.55;
const BRACKET_LEN: f64 = 22.0;
const BRACKET_WIDTH: f64 = 3.0;
const PILL_GAP: f64 = 18.0;
const MIN_SELECTION: f64 = 8.0;

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
}

struct OverlayState {
    phase: Phase,
    drag_origin: (f64, f64),
    selection: Selection,
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
        selection: Selection::default(),
    }));

    let window = gtk::ApplicationWindow::new(app);
    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_keyboard_mode(KeyboardMode::Exclusive);
    window.set_namespace(Some("satty-scroll-capture"));
    for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
        window.set_anchor(edge, true);
    }
    window.add_css_class("scroll-capture-overlay");

    install_css(app);

    let overlay = gtk::Overlay::new();
    window.set_child(Some(&overlay));

    let drawing = gtk::DrawingArea::new();
    drawing.set_hexpand(true);
    drawing.set_vexpand(true);
    overlay.set_child(Some(&drawing));

    // Fixed container for absolutely-positioned pill widgets. Itself not a
    // click target so empty space passes through to the DrawingArea below;
    // the child widgets (buttons, label) remain targetable by default.
    let fixed = gtk::Fixed::new();
    fixed.set_can_target(false);
    overlay.add_overlay(&fixed);

    let prompt = build_prompt_pill();
    let action_pill = build_action_pill();
    fixed.put(&prompt, 0.0, 0.0);
    fixed.put(&action_pill, 0.0, 0.0);
    action_pill.set_visible(false);

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
            let mut s = state.borrow_mut();
            s.phase = Phase::Dragging;
            s.drag_origin = (x, y);
            s.selection = Selection { x, y, w: 0.0, h: 0.0 };
            drop(s);
            prompt_w.set_visible(false);
            action_pill_w.set_visible(false);
            drawing_w.queue_draw();
        });
    }
    {
        let state = Rc::clone(&state);
        let drawing_w = drawing.clone();
        drag.connect_drag_update(move |_, dx, dy| {
            let mut s = state.borrow_mut();
            let (ox, oy) = s.drag_origin;
            let x = ox.min(ox + dx);
            let y = oy.min(oy + dy);
            s.selection = Selection {
                x,
                y,
                w: dx.abs(),
                h: dy.abs(),
            };
            drop(s);
            drawing_w.queue_draw();
        });
    }
    {
        let state = Rc::clone(&state);
        let drawing_w = drawing.clone();
        let fixed_w = fixed.clone();
        let action_pill_w = action_pill.clone();
        let prompt_w = prompt.clone();
        drag.connect_drag_end(move |_, _, _| {
            let mut s = state.borrow_mut();
            if s.selection.is_valid() {
                s.phase = Phase::Selected;
                let sel = s.selection;
                drop(s);
                position_action_pill(&fixed_w, &action_pill_w, sel);
                action_pill_w.set_visible(true);
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
        let fixed_w = fixed.clone();
        drawing.connect_resize(move |_, w, h| {
            let (pw, ph) = pill_natural_size(&prompt_w);
            let x = ((w as f64 - pw) / 2.0).max(0.0);
            let y = ((h as f64 - ph) / 2.0).max(0.0);
            fixed_w.move_(&prompt_w, x, y);
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

    // Wire pill buttons.
    {
        let window_w = window.clone();
        let cancel: gtk::Button = action_pill
            .first_child()
            .and_then(|c| c.downcast().ok())
            .expect("action pill missing cancel button");
        cancel.connect_clicked(move |_| window_w.close());
    }
    {
        let window_w = window.clone();
        let start: gtk::Button = action_pill
            .last_child()
            .and_then(|c| c.downcast().ok())
            .expect("action pill missing start-capture button");
        start.connect_clicked(move |_| {
            // Phase 3 will wire this to the capture loop. For now we just exit
            // cleanly so the Phase 2 deliverable can be exercised end-to-end.
            window_w.close();
        });
    }

    window.present();
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

fn pill_natural_size(pill: &gtk::Box) -> (f64, f64) {
    let (_, w_nat, _, _) = pill.measure(gtk::Orientation::Horizontal, -1);
    let (_, h_nat, _, _) = pill.measure(gtk::Orientation::Vertical, w_nat);
    (w_nat as f64, h_nat as f64)
}

fn position_action_pill(fixed: &gtk::Fixed, pill: &gtk::Box, sel: Selection) {
    let (pw, ph) = pill_natural_size(pill);
    let x = sel.x + (sel.w - pw) / 2.0;
    let y = sel.y + sel.h + PILL_GAP;
    // Clamp so the pill stays roughly on-screen even for selections near the bottom.
    let y = y.min(fixed.allocated_height() as f64 - ph - 8.0).max(8.0);
    fixed.move_(pill, x.max(8.0), y);
}

fn draw_backdrop(cr: &cairo::Context, w: f64, h: f64, s: &OverlayState) {
    let _ = cr.save();
    cr.set_operator(cairo::Operator::Source);

    cr.set_source_rgba(0.0, 0.0, 0.0, BACKDROP_ALPHA);
    cr.rectangle(0.0, 0.0, w, h);
    let _ = cr.fill();

    let active_rect = match s.phase {
        Phase::Dragging | Phase::Selected => Some(s.selection),
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
