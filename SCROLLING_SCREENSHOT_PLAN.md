# Scrolling Screenshot Plan

Goal: scrolling screenshot capture inside satty. Region-select on a dimmed overlay, manual scroll + libei auto-scroll, stitch in-process, hand off to the existing annotation canvas. Ship manual + auto in a single PR.

## UX flow (from mockups)

1. **Entry hotkey** fires `satty --scroll-capture`.
2. Satty opens a **fullscreen layer-shell overlay** with a centered pill: *"Drag to capture the scrolling part of the screen."*
3. User **drags a rectangle**. On release, the overlay dims everything outside the rect (spotlight backdrop), draws corner brackets, and shows a pill below the selection: `× Cancel` · `→ Start Capture`.
4. User clicks **Start Capture**. Pill swaps to `× Cancel` · `▶ Auto-Scroll` · `✓ Done`. The Auto-Scroll button sits **inside** the selection (bottom-centered) so clicking it parks the cursor inside the scroll target. Done is always available — the user can stop at any time.
5. Two ways frames accumulate:
   - **Manual:** user scrolls in their actual window (browser/etc.); capture loop polls wlr-screencopy on the region, keeps frames that visually differ from the previous one. User clicks **Done** when satisfied to finalize.
   - **Auto-Scroll:** click the button → satty uses libei to send wheel events at the cursor location → browser scrolls ~300–400px → satty waits for the frame to settle → captures → repeats. Stops automatically on end-of-content (two consecutive pixel-identical frames). User can also click Done or Cancel at any time.
6. When auto-scroll stops (end-of-content), the Auto-Scroll button hides and the pill becomes `× Cancel` · `✓ Done` with Done highlighted as the obvious CTA.
7. **Done** → stitch in memory → load the tall Pixbuf into satty's normal canvas (all existing tools/save/copy work). **Cancel** → quit.

## Constraints worth pinning down

- **No capture code today.** `Cargo.toml` has no `wayland-client`, no `ashpd`, no screencopy bindings. Pixbuf-only input pipeline (`src/main.rs:1505-1516`).
- **Wayland forbids cross-window input synthesis.** Resolved: use libei via the `reis` crate. Works on Hyprland 0.40+ (your compositor) and GNOME 46+; degrades cleanly on compositors without libei support (Auto-Scroll button hidden, manual scroll still works).
- **Layer-shell input is all-or-nothing.** A pass-through region cannot also report cursor position. Once we hand input back to the browser for manual scroll, we lose pointer tracking. The Auto-Scroll/Cancel/Done buttons are small input-region islands on the overlay; the rest of the selection is pass-through. **De-risked by UX:** Done is always visible during capture, so the user can stop at any time. "Cursor leaves area → stop auto-scroll" is therefore a nice-to-have, not load-bearing — best-effort via libei input events if the compositor exposes them.
- **Global hotkey on Wayland.** Both paths in scope for this PR:
  - **Compositor binding** (cheap): `[scroll-capture]` section in `config.toml` documents the suggested keybind; user binds `satty --scroll-capture` in `hyprland.conf`. Zero runtime cost. Works today.
  - **xdg-desktop-portal GlobalShortcuts** (full daemon mode): satty registers via the portal and idles as a background process listening for the trigger signal; on fire, spawns the capture overlay in-process. New `--daemon` flag; portal availability varies by compositor and degrades cleanly to a clear error message if unsupported.

## Architecture touch-points (from recon)

| Concern | Location |
|---|---|
| CLI args (clap) | `cli/src/command_line.rs` |
| `Tools` enum (CLI side) | `cli/src/command_line.rs:218-248` |
| `main()` / app boot | `src/main.rs:1452 (run_satty)`, `:1550 (main)` |
| Image load → Pixbuf | `src/main.rs:1505-1516` |
| Relm4 root component | `src/main.rs` (`App` struct ~line 64+) |
| Canvas / GL | `src/femtovg_area/imp.rs` (`background_image: Pixbuf` ~:383, `render_native_resolution` ~:1232) |
| Sketchboard state machine | `src/sketch_board.rs` |
| `Tool` trait + `ToolsManager` | `src/tools/mod.rs:52-254`, `:898-954` |
| Toolbar UI | `src/ui/toolbars.rs` |
| Config file loading | `src/config.rs` + `config.toml` at repo root |

## New crate dependencies (load-bearing choices)

| Crate | Purpose | Risk |
|---|---|---|
| `wayland-client` | Wayland connection, registry | well-trodden |
| `wayland-protocols-wlr` | `zwlr_screencopy_manager_v1` | well-trodden |
| `smithay-client-toolkit` | helpers for output enumeration, SHM | well-trodden |
| `gtk4-layer-shell` | layer-shell binding for the overlay window | mature, used by many GTK4 status tools |
| `reis` | libei bindings for auto-scroll | newer crate; may need pinning to a known-working tag |
| `ashpd` | xdg-desktop-portal GlobalShortcuts (Phase 6 only) | mature, optional |

## Phases

### Phase 1 — Capture spike + region capture

**Deliverable:** wlr-screencopy bindings produce a Pixbuf for a full output AND for a sub-region of an output. New module `src/capture/{mod.rs, wlr_screencopy.rs}` exposing `capture_output(output_id)` and `capture_region(output_id, rect)`. SHM (not DMA-BUF) for the first cut.

**Smoke test:** a dev-only CLI flag `--scroll-capture-test FULL` or `--scroll-capture-test x,y,w,h` that writes the result to stdout/file via the existing Pixbuf path — proves capture works before we build any UI on top.

**Risks:** GTK4 main loop vs wayland-client event dispatch. Run capture on a dedicated thread, deliver bytes back via a channel; GTK side wraps in Pixbuf on the main thread.

### Phase 2 — Layer-shell overlay + region selection

**Deliverable:** `satty --scroll-capture` opens a fullscreen, partially-transparent layer-shell overlay. Centered instructional pill. User drags → rectangle with corner brackets. Backdrop dims (alpha ~0.55) outside the selection (spotlight style, reuse the existing spotlight tool's drawing approach where possible). Below-selection pill with `× Cancel` / `→ Start Capture`. Esc cancels, click outside cancels.

**Work:**
- New module `src/scroll_capture/mod.rs` owning the overlay state machine (states: `AwaitingDrag`, `Dragging`, `Selected`, `Capturing`, `Stopped`).
- Reuse selection-drag math from `src/tools/crop.rs` where it fits (constrained rect, corner-bracket rendering).
- Layer-shell window opened via `gtk4-layer-shell` with `layer = Overlay`, `keyboard_mode = Exclusive`, `anchor = all sides`.
- Pill widgets: GTK4 buttons styled to match the mockup (rounded, semi-translucent dark background, light text). Probably reusable as a small `OverlayPill` widget.

**Done when:** user can run `satty --scroll-capture`, drag a region, see corner brackets and the Start Capture pill, click Cancel and have satty exit.

### Phase 3 — Capture loop with manual scroll

**Deliverable:** clicking Start Capture begins continuous region capture. Frames buffered in memory. UI shows `× Cancel` · `▶ Auto-Scroll` · `✓ Done`. As user scrolls in their app, frames that visually differ from the previous one are retained. Clicking Done ends capture and proceeds to stitch + handoff.

**Work:**
- During Capturing state, drop the overlay's input region down to **just the three pill buttons** (small input islands) so input passes through to the underlying app for scrolling.
- Capture timer (GLib timeout, ~10 Hz) calls `capture_region` on the worker thread; main thread compares the new frame to the last retained frame using a fast hash on a horizontal stripe (5–10 rows from mid-height). If different, keep; else drop.
- Frame buffer: `Vec<CapturedFrame { pixels, captured_at, prev_diff_offset_hint }`.
- Done button: stops the capture timer, hands the frame buffer to Phase 5's stitcher.

**Done when:** with the capture mode running, the user can scroll their browser, see frames accumulating (debug indicator/log), and click Done to proceed.

### Phase 4 — Auto-scroll via libei

**Deliverable:** clicking Auto-Scroll synthesizes wheel events at the cursor location; satty captures after each scroll settles; loop stops on end-of-content, Cancel, or Done. (Done is already visible; Auto-Scroll just automates what manual scroll did.)

**Work:**
- Add `reis` dep; on first Auto-Scroll click, connect to libei (xdg-desktop-portal's `RemoteDesktop` interface is the standard request path) and obtain a virtual pointer.
- Loop: send wheel-by-N events (configurable; default ~350px equivalent — likely 8–10 discrete wheel notches depending on browser), wait ~150–250ms for layout to settle, capture, compare. If the new frame is pixel-identical to the previous one for two consecutive iterations → end of content, stop.
- Cursor-leave detection: nice-to-have via libei input events if available; not load-bearing since Done is always present.
- On end-of-content: hide Auto-Scroll button, highlight Done as the CTA.

**Risks:** `reis` API churn — pin a tag. Wheel event semantics differ between toolkits (GTK vs Qt vs browsers); 350px nominal may translate to wildly different actual scrolls in different apps. Live with it for v1 — the stitching algorithm handles variable deltas anyway.

### Phase 5 — Stitching + handoff

**Deliverable:** Done → frames stitch into one tall Pixbuf → satty's normal `App` opens with that Pixbuf as `background_image`. All existing tools (crop, brush, blur, etc.) work.

**Work:**
- Stitching algorithm in `src/scroll_capture/stitch.rs`. For each consecutive frame pair, find the best y-offset by sliding the new frame over the old and minimizing sum-of-absolute-differences across a horizontal band — same algorithm whether the scroll came from auto or manual. Fixed-header duplication is a known limitation; document.
- Build the final image: width = selection width, height = sum of unique-pixel rows. Render rows in order, blending the seam zone (~4px alpha-fade) to hide minor anti-aliasing differences.
- Convert to Pixbuf; spawn the normal satty pipeline with that Pixbuf injected at the point `main.rs:1505-1516` currently loads from stdin/file.

**Risks:**
- femtovg texture size limits — very tall stitches (10k+ px) may exceed `GL_MAX_TEXTURE_SIZE` on some GPUs. Detect and fall back to tiled upload, or cap stitched height with an informative error.
- Sticky headers will duplicate. Accept for v1; potential follow-up: let user mask a header band before stitching.

### Phase 6 — Hotkey wiring + config

**Deliverable:** both hotkey paths working — compositor binding (cheap path) and daemon mode + portal GlobalShortcuts (full path).

**Work — 6a (compositor binding):**
- New `[scroll-capture]` section in `config.toml`:
  - `hotkey = "Super+Shift+S"` (documentation default; what 6b registers, what 6a documents)
  - `auto_scroll_pixels = 350` (per-step nominal)
  - `auto_scroll_settle_ms = 200`
- `--scroll-capture` CLI flag enters the mode directly. This is what compositor-bound hotkeys invoke.
- README documents `bind = SUPER SHIFT, S, exec, satty --scroll-capture`.

**Work — 6b (daemon + portal GlobalShortcuts):**
- Add `ashpd` dep.
- New `--daemon` flag. Daemon mode connects to xdg-desktop-portal, registers the `scroll-capture` shortcut (with the keybind from config), then idles on the GTK main loop waiting for the `Activated` signal.
- On signal fire: spawn the capture overlay in-process (same code path `--scroll-capture` triggers). When the user finishes (Done/Cancel), the overlay closes and the daemon returns to idle.
- Single-instance lock so two daemons don't compete for the shortcut.
- Graceful failure on compositors without the portal: log a clear error, exit cleanly; user can still use 6a's compositor-bound path.
- User-facing docs: how to autostart `satty --daemon`, how to choose between the two paths.

**Done when:**
- Compositor-bound `satty --scroll-capture` works.
- `satty --daemon` registers the shortcut via the portal, and the configured hotkey fires the capture overlay even with no satty window currently open.
- Config defaults match docs; README explains both options and tradeoffs.

## Out of scope for this branch

- Portal/ashpd capture backend (GNOME/KDE-only compositors).
- Horizontal-scroll content.
- Per-frame masking of sticky headers/footers (potential follow-up).
- Video/animated capture.

## Merge strategy

Single PR off `feature/scrolling-screenshot` → `main` once all six phases are functional.
