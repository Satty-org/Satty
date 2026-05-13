# Satty Cleanup & Refactor Plan

Tracker for dead-code removal and architectural cleanup. Independent from the
`feature/movable-annotations` work in `PLAN.md`.

## How to use this doc

- Work top-to-bottom by tier. Each tier is safe to land on its own.
- Each task is a checkbox. Tick `- [x]` when merged.
- Each task lists **Verify** steps. Do them before ticking. If a verify step
  fails, the task isn't done — figure out why.
- Each task is meant to be **one commit** (small, reviewable, easy to revert).
- Re-verify dead-code claims with the grep commands shown — code moves.

## Standard verify checklist

For any change, the minimum is:

1. `cargo build` — zero new warnings, ideally fewer.
2. `cargo clippy --no-deps` — no new lints, ideally fewer.
3. Launch the app on a real screenshot and smoke-test the **affected
   surface only**. For example: deleting unused color methods → no UI test
   needed. Splitting the color picker → must open the picker, pick
   palette + custom + saved-custom + drag-reorder, all still work.

For Tier 2/3 work, also do a **broad smoke test** at the end of the tier:

- Open with a screenshot loaded.
- Draw with each tool (Pointer, Crop, Rect, Ellipse, Line, Arrow, Brush,
  Marker, Highlight, Spotlight, Blur, Text).
- Open the color popover, pick a palette color, pick custom, save a custom,
  drag-reorder, delete a saved-custom.
- Toggle each style picker (arrow style, blur style, highlight style,
  text-bg style, fill, smoothness).
- Resize via the size slider; nudge annotation factor.
- Crop, resize image, change crop bg color.
- Undo / redo a chain of edits across tools.
- Selection: click-select, marquee, move, handle-resize, Ctrl+A, Delete,
  Alt+D duplicate.
- Text: type, IME preedit (if available), arrow keys, selection, paste.
- Save to file; copy to clipboard.

If any of those break, the change is wrong.

---

## Tier 1 — Dead code removal

Zero behavior change. Each task is independently safe.

### 1.1 Delete unused `HighlighterStyle` import

- [x] **Task**
  - File: `src/sketch_board.rs:1302`
  - Remove `HighlighterStyle` from the `use crate::tools::{...}` line.
  - Verify: `cargo build` → one fewer warning.

### 1.2 Delete `ColorPalette::custom()`

- [x] **Task**
  - File: `src/configuration.rs:215–222` (the `custom()` method).
  - Re-check before deleting: `grep -rn "\.custom()\|ColorPalette::custom" src/`
    — should only match the definition.
  - Delete the method.
  - Verify: `cargo build` → one fewer warning.
  - **Note**: the underlying `ColorPalette.custom: Vec<Color>` field is now
    write-only (populated from the config file's `custom = [...]` entry but
    never read). Compiler doesn't flag it (probably the `From`/serde glue
    counts as a use). Open question for later: should `custom` and
    `ColorPaletteFile.custom` be removed entirely (breaks user config-file
    compat) or kept as a no-op (silent acceptance of stale config)? Add
    to "Open questions" below if not handled in Tier 1.5.

### 1.3 Delete `band_at_y()`

- [x] **Task**
  - File: `src/text_bands.rs:182`
  - Re-check: `grep -rn "band_at_y" src/` — only the definition.
  - Delete the function and its doc comment.
  - Verify: `cargo build` → one fewer warning.

### 1.4 Delete `Color::to_rgba_f64()` and `Color::to_rgba_u32()`

- [x] **Task**
  - File: `src/style.rs:219` and `src/style.rs:227`.
  - Re-check: `grep -rn "to_rgba_f64\|to_rgba_u32" src/` — only definitions.
  - Delete both methods.
  - Verify: `cargo build`.

### 1.5 Delete `ToolsToolbarInput::ReorderCustomColor` and its handler

- [x] **Task**
  - File: `src/ui/toolbars.rs`
  - Remove the variant at line 2041.
  - Remove its handler around line 3404 (the comment confirms it's dead:
    "Programmatic reorder (currently unused…)").
  - Re-check: `grep -rn "ReorderCustomColor" src/` — zero matches after.
  - Verify: `cargo build` → one fewer warning.

### 1.6 Delete `ToolsToolbarInput::ResizeImageRequested` and its handler

- [x] **Task**
  - File: `src/ui/toolbars.rs`
  - Remove the variant at line 2107.
  - Remove its handler at line 3742.
  - Re-check: `grep -rn "ResizeImageRequested" src/` — zero matches.
  - Verify: `cargo build`.

### 1.7 Delete `StyleToolbarInput::ShowAnnotationDialog` and its handler

- [x] **Task**
  - File: `src/ui/toolbars.rs`
  - Remove the variant at line 2123.
  - Remove its handler at line 4964 (calls `show_annotation_dialog`). If
    nothing else calls `show_annotation_dialog`, also delete that method.
  - Re-check: `grep -rn "ShowAnnotationDialog\|show_annotation_dialog" src/`.
  - Verify: `cargo build`. Annotation pill should still work (drag + inline
    edit are the live paths, not this dialog).
  - **Done**: deletion cascaded further than the plan anticipated. Once
    nothing fired `ShowAnnotationDialog`, the whole `AnnotationSizeDialog`
    component became unreachable, so the commit also removed:
    - The `AnnotationSizeDialog` struct + entire `impl Component` block
    - `AnnotationSizeDialogInput` / `AnnotationSizeDialogOutput` enums
    - `StyleToolbar::annotation_dialog_controller` field + init
    - The now-stale `Window` import and the `root` parameter in
      `StyleToolbar::update` (renamed to `_root`)
    - Stale doc comment in `welcome.rs` that referenced the dialog
  - `AnnotationDialogFinished` stays — it's reused by the welcome-dialog
    flow in `main.rs:640` to push the saved annotation size into the
    style toolbar.
  - **Knock-on**: Task 3.5 (split `AnnotationSizeDialog` to its own
    file) is no longer applicable. Marked deleted below.

### 1.8 Delete `StyleToolbarInput::DimensionsChanged` and its handler

- [x] **Task**
  - File: `src/ui/toolbars.rs`
  - Remove the variant at line 2173.
  - Remove its handler at line 5141 (comment confirms: "Dimensions display
    moved to App's bottom_row.end_widget; ignore here so the variant can be
    deprecated later").
  - Trace senders: `grep -rn "DimensionsChanged" src/` — confirm nothing
    sends it (the handler ignores it but something might still emit it; if
    so, remove that emit too).
  - Verify: `cargo build`. Bottom-bar dimensions still display correctly.

### 1.9 Run cargo fix to clear remaining trivial warnings

- [x] **Task**
  - Run `cargo fix --bin satty --allow-dirty` for the auto-fixable lints
    (the "use suggestion" ones from clippy).
  - Manually review the diff — reject any rewrite that hurts readability.
  - Verify: `cargo build && cargo clippy --no-deps`.
  - **Done** — used `cargo clippy --fix --bin satty -p satty -- --no-deps`.
    24 → 18 clippy warnings, 0 build warnings. Files touched:
    - `src/display.rs` — `if ….is_none() { return None }` → `…?;`
    - `src/hyprland.rs` — `.iter().any()` → `.contains()` (1.12 item)
    - `src/tools/brush.rs` — `1 | 2 | 3` → `1..=3` patterns (1.12 item)
    - `src/tools/text.rs` — `% 2 == 0` → `.is_multiple_of(2)` (1.12 item)
    - `src/ui/toolbars.rs` — collapsed nested `if` + `if let` to a let
      chain (1.12 item); had to manually fix the body indentation
      cargo-fix left over-indented.

---

## Tier 1.5 — Real bugs flagged by clippy

These aren't just style — the duplicate-block ones look like genuine logic
errors that happen to be self-cancelling.

### 1.10 Investigate duplicate `if/else` blocks in boundary-flip

- [x] **Task**
  - File: `src/sketch_board.rs:1041–1061`.
  - **Verdict: not a bug, but the structure was misleading.** Both branches
    correctly *guard* different cases (`dx < 0` going off-left vs `dx > 0`
    going off-right) and they *should* both flip via `dx = -dx`. Clippy
    was flagging the structural redundancy (`if A { x } else if B { x }`),
    not a logic error.
  - Refactored to `let flip_dx = case_a || case_b; if flip_dx { dx = -dx; }`
    for each axis. Same behavior, one decision per axis, clippy clean.
  - Did NOT need the UI verify step — pure structural refactor with
    identical observable behavior.

### 1.11 Fix `manual_checked_ops` in blur

- [x] **Task**
  - File: `src/tools/blur.rs:307`
  - Used `sum.checked_div(count).unwrap_or(0)` per clippy. Replaces a
    9-line if/else with 4 lines — also drops the now-redundant
    `(0, 0, 0, 0)` default because `unwrap_or(0)` per component
    produces the same transparent-black fallback.
  - Verified: clippy clean (1 warning gone).

### 1.12 Other small clippy cleanups

- [x] **Task**
  - ~~`src/tools/brush.rs:105,109` — range patterns~~ (done by 1.9)
  - `src/tools/brush.rs:138` — kept the index loop with
    `#[allow(clippy::needless_range_loop)]` + a comment. Clippy's
    enumerate-skip-take rewrite is uglier and clippy itself used
    `<item>` as a placeholder.
  - ~~`src/tools/text.rs:1411` — `.is_multiple_of(2)`~~ (done by 1.9)
  - ~~`src/hyprland.rs:138` — `.contains(...)`~~ (done by 1.9)
  - ~~`src/ui/toolbars.rs:3358` — let-chain collapse~~ (done by 1.9)
  - `src/ui/toolbars.rs` doc continuations: root cause was `///` lines
    starting with `+` (used as a plus sign in formula/prose, but
    markdown reads it as a list bullet). Reflowed each case so the
    `+` lands at end of the previous line. Also split a merged
    two-paragraph doc block near `PICKER_COLORPLANE_WIDTH` with a
    blank `///` separator.
  - `src/sketch_board.rs` match → `matches!` applied.
  - `src/tools/arrow.rs:457` and `src/femtovg_area/imp.rs:1460` —
    `too_many_arguments`: `#[allow]` on each. Both are render
    functions whose params are individually meaningful; bundling
    would just scatter call sites without clarity gain.
  - Verify: `cargo clippy --no-deps` → **0 warnings**.

---

## Tier 2 — Targeted dedup

Behavior-preserving consolidation. Each task is verifiable independently.

### 2.1 Generic `apply_property_to_selection` helper

- [x] **Task**
  - File: `src/sketch_board.rs`
  - Today: `apply_arrow_style_to_selection` (1133), `apply_blur_style_to_selection`
    (1162), `apply_brush_smooth_to_selection` (1195) all share the same
    shape (iterate selected, clone, set property if Some, decide
    Unmodified/Modify/Modifies based on count).
  - Add a private helper:
    ```rust
    fn apply_to_selection<F>(&self, mut f: F) -> ToolUpdateResult
    where F: FnMut(&mut dyn Drawable) -> bool { … }
    ```
    where `f` returns `true` if the drawable was actually modified.
  - Rewrite the three callers to use it.
  - Verify: select a shape, change its arrow/blur/brush style from the
    toolbar — should still update; selection should still indicate
    Modify/ModifyDrawables correctly (multi-select changes should still
    register as one undo step).

### 2.2 Generic `cycle_seed` helper

- [x] **Task**
  - File: `src/sketch_board.rs:1226–1287`
  - `cycle_seed_arrow`, `cycle_seed_blur`, `cycle_seed_text` (and to a
    lesser extent `cycle_seed_highlighter`) share the same pattern: query
    selection, if singleton, extract a property; else load from state.toml.
  - Unify into a generic helper parameterised on the extractor closure and
    the state-load fn.
  - Verify: double-tap each tool's letter shortcut (cycles to the next
    style) — should cycle through the right options for each tool.

### 2.3 Unify silent/non-silent style setters

- [x] **Task**
  - File: `src/ui/toolbars.rs:5239–5386`
  - Today: `SetArrowStyle` / `SetArrowStyleSilently`, `SetTextBackground` /
    `SetTextBackgroundSilently`, etc. — pairs that differ only in whether
    they emit upstream.
  - Replace the variant pairs with single variants taking
    `{ value: X, emit: bool }` (or merge into one and add a separate
    sketch-board→toolbar "sync from selection" path that sets the local
    flag without re-dispatching the variant).
  - **Caveat**: read each callsite carefully — some paths rely on the
    silent variant to update internal state during selection sync without
    triggering a render cycle.
  - Verify: change a style via toolbar (full path), then select a shape
    with a different style (sync path) — both should work, and selecting
    should not loop or double-emit.

### 2.4 Consolidate `Text` RefCells into `LayoutCache`

- [x] **Task**
  - File: `src/tools/text.rs`
  - Today: 8 separate `RefCell` fields on `Text` (rect, editing_rect,
    last_line_height, last_css_to_image, last_natural_text_width, glyphs,
    line_ranges, cursor_visible).
  - Group the layout-derived ones into a `LayoutCache` struct held by a
    single `RefCell<LayoutCache>`. Keep `cursor_visible` separate (it's
    independent state, not derived).
  - Pay attention to borrow scopes — fewer RefCells means longer borrow
    chains, which can deadlock under nested calls. If you hit borrow
    errors, the structure is wrong, not the locking.
  - Verify: type text, edit existing text, IME preedit, resize wrap-box,
    save/load — all still work.

### 2.5 Extract `tools::common::handle` for handle render + hit-test

- [x] **Task**
  - Files: `src/tools/text.rs`, `src/tools/pointer.rs` (and any other tool
    that renders handles).
  - Create `src/tools/common.rs` (or extend `src/tools/mod.rs`) with:
    - `render_handle(canvas, pos, kind, dpr)` — disc + ring.
    - `Handle::hit_test(point, tolerance)` — already on `Handle`?
      Confirm one canonical impl.
  - Replace duplicated bodies in text.rs and pointer.rs.
  - Verify: drag handles on text (wrap-box), arrow (endpoints), rect (8
    handles), etc. — all still feel identical (hit radius matters).

---

## Tier 3 — File splits

Structural moves. Each split is one PR. Do dedup (Tier 2) first so we're
not splitting code that's about to disappear.

### Pre-split checklist

Before each split:

- Make sure the file is at a clean checkpoint (build green, no
  uncommitted unrelated work).
- The split commit should be **moves only** — no logic changes. If
  something needs to change to compile, that's a separate commit before
  the move.
- After the move, run the full smoke-test checklist (top of doc).

### 3.1 Split `src/ui/toolbars.rs` — pull out tooltip helpers

- [ ] **Task**
  - Lines 1–188: `RobustTooltipExt`, `ACTIVE_TOOLTIP`, tooltip helpers.
  - New file: `src/ui/tooltip.rs`.
  - Update `src/ui/mod.rs` to declare it.
  - Make the public surface `pub(crate)` if only used internally.
  - Verify: tooltips still appear/disappear on hover for both toolbars.

### 3.2 Split `src/ui/toolbars.rs` — pull out the color picker subsystem

- [ ] **Task**
  - The largest single chunk (~2,200 lines): `ColorPopoverHandles` (504),
    `build_color_popover` + chooser internals (528–849),
    `build_inline_picker_panel` (850–996), floating swatch tooltip
    (997–1146), `build_color_popover_grid` + saved-custom helpers
    (1148–2670).
  - Plus the color-related `ToolsToolbarInput` variants and their handlers
    (drag-reorder, save-custom, delete-custom, etc.).
  - New file: `src/ui/color_picker.rs` (or a sub-module dir
    `src/ui/color_picker/mod.rs` if it wants further splitting).
  - The seam: `ToolsToolbar` calls into the color picker module to build
    the popover and routes color-related events to it.
  - **Risk**: this is the biggest single move. Do it in stages — first
    move pure helpers (no state), then the popover builders, then the
    handlers.
  - Verify: full color-picker smoke test — palette, custom RGB,
    saved-custom grid, drag-reorder, empty-slot click, dynamic refresh,
    floating swatch tooltip.

### 3.3 Split `src/ui/toolbars.rs` — pull out crop controls

- [ ] **Task**
  - Crop-related fields, input variants, and handlers (~1,200 lines).
  - New file: `src/ui/crop_controls.rs`.
  - Verify: crop mode toggle, W/H entry, image-resize popover, bg-color
    preset selection, resize units toggle (px ↔ %).

### 3.4 Split `src/ui/toolbars.rs` — pull out style pickers

- [ ] **Task**
  - blur/arrow/highlighter/text-bg dropdowns and their build helpers
    (~600 lines).
  - New file: `src/ui/style_pickers.rs`.
  - Consider whether this can become a single generic component
    parameterised on the enum it picks from (depends on Tier 2.3 unifying
    the silent setter pattern first).
  - Verify: change each style from the dropdown; select a shape with each
    style and confirm dropdown reflects it.

### 3.5 ~~Split `AnnotationSizeDialog`~~ — N/A

- [x] **Obsolete**: Task 1.7 confirmed the dialog was fully dead and
  removed it. Nothing left to split.

### 3.6 Split `src/sketch_board.rs` — pull file I/O out

- [ ] **Task**
  - Lines 715–945: save/save_as/clipboard/external-process I/O.
  - New file: `src/file_export.rs`.
  - The seam: free functions taking `(pixbuf, destination, config)` —
    `SketchBoard` calls them, no shared state.
  - Verify: save to file, copy to clipboard, copy-as-text, copy-filepath,
    save-to-external-process (the `--copy-command` path).

### 3.7 Split `src/sketch_board.rs` — pull cursor logic out

- [ ] **Task**
  - Lines 2277–2476: `update_hover_cursor`, `idle_cursor_for_active_tool`,
    `custom_drawing_cursor`, `apply_idle_cursor`.
  - New file: `src/cursor_logic.rs` (note: existing `src/ui/cursor.rs` is
    something else — name to avoid collision).
  - Inputs: `image_pos`, active tool, selection state, hit-test result.
    Output: cursor name string + side effects on the renderer.
  - Verify: cursor changes when hovering handles, hit-test bodies, blank
    canvas, with different active tools, in crop mode.

### 3.8 Split `src/sketch_board.rs` — break up `handle_toolbar_event`

- [ ] **Task**
  - The 549-line match (lines 1374–1921).
  - Split into one private fn per variant: `handle_tool_selected`,
    `handle_color_selected`, `handle_size_selected`, etc.
  - `handle_toolbar_event` becomes a thin dispatcher: `match event { … =>
    self.handle_X(...) }`.
  - This is mechanical but very fiddly — variants share local state
    (e.g., `tool_before_crop` capture, snapback cascades). Each extracted
    fn either takes `&mut self` (most cases) or returns a small "do this
    next" struct.
  - Verify: every toolbar interaction still works end-to-end. Tool switch
    + snapback paths (Spotlight/Highlighter/Brush/Rectangle/Ellipse) need
    extra attention.

### 3.9 Split `src/tools/text.rs` — IME / preedit rendering

- [ ] **Task**
  - Lines 1109–1220 + supporting `segments_for_line_span` (1265–1310).
  - New file: `src/tools/text/ime_preedit.rs` (move text.rs to
    `src/tools/text/mod.rs` first, or use a free-function module).
  - Inputs: `TextDrawingContext`, preedit state, paint. Output: canvas
    draw calls.
  - Verify: IME preedit (e.g., type in a CJK input method or `ibus`/`fcitx`
    test) — underlines, segment backgrounds, foreground colour, cursor
    inside preedit all render correctly. If IME isn't reachable on the
    dev machine, this task may need to be deferred.

### 3.10 Split `src/tools/text.rs` — layout module

- [ ] **Task**
  - `LineLayout`, `TextDrawingContext`, `text_width`, `glyph_rect`,
    `editing_box`, and the wrap/measure logic currently inside
    `Text::draw`.
  - New file: `src/tools/text/layout.rs`.
  - The seam: `LayoutResult { lines, glyphs, bbox, ... }` produced by
    `compute_layout(text, paint, canvas)`, consumed by `Text::draw`.
  - This is the biggest payoff — `Text::draw` shrinks from 479 lines to
    ~150.
  - Verify: text rendering identical (compare screenshots before/after).
    Wrap, multi-line, hit-test, glyph selection all unchanged.

### 3.11 Split `src/tools/text.rs` — editing action module

- [ ] **Task**
  - Lines 2448–2855: `Action`, `ActionScope`, `handle_text_buffer_action`.
  - New file: `src/tools/text/editing_action.rs`.
  - The seam: `apply_action(buffer, action) -> ActionResult` — pure
    function of (buffer, action), returns what changed.
  - Verify: arrow keys, shift-arrows (selection), ctrl-arrows (word
    motion), home/end, ctrl-home/end (buffer ends), delete, backspace —
    all behave identically.

---

## Open questions / deferred

Things flagged during diagnosis that aren't action items yet:

- **`tools::common` module scope** — do we want a shared module for
  common Tool patterns (bounds inflation, handle math, etc.)? Tier 2.5
  is the first step; if it goes well, expand. If it fights us, the tools
  are heterogeneous enough that each can keep its own helpers.
- **Color picker as a Relm4 sub-component?** — Tier 3.2 extracts it to a
  module; promoting it to a true Relm4 component (with its own
  Input/Output enums) is a separate, bigger question. Defer until 3.2
  is in.
- **`PLAN.md` movable-annotations work** — that's a separate workstream.
  When it lands, re-evaluate whether Pointer-tool dedup with Text-tool
  handle rendering (Tier 2.5) needs to happen sooner.
- **Generated icon code** (`src/icons/mod.rs`) — has `#![allow(dead_code)]`
  by design. Don't touch.

---

## Progress summary

Update these numbers as tasks land:

- Tier 1 dead code: 9 / 9 tasks (1.1–1.9) ✅
- Tier 1.5 clippy: 3 / 3 tasks (1.10–1.12) ✅ — clippy now 0 warnings
- Tier 2 dedup: 5 / 5 tasks (2.1–2.5) ✅
- Tier 3 splits: 1 / 11 tasks (3.5 obsolete after 1.7 cascade)

**Total: 18 / 28 tasks.**
