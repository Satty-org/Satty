use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// linux/input-event-codes.h BTN_MIDDLE. Synthetic middle-click is used to
/// transfer pointer focus from our overlay to the underlying surface before
/// sending wheel events — most apps have no action bound to middle-click.
const BTN_MIDDLE: u32 = 0x112;

/// linux/input-event-codes.h KEY_PAGEDOWN. PgDn is widely respected by
/// scrollable apps (browsers, editors, viewers).
const KEY_PAGEDOWN: u32 = 109;

/// Minimal xkb keymap. Maps xkb keycode 117 (kernel KEY_PAGEDOWN=109 + 8
/// xkb offset) to the Page_Down keysym. The compositor uses this keymap to
/// interpret the key events we send via the virtual keyboard.
const KEYMAP_TEMPLATE: &str = r#"xkb_keymap {
    xkb_keycodes "minimal" {
        minimum = 8;
        maximum = 255;
        <PGDN> = 117;
    };
    xkb_types "complete" {
        type "ONE_LEVEL" {
            modifiers = none;
            level_name[Level1] = "Any";
        };
    };
    xkb_compatibility "complete" {};
    xkb_symbols "minimal" {
        key <PGDN> { [ Page_Down ] };
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
        pointer.axis_discrete(
            time,
            wl_pointer::Axis::VerticalScroll,
            NOTCH_VALUE,
            1,
        );
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
    let fd: OwnedFd = memfd_create("satty-keymap", MemfdFlags::empty())
        .context("memfd_create for keymap failed")?
        .into();
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
pub fn spawn_worker(stop: Arc<AtomicBool>, cursor_x: i32, cursor_y: i32) -> Result<()> {
    let conn = Connection::connect_to_env()
        .context("auto-scroll: failed to connect to wayland")?;
    let (globals, mut event_queue) =
        registry_queue_init::<State>(&conn).context("auto-scroll: registry init")?;
    let qh = event_queue.handle();
    let manager = globals
        .bind::<zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1, _, _>(&qh, 1..=2, ())
        .context("compositor does not expose zwlr_virtual_pointer_manager_v1")?;
    let kbd_manager = globals
        .bind::<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1, _, _>(
            &qh, 1..=1, (),
        )
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

        // Position the virtual pointer at the target then synthesize a
        // middle-click to encourage Hyprland to transfer focus from our
        // overlay to the underlying surface.
        let t = time_ms(start);
        pointer.motion_absolute(t, cx, cy, sw as u32, sh as u32);
        pointer.frame();
        pointer.button(t + 1, BTN_MIDDLE, wl_pointer::ButtonState::Pressed);
        pointer.frame();
        pointer.button(t + 2, BTN_MIDDLE, wl_pointer::ButtonState::Released);
        pointer.frame();
        let _ = event_queue.flush();
        thread::sleep(Duration::from_millis(80));

        while !stop.load(Ordering::Relaxed) {
            // Press + release PageDown. Most scrollable apps respond.
            let t = time_ms(start);
            keyboard.key(t, KEY_PAGEDOWN, wl_keyboard::KeyState::Pressed.into());
            keyboard.key(t + 1, KEY_PAGEDOWN, wl_keyboard::KeyState::Released.into());
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
