use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use smithay_client_toolkit::delegate_registry;
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_pointer, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1, zwlr_virtual_pointer_v1,
};

/// One discrete wheel notch worth of scroll, expressed in logical pixels;
/// matches libinput's ~10 px per notch convention. Browsers multiply this by
/// their own per-tick factor (~100–120 px) when scrolling.
const NOTCH_VALUE: f64 = 10.0;

struct State {
    registry_state: RegistryState,
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

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    smithay_client_toolkit::registry_handlers![];
}

delegate_registry!(State);
