# Phase 5c — Auto-Scroll Buttons Inside Selection (No Cursor Jump)

UX redesign of the Auto-Scroll trigger to (a) eliminate the visible cursor
jump that happens today, (b) support horizontal scrolling, and (c) prevent
the Auto-Scroll button from being baked into captured frames.

## Current state (after `5d00c4b`)

- The bottom **`capturing_pill`** has three buttons: `× Cancel`, `▶ Auto-Scroll`, `✓ Done`. It sits OUTSIDE the selection (below the rect, or above if there's no room below).
- Clicking Auto-Scroll triggers `start_auto_scroll` (in `src/scroll_capture/mod.rs`), which:
  1. Computes a target cursor position just below the Move-handle center, inside the selection.
  2. Spawns the worker (`auto_scroll::spawn_worker` in `src/scroll_capture/auto_scroll.rs`) with that target.
  3. Worker does `motion_absolute` to that position (this is the visible cursor jump), synthesises `BTN_MIDDLE` press+release to transfer pointer focus to the underlying surface, then loops sending Down-arrow keypresses via `zwp_virtual_keyboard_v1`.
- After end-of-content (no new retained frames for 1.5 s), the monitor calls `end_of_content_ui` which hides the Auto-Scroll button, recenters the remaining `Cancel/Done` pill, and highlights Done.
- During Capturing, handles + Move are hidden; input region = pill bounds only (`include_handles: false`).

The cursor jump still happens because `motion_absolute` moves the visible
cursor on Hyprland. We park near the Move handle to avoid landing on the
URL bar; we can't eliminate the jump because the user clicked our overlay's
Auto-Scroll button — focus is on us, we need an underlying-click to
transfer focus.

## Target UX

Two new buttons positioned **inside the selection rect**:

- **Vertical Auto-Scroll** — pill with `▼ Auto-Scroll`, anchored bottom-center inside the selection (~40 px above the bottom edge).
- **Horizontal Auto-Scroll** — circular `▶` button, anchored right-center inside the selection (~40 px from the right edge).

Reference look: the mock shared in the conversation — small dark pill with
a play-arrow icon, sitting on top of the underlying content.

When the user clicks one of these buttons:

1. The user's cursor is already inside the selection (on the clicked button) — no `motion_absolute` is needed.
2. **Hide both Auto-Scroll buttons immediately** (so they don't appear in captured frames).
3. **Remove their bounds from the surface's input region.**
4. After the next idle (so the input-region change has been committed), **synthesise a virtual-pointer click at the same position** — now in the pass-through region, so the click reaches the underlying app and transfers keyboard focus to it.
5. **Spawn the worker** with the matching direction (Down arrow for vertical, Right arrow for horizontal).

When the worker exits (end-of-content / Cancel / Done): show both buttons
again so the user can trigger another scroll pass or click Done. (Same
treatment as today's end-of-content for Done highlighting on the pill.)

The bottom `capturing_pill` becomes `× Cancel` · `✓ Done` only.

## Code touch points

| Concern | Where |
|---|---|
| Auto-Scroll button widget construction | `src/scroll_capture/mod.rs` — `build_capturing_pill()` currently builds the 3-button pill. Split into `build_capturing_pill()` (2-button) + `build_inside_vert_auto_scroll()` + `build_inside_horiz_auto_scroll()`. |
| Adding overlay children | `src/scroll_capture/mod.rs`, `build_overlay()` — currently adds `prompt`, `action_pill`, `capturing_pill` as overlay children. Add the two new buttons the same way. |
| Showing/hiding on phase transitions | `start_capture()` shows the buttons (in addition to capturing_pill). `end_of_content_ui`, `stop_capture`, and the worker-exit path should show them again. The Auto-Scroll click handler hides them. |
| Positioning | New helpers `position_vert_auto_scroll(overlay, button, sel)` and `position_horiz_auto_scroll(overlay, button, sel)`. Set `margin_start` / `margin_top` similar to existing pill positioners. Defer via `glib::idle_add_local_once` so `measured_pill_size` returns proper values. |
| Input region | `set_pill_input_region(window, pill_x, pill_y, pill_w, pill_h, sel, include_handles)` — extend signature to also take optional auto-scroll button bounds. When `auto_scroll_active=false`, include the two button rects in the region. When `true`, only the bottom pill. |
| Click handler | New `start_auto_scroll_at(state, window, overlay, capturing_pill, vert_btn, horiz_btn, direction, button_cx, button_cy)`. Hides both buttons, updates input region, schedules an idle to do the virtual-pointer click + spawn worker. |
| Worker direction | `src/scroll_capture/auto_scroll.rs` — `spawn_worker` currently always sends `KEY_DOWN`. Add a `direction: ScrollDirection` parameter (`Down`/`Right`). Update keymap to map both `KEY_DOWN` (108) and `KEY_RIGHT` (106). Send the appropriate key in the loop. |
| Eliminating cursor jump | In `spawn_worker`, the cursor position comes from the click point on the button. `motion_absolute(button_cx, button_cy, ...)` will set the virtual pointer to where the real cursor already is — no visible movement. Then `BTN_MIDDLE` press+release at that location to transfer focus. |
| State field | Add `auto_scroll_active: bool` to `OverlayState` to drive button visibility consistently across the click handler, monitor, and `stop_capture`. |

## Keymap extension (`auto_scroll.rs`)

Current keymap defines `<DOWN> = 116`. Add `<RIGHT> = 114` (kernel keycode 106 + 8 xkb offset):

```xkb
xkb_keycodes "minimal" {
    minimum = 8;
    maximum = 255;
    <DOWN>  = 116;
    <RIGHT> = 114;
};
xkb_symbols "minimal" {
    key <DOWN>  { [ Down  ] };
    key <RIGHT> { [ Right ] };
};
```

Add constants `KEY_RIGHT: u32 = 106` and `ARROWS_PER_TICK_RIGHT` (probably the same value as vertical for now, ~5).

## Suggested implementation order

1. **Refactor `capturing_pill`** to be 2 buttons (Cancel/Done). Update `wire_capturing_pill` indices to match (currently 0=Cancel, 1=Auto-Scroll, 2=Done → becomes 0=Cancel, 1=Done). Test that the existing flow still works (capture, end-of-content, done — minus auto-scroll for the moment).
2. **Add the vertical Auto-Scroll button** as a new overlay child. Position it inside the selection, bottom-center. Add it to the input region. Wire its click to log only first.
3. **Implement the click → hide + virtual-pointer-click + worker** path. Verify no cursor jump and that scrolling works.
4. **Add the horizontal Auto-Scroll button** with the `▶` icon. Same wiring, but worker direction = Right.
5. **Restore buttons on worker exit** (monitor end-of-content path, plus when `stop_capture` runs).
6. **Polish**: button styling to match the mock (rounded pill, dark bg, light text/icon). Consider a click-position-aware variant so the synthetic click lands exactly where the user clicked rather than at the button center.

## Tests / sanity checks

- Click vertical Auto-Scroll → cursor doesn't move visibly → page scrolls down → frames accumulate → end-of-content hides spinner / restores buttons.
- Click horizontal Auto-Scroll → same but page scrolls right.
- During auto-scroll, captured frames don't contain either Auto-Scroll button or the Move handle/edges (handles already hidden in Capturing per Phase 5b).
- Resize handles still work in Selected (before Start Capture).
- Done loads the stitched output into satty's annotation canvas as before.

## Out of scope

- The dual-layer transparent-overlay architecture (would let UI elements live inside the selection without being captured even when visible). Bigger refactor — see `project_satty_auto_scroll.md`.
- Move-the-whole-rect via center handle is already in place from Phase 5b; no change needed.
- Stitching algorithm refinements (some duplication still visible near end-of-page) — separate follow-up.
