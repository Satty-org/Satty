use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rustix::fs::{MemfdFlags, memfd_create};
use smithay_client_toolkit::delegate_registry;
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1, zwp_virtual_keyboard_v1,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1, zwlr_virtual_pointer_v1,
};

/// One discrete wheel notch worth of scroll, expressed in logical pixels;
/// matches libinput's ~10 px per notch convention. Browsers multiply this by
/// their own per-tick factor (~100–120 px) when scrolling.
const NOTCH_VALUE: f64 = 10.0;

/// linux/input-event-codes.h KEY_DOWN. We send several Down-arrow presses
/// per scroll tick instead of one PgDn so the per-tick scroll delta is
/// smaller than the user's selection height — that way each captured frame
/// overlaps the previous one and the stitcher can find the alignment.
const KEY_DOWN: u32 = 108;

/// linux/input-event-codes.h KEY_RIGHT — used for horizontal auto-scroll.
const KEY_RIGHT: u32 = 106;

/// Direction the auto-scroll worker should drive the underlying app.
#[derive(Clone, Copy, Debug)]
pub enum ScrollDirection {
    Down,
    Right,
}

impl ScrollDirection {
    fn keycode(self) -> u32 {
        match self {
            ScrollDirection::Down => KEY_DOWN,
            ScrollDirection::Right => KEY_RIGHT,
        }
    }
}

/// How many Down-arrow presses per scroll tick. 5 presses ≈ 200 logical px
/// of scroll in most browsers (~40 logical px per arrow), small enough to
/// fit within typical selections while still progressing visibly.
pub const ARROWS_PER_TICK: u32 = 5;

/// Approximate scroll (in logical pixels) produced by a single Down-arrow
/// keypress in typical browsers. The stitcher uses this as a hint so the
/// SAD search only has to refine within a narrow window around the
/// expected delta — much faster than an unconstrained search and immune
/// to sticky-header local minima.
pub const ARROW_SCROLL_LOGICAL_PX: u32 = 40;

/// Minimal xkb keymap. Maps xkb keycode 116 (kernel KEY_DOWN=108 + 8 xkb
/// offset) to the Down keysym and 114 (kernel KEY_RIGHT=106 + 8) to Right.
const KEYMAP_TEMPLATE: &str = r#"xkb_keymap {
    xkb_keycodes "minimal" {
        minimum = 8;
        maximum = 255;
        <DOWN>  = 116;
        <RIGHT> = 114;
    };
    xkb_types "complete" {
        type "ONE_LEVEL" {
            modifiers = none;
            level_name[Level1] = "Any";
        };
    };
    xkb_compatibility "complete" {};
    xkb_symbols "minimal" {
        key <DOWN>  { [ Down  ] };
        key <RIGHT> { [ Right ] };
    };
};
"#;

/// Time between auto-scroll wheel tick groups. ~600ms is a comfortable
/// reading cadence and gives the underlying app time to render new content.
pub const SCROLL_INTERVAL_MS: u64 = 600;

/// How many wheel notches per auto-scroll tick. 3 notches ≈ 300–360px in
/// most browsers (notch * the app's per-tick pixel multiplier).
pub const NOTCHES_PER_TICK: u32 = 3;

struct State {
    registry_state: RegistryState,
    output_size: Option<(i32, i32)>,
}

/// Smoke test: connect, bind the virtual-pointer manager + first seat, send
/// 3 wheel-down notches with 600ms pauses. Whatever is under the cursor at
/// invocation time should scroll three times.
pub fn smoke_test() -> Result<()> {
    eprintln!("auto-scroll-test: connecting to wayland...");
    let conn = Connection::connect_to_env().context("failed to connect to wayland display")?;
    let (globals, mut event_queue) =
        registry_queue_init::<State>(&conn).context("registry init failed")?;
    let qh = event_queue.handle();

    let manager = globals
        .bind::<zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1, _, _>(&qh, 1..=2, ())
        .context("compositor does not expose zwlr_virtual_pointer_manager_v1")?;
    let seat = globals
        .bind::<wl_seat::WlSeat, _, _>(&qh, 1..=8, ())
        .context("no wl_seat available")?;

    let mut state = State {
        registry_state: RegistryState::new(&globals),
        output_size: None,
    };
    let pointer = manager.create_virtual_pointer(Some(&seat), &qh, ());
    event_queue
        .roundtrip(&mut state)
        .context("roundtrip after virtual-pointer creation")?;

    eprintln!("auto-scroll-test: sending 3 wheel-down notches...");
    let start = Instant::now();
    for i in 0..3 {
        scroll_down(&pointer, 1, time_ms(start));
        event_queue.flush().context("flush after scroll event")?;
        eprintln!("auto-scroll-test:   notch {} sent", i + 1);
        thread::sleep(Duration::from_millis(600));
    }

    pointer.destroy();
    Ok(())
}

/// Emit `notches` wheel-down clicks via the virtual pointer. Caller is
/// responsible for flushing the wayland connection afterwards.
pub fn scroll_down(
    pointer: &zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
    notches: u32,
    time: u32,
) {
    pointer.axis_source(wl_pointer::AxisSource::Wheel);
    for _ in 0..notches {
        pointer.axis_discrete(time, wl_pointer::Axis::VerticalScroll, NOTCH_VALUE, 1);
        pointer.axis(time, wl_pointer::Axis::VerticalScroll, NOTCH_VALUE);
    }
    pointer.frame();
}

/// Wayland event timestamps are milliseconds since some monotonic epoch.
/// We just pass our own monotonic counter; the compositor only uses these
/// for ordering, not absolute time.
fn time_ms(start: Instant) -> u32 {
    let _ = (SystemTime::now(), UNIX_EPOCH); // silence unused-import lint
    start.elapsed().as_millis() as u32
}

fn make_keymap_fd() -> Result<(OwnedFd, u32)> {
    let keymap = format!("{}\0", KEYMAP_TEMPLATE);
    let size = keymap.len() as u32;
    let fd: OwnedFd = memfd_create("tensaku-keymap", MemfdFlags::empty())
        .context("memfd_create for keymap failed")?;
    let mut file = std::fs::File::from(fd);
    file.write_all(keymap.as_bytes())
        .context("writing keymap to memfd")?;
    Ok((file.into(), size))
}

/// Spawn a worker thread that positions the virtual pointer at `(cursor_x,
/// cursor_y)` in compositor coordinates, then loops sending `Page Down` key
/// events via a virtual keyboard until `stop` is set. Caller controls the
/// stop signal; the worker exits on the next loop iteration after stop=true,
/// destroys the virtual devices, and returns.
///
/// Uses keyboard PgDn rather than mouse wheel events: on Hyprland with a
/// layer-shell overlay running, wlr-virtual-pointer wheel events don't reach
/// the underlying app (separate investigation — see project memory). PgDn
/// is widely respected by scrollable apps (browsers, editors, viewers) and
/// keyboard event routing may have different focus semantics that work
/// through our overlay's pass-through input region.
pub fn spawn_worker(
    stop: Arc<AtomicBool>,
    cycle_counter: Arc<AtomicU64>,
    cursor_x: i32,
    cursor_y: i32,
    direction: ScrollDirection,
) -> Result<()> {
    let conn = Connection::connect_to_env().context("auto-scroll: failed to connect to wayland")?;
    let (globals, mut event_queue) =
        registry_queue_init::<State>(&conn).context("auto-scroll: registry init")?;
    let qh = event_queue.handle();
    let manager = globals
        .bind::<zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1, _, _>(&qh, 1..=2, ())
        .context("compositor does not expose zwlr_virtual_pointer_manager_v1")?;
    let kbd_manager = globals
        .bind::<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1, _, _>(&qh, 1..=1, ())
        .context("compositor does not expose zwp_virtual_keyboard_manager_v1")?;
    let seat = globals
        .bind::<wl_seat::WlSeat, _, _>(&qh, 1..=8, ())
        .context("no wl_seat available")?;
    let _output = globals
        .bind::<wl_output::WlOutput, _, _>(&qh, 1..=4, ())
        .context("no wl_output available")?;

    let mut state = State {
        registry_state: RegistryState::new(&globals),
        output_size: None,
    };
    // Two roundtrips to pick up wl_output mode events reliably.
    event_queue
        .roundtrip(&mut state)
        .context("roundtrip 1 for wl_output mode")?;
    event_queue
        .roundtrip(&mut state)
        .context("roundtrip 2 for wl_output mode")?;
    let (sw, sh) = state
        .output_size
        .context("wl_output did not report a mode")?;

    let pointer = manager.create_virtual_pointer(Some(&seat), &qh, ());

    let cx = cursor_x.clamp(0, sw) as u32;
    let cy = cursor_y.clamp(0, sh) as u32;

    // Create the virtual keyboard and upload our keymap before spawning
    // the worker thread, so the compositor has parsed the keymap by the
    // time we start sending key events.
    let keyboard = kbd_manager.create_virtual_keyboard(&seat, &qh, ());
    let (keymap_fd, keymap_size) = make_keymap_fd()?;
    keyboard.keymap(
        wl_keyboard::KeymapFormat::XkbV1.into(),
        keymap_fd.as_fd(),
        keymap_size,
    );
    event_queue.flush().context("flush after keymap upload")?;

    thread::spawn(move || {
        let start = Instant::now();

        // Position the virtual pointer at the target. We deliberately do
        // NOT synthesise a button click: a left-click would trigger link
        // navigation, a middle-click puts Chrome into autoscroll mode
        // (compass icon + erratic scrolling), and a right-click opens a
        // context menu. Pointer motion alone is enough to transfer focus
        // when the compositor uses focus-follows-pointer (Hyprland's
        // default `follow_mouse = 1`).
        //
        // BUT: a single motion_absolute to the exact pixel where the
        // user's cursor already is gets de-duped by the compositor — no
        // wl_pointer.motion is generated, no pointer.enter on the
        // underlying surface, no focus transfer. User-observed symptom:
        // "had to jiggle the mouse a tiny bit for auto-scroll to start."
        //
        // Fix: send the motion to a 1-px-offset position FIRST (forcing
        // a real motion event), then to the actual target. Two adjacent
        // motions give the compositor a real delta to process.
        let nudge_x = cx.saturating_add(1).min(sw.saturating_sub(1) as u32);
        let t = time_ms(start);
        pointer.motion_absolute(t, nudge_x, cy, sw as u32, sh as u32);
        pointer.frame();
        let _ = event_queue.flush();
        thread::sleep(Duration::from_millis(20));
        let t = time_ms(start);
        pointer.motion_absolute(t, cx, cy, sw as u32, sh as u32);
        pointer.frame();
        let _ = event_queue.flush();
        // Give the compositor a couple of frames to propagate the focus
        // change before we start sending arrow keys.
        thread::sleep(Duration::from_millis(200));
        eprintln!(
            "auto-scroll: parked (no click, nudged) at ({},{}) within {}x{}",
            cx, cy, sw, sh
        );

        let keycode = direction.keycode();
        // Order of operations per cycle:
        //   1. Signal "a stable frame is ready" by bumping the counter.
        //   2. Wait long enough for the main-thread capture_tick (100 ms
        //      cadence) to pick it up.
        //   3. THEN send the next batch of arrows + wait for them to
        //      render.
        //
        // The previous order (arrows → sleep → bump) meant the very first
        // captured frame was already one cycle past the initial view —
        // the page-top (search bar, location chip, "Sponsored result"
        // heading row) had scrolled out before any capture happened. With
        // this ordering, cycle 1's bump captures the true initial view,
        // cycle 2's bump captures the post-first-scroll view, etc.
        let inter_capture_settle = Duration::from_millis(150);
        while !stop.load(Ordering::Relaxed) {
            cycle_counter.fetch_add(1, Ordering::Relaxed);
            thread::sleep(inter_capture_settle);
            if stop.load(Ordering::Relaxed) {
                break;
            }
            // Send several arrow presses per tick. Each arrow scrolls
            // ~40 logical px in most browsers — small enough that the per-
            // tick scroll stays within the user's selection rect and the
            // captured frames overlap enough for the stitcher to align.
            let t = time_ms(start);
            for i in 0..ARROWS_PER_TICK {
                keyboard.key(t + i * 2, keycode, wl_keyboard::KeyState::Pressed.into());
                keyboard.key(
                    t + i * 2 + 1,
                    keycode,
                    wl_keyboard::KeyState::Released.into(),
                );
            }
            let _ = event_queue.flush();
            thread::sleep(Duration::from_millis(SCROLL_INTERVAL_MS));
        }
        keyboard.destroy();
        pointer.destroy();
        let _ = event_queue.flush();
        eprintln!("auto-scroll: worker exited");
    });

    eprintln!(
        "auto-scroll: worker started ({}× notches every {}ms)",
        NOTCHES_PER_TICK, SCROLL_INTERVAL_MS
    );
    Ok(())
}

impl Dispatch<zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
        _: zwlr_virtual_pointer_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
        _: zwlr_virtual_pointer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
        _: zwp_virtual_keyboard_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
        _: zwp_virtual_keyboard_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Mode { width, height, .. } = event {
            state.output_size = Some((width, height));
        }
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    smithay_client_toolkit::registry_handlers![];
}

delegate_registry!(State);
