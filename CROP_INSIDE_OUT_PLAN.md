# Crop View: Inside-Out Editing (Approach A)

Tracker for the crop tool refactor that adds "zoom + pan to define the
crop" alongside the existing handle-drag workflow. Independent of
`PLAN.md` (movable annotations) and `CLEANUP_PLAN.md` (refactor sweep).

## Goal

In crop **edit** mode, give the user a second way to define the crop
region without touching the handles:

1. **Plain wheel** zooms the image around the cursor. The crop frame
   stays at the same canvas position + size, so the visible-area
   that lands inside the frame shrinks.
2. **Ctrl+drag** anywhere pans the image under the (canvas-fixed)
   frame. Lets the user position the desired image region inside the
   frame without dragging handles.
3. The existing handle-drag (resize) and inside-frame click+drag
   (move frame) keep working — outside-in editing alongside the new
   inside-out one.

The committed-crop view is unchanged: once Enter commits, the
zoomed-on-the-crop view continues to render as it does today.

## Gesture map (edit mode only)

| Gesture | Action |
|---|---|
| Plain wheel | Zoom image around cursor; frame canvas-fixed |
| Super+wheel | Full-canvas zoom (existing; scales image AND frame together) |
| Click+drag on a handle | Resize frame (existing) |
| Click+drag inside frame (no handle hit) | Move frame (existing) |
| Click+drag outside frame | Start a new frame (existing) |
| Ctrl+drag anywhere | Pan image; frame canvas-fixed |
| Alt+drag | Bypass snap-to-image-edges for this drag (moved off Ctrl when Ctrl claimed pan) |
| Enter | Commit (existing) |
| Esc | Cancel / restore last-committed (existing) |

## The core idea

Today the crop frame is stored in **image coordinates** (`crop.pos`,
`crop.size` in image px). When the renderer's canvas zoom changes, the
frame scales with it — same proportion of the image stays inside, no
net change in crop region.

For inside-out editing we want the frame to behave as if it lived in
**canvas coordinates** during edit mode. Zooming the image scales the
image only; the frame stays at the same on-screen rectangle. Panning
the image scrolls the image only; the frame stays put. The image-
coordinate crop region — the thing that actually gets used on commit —
falls out as an inverse transform of the canvas frame on commit.

**Approach A**: store the frame natively in canvas coords during edit;
convert to image coords on commit. Cleaner long-term; bigger
diff up front than the hookier "image-coords with auto-compensate on
every zoom/pan" alternative.

## Phases

Each phase is one PR-sized commit (or small commit group). Each is
independently buildable and shippable.

### Phase 1 — Add canvas-coords frame representation

- [ ] **Task**
  - In `src/tools/crop.rs`, add `canvas_pos`, `canvas_size` fields to
    `Crop`. Image-coords `pos` / `size` stay as the source of truth
    for the **committed** view; the canvas-coords pair is populated
    only while `active && !committed`.
  - On `handle_activated` (entering edit mode), derive the canvas
    coords from the current image coords via the renderer's current
    transform. Need a new helper on the renderer:
    `image_to_canvas_rect(image_pos, image_size) -> (canvas_pos, canvas_size)`.
  - On `handle_deactivated` (leaving edit mode without commit) and
    on `commit`, derive image coords back from canvas via
    `canvas_to_image_rect`.
  - Rendering still draws in image coords; the canvas-coords pair is
    edit-mode bookkeeping.
  - Verify: enter crop, immediately Esc; verify the frame survives
    the round-trip unchanged.

### Phase 2 — Route canvas coords into mouse events

- [ ] **Task**
  - `MouseEventMsg` currently carries `pos: Vec2D` (image coords) for
    crop and tool dispatch. Add `canvas_pos: Vec2D` to the same
    struct so the crop tool can read it without re-running the
    inverse transform.
  - `handle_event_mouse_input` in `sketch_board.rs` (the place that
    converts widget→image) also writes the canvas-equivalent value.
  - Other tools ignore `canvas_pos`; only crop reads it.
  - Verify: build + existing crop interactions (handle drag, move-
    frame) still work — they should be untouched since they
    continue to read `pos` (image coords). The new field is just
    additional information.

### Phase 3 — Update edit-mode drag handlers to canvas coords

- [ ] **Task**
  - Inside `begin_drag` / `update_drag` / `end_drag`, when
    `crop.active && !crop.committed`, read `event.canvas_pos`
    instead of `event.pos` for the frame mutations. Update the
    canvas-coords fields; image coords stay derived.
  - Hit-testing (`test_handle_hit`, `test_inside_crop`,
    `get_closest_handle`) also runs in canvas coords during edit
    mode, against the canvas frame.
  - Hold the image-coords pair in sync after every update so
    rendering (which still uses image coords) shows the frame at
    the right place: `crop.pos = canvas_to_image(crop.canvas_pos)`.
  - Verify: handle-drag resizes the frame as before. Inside-frame
    click+drag moves the frame as before. Both feel identical to
    today.

### Phase 4 — Plain wheel = zoom image, frame stays canvas-fixed

- [ ] **Task**
  - In `sketch_board.rs` wheel handler, when active tool is `Crop`
    AND no modifiers AND `crop.is_some() && active && !committed`,
    call `renderer.set_zoom_scale_at_cursor(multiplier)` (existing
    cursor-anchored zoom).
  - The frame's canvas-coords are already fixed by Phase 1, so the
    image scales around cursor while the canvas frame stays put.
    The image-coords frame derived from canvas naturally shrinks as
    the image zooms in.
  - **Important**: also re-derive the image-coords from canvas on
    every render tick during edit mode — the canvas→image transform
    changes whenever zoom or drag_offset changes, even if the user
    isn't actively dragging. Easiest place: a hook in the renderer's
    `update_transformation` or a per-render call from the crop tool's
    `draw`.
  - Verify: enter crop, scroll wheel up over an interesting area;
    the image zooms toward cursor and the frame on-screen stays put
    (covering less of the image). Press Enter — the commit should
    use the shrunken image region.

### Phase 5 — Ctrl+drag pans the image

- [ ] **Task**
  - In the crop tool's `handle_mouse_event`, treat
    `event.modifier.contains(CONTROL_MASK)` as a pan gesture
    (regardless of where the click lands). Capture the click's
    `event.canvas_pos` as the pan origin.
  - On `UpdateDrag` with the same modifier, compute canvas delta and
    call `renderer.pan_by(dx_canvas, dy_canvas)` (existing helper).
  - On `EndDrag`, just stop tracking — pan_by has already applied.
  - Same constraint as Phase 4: re-derive image-coords frame on each
    render so the frame visually stays put during pan.
  - Verify: enter crop, Ctrl+drag from the middle — image slides
    around, frame stays put on canvas. The image region inside the
    frame changes.

### Phase 6 — Edge cases + polish

- [ ] **Task**
  - **Window resize during edit**: the canvas frame is in canvas-px,
    but if the canvas widget resizes (window resize), the canvas
    coords are still valid in the new canvas's pixel grid — but the
    user probably expects the frame to keep covering the same image
    region. Decide: on canvas resize during edit, re-derive canvas
    coords from image coords. (Behavior closer to today.)
  - **Zoom hitting the min/max clamp** (10%/500%): the frame's
    derived image-coords could become degenerate or extend past
    image bounds. Clamp the derived image-coords to the image rect
    on commit. Show a visual cue if the frame extends past image
    edges (existing "outside image" handling probably needs review).
  - **FitCanvas / Abs zoom commands during edit**: these reset
    `drag_offset` and change `scale_factor`. The canvas frame
    survives, but the on-screen position may look stale until the
    next derive. Make sure the derive runs on those paths too.
  - **Committed crop re-edit**: `handle_activated` flips `committed`
    off. At that moment, populate canvas-coords from image-coords
    using the renderer's CURRENT transform (which was just the
    committed-crop transform). Should give the same on-screen
    rectangle as the committed view.
  - Verify each: resize window mid-edit; zoom to max; FitCanvas
    after a partial zoom; re-edit a committed crop.

## Risks

1. **Hit-testing in canvas vs image coords**: the existing helpers
   work in image coords. Phase 3 has them switch to canvas during
   edit mode. Any helper that mixes the two (e.g., a hit test that
   touches both the frame and an outside-image margin) needs to be
   audited. Likely: a single set of helpers parametrised on which
   coord system to use, called with the right one per mode.

2. **Two sources of truth**: dual storage (canvas + image coords) is
   prone to drift. Strict rule: during edit mode, **canvas is the
   source of truth** and image is derived on every change. On commit:
   compute final image-coords and discard canvas. Document this
   prominently in `crop.rs`.

3. **Pan limits**: the existing `drag_offset` clamping logic prevents
   the image from sliding entirely off-screen. With Ctrl+drag in crop
   mode the user might WANT to pan further than usual to position a
   distant image region inside the frame. Audit the clamps;
   consider relaxing during crop edit, or leaving as-is and relying
   on zoom to bring distant regions into reach.

4. **Renderer internal changes**: Phase 4 wants a "re-derive on every
   render" hook in `update_transformation`. Crop tool ↔ renderer
   tight coupling. Consider exposing a small callback or having the
   crop tool register itself with the renderer for transform-change
   notifications. Keep the coupling minimal.

5. **Existing Esc-cancels-uncommitted behavior**: today, leaving
   crop without Enter discards in-progress changes and restores
   `last_committed`. With canvas-coords editing, the "in-progress
   changes" are now in canvas-space. Make sure the restore still
   reverts cleanly — store the last-committed in image coords
   (existing) and re-derive canvas coords from it on re-entry.

## Verification checklist

After all phases, smoke-test:

- [ ] Enter crop on a fresh screenshot; whole image in frame; press
      Enter — commits the whole image as the crop. (Sanity.)
- [ ] Drag a handle inward; press Enter — commits the smaller frame.
- [ ] Drag inside the frame to move it; press Enter — commits the
      moved frame.
- [ ] Plain wheel up over an interesting area; frame stays in place
      on canvas; image scales around cursor; press Enter — commits a
      smaller image region centered on what was under cursor.
- [ ] Ctrl+drag image around with a smallish frame; the visible
      image-inside-frame changes; press Enter — commits the dragged
      region.
- [ ] Mix all three: drag handle to shrink, plain wheel to zoom,
      Ctrl+drag to pan, Enter to commit. Final crop should be the
      composition.
- [ ] Esc out of edit mode without Enter; re-enter; should be back
      where it was at last commit (or fresh seed if never committed).
- [ ] Window resize during edit; frame should keep covering the
      same image region.
- [ ] Hit zoom min (10%) and max (500%) during crop edit; frame
      shouldn't go degenerate; commit should still produce a valid
      crop.

## Out of scope

- **Toolbar responsiveness** (separate session per user).
- **Outside-in crop polish** (existing handle behavior unchanged).
- **Live preview during edit** (the dark overlay outside the frame
  already does this).
- **Alt+drag axis-locked pan** — gesture is reserved but not
  implemented; punt until requested.

## Progress summary

- Phase 1 (canvas-coords field): 1 / 1
- Phase 2 (canvas_pos in MouseEventMsg): 1 / 1
- Phase 3 (drag handlers use canvas): 1 / 1
- Phase 4 (plain wheel = zoom image): 1 / 1
- Phase 5 (Ctrl+drag = pan): 1 / 1
- Phase 6 (edge cases + polish): 1 / 1

**Total: 6 / 6.**

## Implementation notes / deviations from plan

- **Phase 3 — source of truth direction**: the plan declared canvas
  the source of truth during edit, with image derived after each
  change. The shipped Phase 3 inverts that: image stays the source
  of truth that snap + aspect + clamp math operates on, and canvas
  is refreshed from image after every drag tick. Phase 4/5 reverse
  the derivation direction at the wheel + pan hooks (canvas stays
  put, image re-derives). Both work because each gesture writes one
  side and re-derives the other, keeping them consistent. Net UX
  result is the same — drag still snaps and aspect-locks the way
  it did pre-refactor; wheel + pan still hold the canvas frame.

- **Phase 5 — snap-disable modifier moved Ctrl → Alt.** Ctrl is
  reserved for the pan gesture, so the temporary "hold to defeat
  snap-to-image-edges" override moved onto Alt. Shift was a
  non-starter (every other tool treats it as snap-TO-angle, the
  opposite convention). The bottom-bar hint label updated to
  "Hold Alt to disable snapping." Alt+drag for axis-lock pan that
  the gesture map originally reserved isn't implemented yet — if
  it lands, it'll have to share with snap-defeat or pick a
  different modifier.

- **Phase 6 — transform sync direction policy**:
  - Wheel zoom (Phase 4): canvas fixed, image re-derives.
  - Ctrl+drag pan (Phase 5): canvas fixed, image re-derives.
  - Window resize: image-region fixed, canvas re-derives.
  - Toolbar zoom (In/Out/FitCanvas/Abs): image-region fixed, canvas
    re-derives (treated as "frame the canvas around the image"
    rather than "reflow image under the frame").
  - Renderer-level `update_transformation` push: always refreshes
    the cached transform on the crop tool, but only the
    canvas-size-changed window-resize branch auto-runs the A
    direction. Caller-triggered transforms still decide their own
    direction at the call site.

