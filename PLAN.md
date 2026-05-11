# Movable Annotations for Satty — Plan & Status

## Where we are

Working on `/home/jon/Code/satty` (the user's local clone of `Satty-org/Satty`),
on branch **`feature/movable-annotations`**. The goal is to add direct
manipulation: select existing annotations, move them, drag
handles to reshape, restyle from the toolbar, multi-select via marquee and
Ctrl+A. The original Satty was "fire and forget" — once committed, drawables
were immutable and could only be removed via undo.

## What's done

### Core architecture
- **Stable IDs** — every committed drawable gets a `DrawableId(u64)`. The
  stack stores `Vec<Stacked { id, drawable }>` (`src/femtovg_area/imp.rs`).
- **Action-based undo/redo** — `UndoAction { Add, Remove, Modify, Batch }`.
  Redo via inverse of inverse. Multi-shape ops use `Batch` so a single
  Ctrl+Z reverses the whole group (`src/tools/mod.rs`, applied in
  `apply_inverse` in `src/femtovg_area/imp.rs`).
- **Drawable trait extensions** (`src/tools/mod.rs`):
  - `bounds() -> Option<Rect>`
  - `hit_test(point, tolerance) -> bool`
  - `translate(delta)`
  - `handles() -> Vec<Handle>` — endpoint handles for arrow/line, 8-handle
    bbox for rect/ellipse/blur/highlight-block.
  - `move_handle(handle_id, to)`
  - `set_style(style)`
  - `render_glow(canvas, font, bounds)` — selection halo per shape.
- **`HandleId` enum** with `Start`, `End`, `Control` (curved/double arrow
  Bezier mid-handle), and 8 bbox positions.
- **Per-shape impls** — every shape in `src/tools/{rectangle,ellipse,line,
  arrow,brush,highlight,marker,blur,text}.rs` implements the new methods.

### Pointer tool & implicit selection
- **`PointerTool` (`src/tools/pointer.rs`)** — single source of truth for
  selection state. `selected: Vec<DrawableId>`, `drag: Option<DragState>`,
  `marquee: Option<MarqueeState>`, `active_as_primary: bool`.
- **Implicit selection mode** — when a non-Pointer tool (e.g. Arrow) is
  active, mouse and key events are routed through the Pointer tool first
  (`SketchBoard::update` mouse + key branches). It returns
  `RedrawAndStopPropagation` / `ModifyDrawable` / `DeleteDrawable` etc. when
  it consumes; otherwise falls through to the active drawing tool.
- **Click-off-to-deselect** — empty BeginDrag with a prior selection only
  deselects, doesn't fall through to the drawing tool. Click-on-existing
  consumes the post-drag Click so e.g. Text doesn't create a new box.
- **`DrawableStore` trait** (`src/tools/mod.rs`) — read-only renderer view
  exposing `hit_test`, `clone_drawable`, `drawables_in_rect`,
  `all_drawable_ids`. Injected via `set_drawable_store` at tool-activation
  and once at init for the Pointer tool (so it works in implicit mode).

### Selection visuals
- **Glow trace** — semi-transparent blue halo per shape. For closed
  shapes (rect, ellipse, blur, block highlight) the glow uses an *offset
  path* — a slightly-inflated copy of the shape — so the halo is entirely
  outside the silhouette (no bleed into stroked-rect interiors). Standard
  arrow uses `GLOW_STROKE_WIDTH_WIDE` (14 px) to overcome the rounded-corner
  outline overlay; everything else uses `GLOW_STROKE_WIDTH` (8 px).
- **Handles** — 12 px solid blue inner disc + 2 px white ring. Drawn only
  for single selection (not multi-select).
- **Marquee rectangle** — faded blue fill + thin stroke, drawn during
  drag-rect selection.

### Multi-select
- **Drag-rect marquee** (only when Pointer tool is the active tool):
  empty BeginDrag → start marquee, UpdateDrag extends it, EndDrag finalizes
  via `DrawableStore::drawables_in_rect` (intersect bounds).
- **Ctrl+A** → select all (`PointerTool::handle_key_event`).
- **Delete/Backspace** on multi-selection → `DeleteDrawables(Vec<DrawableId>)`
  → `FemtoVgAreaMut::delete_many` → single `UndoAction::Batch` so one
  Ctrl+Z restores all.
- **Multi-restyle** — toolbar color/size/fill on multi-selection routes
  through `PointerTool::handle_style_event` → `ModifyDrawables(Vec<...>)`
  → `FemtoVgAreaMut::modify_many` (also a Batch).

### Arrow geometry & styles
- Four `ArrowStyle` variants: `Standard`, `Fancy`, `Curved`, `Double`
  (`src/tools/arrow.rs`).
- **Standard** — tapered tail (small back stub at ~7 % of tail width),
  rounded-corner triangle head with a "shoulder notch" where head meets tail
  (the inner corner is slightly forward of the head's outer base, ~12 % of
  head length).
- **Fancy** — thin stroked shaft + filled triangular head.
- **Curved** — quadratic Bezier shaft + filled head at the end. Default
  curvature = 25 % of chord length, but the user can drag a `Control`
  handle (mid-curve) to bend the arc to any angle. The control point is
  stored on `Arrow.curve_control: Option<Vec2D>` (None = use auto default).
- **Double** — same Bezier shaft, filled heads at both ends.
- **Toolbar dropdown** — `gtk::DropDown` next to size buttons,
  `set_visible: model.current_tool == Tools::Arrow`. Currently labels-only
  (no rendered icons).

### Sizes
- 6-step scale: `XSmall`, `Small`, `Medium`, `Large`, `XLarge`, `XXLarge`
  (`src/style.rs`).
- All `Size::to_*` methods (text, line width, arrow head/tail, blur,
  highlight) have 6 arms.
- Toolbar has 6 buttons: `XS / S / M / L / XL / XXL`. Tooltips include the
  full name.
- `MarkerTool` has its own marker-specific text-size scale (smaller than
  the generic text scale) in `marker.rs::Marker::marker_text_size`.

### Cursors
- `update_hover_cursor` in `sketch_board.rs` — runs on `PointerPos` events.
  - Hovering a handle of selection → resize cursor (`nwse-resize`,
    `nesw-resize`, `ns-resize`, `ew-resize`, `move` for endpoints).
  - Hovering any drawable body → `grab`.
  - Empty canvas + drawing tool active → `crosshair`.
  - Empty canvas + Pointer/Crop active → default arrow.
- `apply_idle_cursor` runs on tool switch so the cursor updates immediately.

### Text BG pill
- `Text::draw` renders a rounded ivory rectangle behind the text bounds
  using the cached glyph rect (one-frame lag on the very first draw).
- Click-off-no-create — clicking outside an in-progress text only commits
  it, doesn't create a new one on the same click (`text.rs` Click handler).

### Markers
- Single solid filled disc (no outer ring).
- Smaller text size for the X-Small / Small variants.

### Toolbar polish
- All toolbar buttons (top + bottom + color palette + arrow dropdown +
  size buttons + dimensions label + annotation-size dialog) use a custom
  `RobustTooltipExt` helper (`install_tooltip` for downward / top toolbar,
  `install_tooltip_above` for upward / bottom toolbar) that bypasses
  GTK4's tooltip system entirely. Each widget gets its own `gtk::Popover`
  with a `gtk::Label` child, controlled by a per-widget
  `EventControllerMotion`: popup on enter, popdown on leave. An 8 px
  `set_offset` keeps the popover from crowding the button edge. The
  popover is unparented in `connect_destroy` to avoid GTK shutdown
  warnings. We did this because GTK4's tooltip subsystem keeps a
  window-level "tooltip recently shown" flag that only resets when the
  pointer leaves the toplevel window — toggling `has-tooltip` or
  returning true from `query-tooltip` doesn't clear it, so subsequent
  hovers within the same window silently failed.
- Color palette buttons get tooltips with their digit shortcut.

### Re-entrant borrow fix
- `PointerTool::build_overlay` previously called back to
  `self.store.clone_drawable(...)` to fetch the selected drawable for
  handle computation. The renderer holds a mutable borrow on its `inner`
  state across this call (it passes the drawable in as the `selected`
  parameter), so the callback panicked with `RefCell already borrowed`
  whenever a single-selection render hit the overlay path. Fix: use the
  `selected` parameter directly instead of calling back into the store.

## What's pending

In rough priority order:

### 1. Arrow dropdown — render shape icons
Currently the dropdown is text-only. The user wants each option to show a
small rendering of the arrow shape next to its name.

Plan:
- Add a `gtk::SignalListItemFactory` to the dropdown.
- `connect_setup` builds a `gtk::Box[gtk::Image, gtk::Label]` per row.
- `connect_bind` sets the label and renders a Pixbuf via Cairo
  (similar to how each shape's render_glow path is built — see
  `Arrow::standard_path`, `head_path`, `bezier_control`).
- Store rendered Pixbufs once at toolbar init (not per bind).

The cleanest path is to render to a `cairo::ImageSurface` (cairo-rs is
already a transitive dep via gtk), convert to `gdk::Texture` /
`gdk_pixbuf::Pixbuf`. Don't try to use femtovg here — it's GL-based and
would require an offscreen FBO.

### 2. Custom pen-tool cursor
User spec for the Brush tool:
- Innermost: 8 px outer dia, 2 px gray border, 6 px transparent inside.
- Middle: 2 px black border, wraps the innermost (so outer dia ≈ 12 px).
- Outermost: 2 px gray border, wraps the middle (outer dia ≈ 16 px).

Plan:
- Render the three concentric rings to a `cairo::ImageSurface` (e.g. 24×24
  with the cursor centered).
- Convert to `gdk::MemoryTexture` (premultiplied BGRA).
- `gdk::Cursor::from_texture(&texture, hotspot_x, hotspot_y, None)`.
- In `sketch_board::apply_idle_cursor` and `update_hover_cursor`, when
  active tool is `Tools::Brush`, set this cursor via
  `self.renderer.set_cursor(Some(&cursor))`.
- Cache the cursor (build once, reuse).

The cursor should NOT replace the hover cursors for handles/grab — those
should still take priority. Only the *idle* / *empty-canvas* cursor for
Brush is the precision pen.

### 3. Multi-drag move
Currently clicking a member of a multi-selection collapses to single before
dragging. To properly "drag all selected at once":

- Refactor `DragState` to carry a `Vec<DragItem>` instead of one item.
- `dragging_drawable_ids() -> Vec<DrawableId>` (replaces the singular
  `dragging_drawable_id`). The renderer needs to skip *all* of them.
- `working_copies(&self) -> Vec<&dyn Drawable>` (new Tool method) — the
  renderer iterates this for rendering (with glow under each).
- On EndDrag with multi-drag: emit `ModifyDrawables(Vec<(id, working)>)`
  (already exists) — single Batch undo restores all positions.

The renderer's `render` already uses `selected_drawables: Vec<DrawableId>`
(no further change needed there for the glow loop). It only uses
singular `dragging_drawable_id` — that's the spot to fan out.

### 4. ~~Text edit UI overhaul~~ — **DONE**
- Blue rectangle outline around the wrap area during editing, scaled
  in CSS pixels (constant on-screen thickness regardless of zoom).
- `Text::text_box_width: Option<f32>` carries the explicit wrap width;
  new texts default to `DEFAULT_INITIAL_BOX_WIDTH` (220 image-space
  px) so the empty editing box has a finite outline before any typing.
- Side handles (`HandleId::Left` / `Right` at vertical midpoints) drag
  to adjust `text_box_width`, triggering text reflow on the next draw.
  Left also moves `pos.x` so the right edge stays put.
- Bottom-right corner handle scales `annotation_size_factor` based on
  height delta from BeginDrag, so text reflows at the new font size.
  Width updates independently from the same handle (single-handle
  resize, two effects).
- `Drawable::handles()` returns the three handles based on bounds.
  Bounds during editing report the wrap-area rect; bounds for
  committed selection inflate the glyph rect to match
  `text_box_width` when set.
- Editing-mode handles are also exposed via `Tool::editing_handles()`
  so `update_hover_cursor` lights up the resize cursors over them,
  matching committed-selection behavior.
- Double-click on a committed Text re-enters edit mode. PointerTool
  detects this in its `Click` handler (n_pressed == 2, hit drawable
  downcasts to `Text` via the new `Drawable::as_any` method) and
  emits `ToolUpdateResult::EditTextDrawable(id)`. sketch_board
  switches to TextTool and calls `Tool::enter_text_edit_mode`, which
  clones the committed Text into the editing slot and stamps
  `TextTool::edit_target_id = Some(id)`. The renderer hides the
  original via `dragging_drawable_id`. On commit/deactivate, TextTool
  emits `ModifyDrawable(id, …)` instead of `Commit(…)` so the
  existing drawable is replaced in place (single undo entry).
- New `Drawable::as_any -> &dyn Any` trait method (one-line override
  per impl). Enables `downcast_ref::<Text>()` from PointerTool's
  double-click handler.
- `FemtoVgAreaMut::hit_test` now skips drawables whose id matches
  either tool's `dragging_drawable_id`, so re-editing a Text doesn't
  cause a body-drag of the (hidden) original when the user clicks
  inside the editing copy. Uses `try_borrow` so a tool calling
  hit_test while it's already borrowed mutably doesn't panic.

### 5. Glow polish
- Currently the Standard arrow's glow uses a 14 px stroke at the path; the
  inner half is masked by the arrow's fill. Works for filled arrows but
  could look uneven when the arrow is short (head dominates). Consider an
  offset-path glow for arrow if it bothers visually.
- Curved/Double arrow glow uses a wider Bezier stroke at the same path,
  similar to lines. The visible halo is on both sides of the curve, which
  is OK but not ideal.

### 6. Misc UX
- Shift+Click to add to multi-selection (currently click always replaces).
- Multi-handle resize when multiple shapes are selected (group bbox handles).
- Cursor change while dragging marquee (something other than crosshair).
- `Tab` / `Shift+Tab` cycling between arrow styles was removed when the
  dropdown was added — could be re-added if the user wants a keyboard
  shortcut.

## Architecture map

- `src/main.rs` — App component (relm4). Wires SketchBoard, ToolsToolbar,
  StyleToolbar.
- `src/sketch_board.rs` — central event router. `SketchBoard::update`
  handles input events, dispatches to active tool *and* the pointer tool
  (for implicit-mode selection). `dispatch_style_change` fans toolbar
  style changes to both. `update_hover_cursor` + `apply_idle_cursor`
  manage the cursor.
- `src/femtovg_area/imp.rs` — `FemtoVgAreaMut` is the renderer state:
  `drawables: Vec<Stacked>`, `undo_stack: Vec<UndoAction>`, `redo_stack`,
  `next_drawable_id`. `commit / modify / modify_many / delete /
  delete_many / undo / redo / reset / hit_test / drawables_in_rect /
  all_drawable_ids` are the public surface. `render` is the draw loop.
- `src/femtovg_area/mod.rs` — `FemtoVGArea` widget wrapper + `impl
  DrawableStore for FemtoVGArea`.
- `src/tools/mod.rs` — `Drawable` trait, `Tool` trait, `Handle` /
  `HandleId`, `bbox_handles`, `bbox_resize` shared helpers, `UndoAction`,
  `ToolUpdateResult`, `DrawableStore` trait, `SELECTION_BLUE` /
  `GLOW_COLOR` / `GLOW_STROKE_WIDTH` / `GLOW_STROKE_WIDTH_WIDE` constants.
- `src/tools/pointer.rs` — selection state machine. The big one.
- `src/tools/arrow.rs` — four arrow variants + Bezier control handle.
- `src/tools/{rectangle,ellipse,line,brush,highlight,marker,blur,text}.rs`
  — per-shape Drawable impls.
- `src/ui/toolbars.rs` — `ToolsToolbar` (top), `StyleToolbar` (bottom,
  has 6 size buttons + arrow dropdown + tool-aware visibility).
- `src/style.rs` — `Style { color, size, fill, annotation_size_factor }`,
  `Size` enum (6 variants), `to_*` scale methods.

## Design preferences (user-stated, captured for context)

- **No bounding box on selection** — handles + glow only, no enclosing
  rectangle.
- **Glow must be entirely outside the shape** — semi-transparent blue
  halo "surrounding" the shape, never on top.
- **Arrows have endpoint handles** — not 8-corner bbox handles. Curved
  and Double arrows additionally have a mid-curve `Control` handle.
- **Click-off only deselects** — does NOT also start drawing a new
  shape; second click is needed.
- **Click on existing shape consumes the click** — Text won't create a
  new box on top of a selected text, Marker won't drop a number on a
  selected drawable.
- **Implicit selection mode** — clicking an existing shape with any tool
  active selects it (toolbar doesn't change). Empty canvas + drawing tool
  starts a new shape.
- **Toolbar style affects current selection** — picking a new color/size
  with a selection updates the selected shape (undoable).
- **Crosshair cursor on drawing tools** — over empty canvas. `grab` over
  shapes, resize cursors over handles, default arrow for Pointer/Crop.
- **Standard arrow geometry** — solid filled, tapered tail (small back
  stub, not a true point), rounded-corner triangle head with a slight
  forward-slanting shoulder where head meets tail.
- **Markers** — solid filled discs, no outer ring; smaller for the
  Small/X-Small sizes.

## How to test

Take a screenshot with `grim` (or any tool that produces a PNG) and feed
it to the dev binary:

```sh
grim /tmp/satty-test.png
/home/jon/Code/satty/target/debug/satty -f /tmp/satty-test.png
```

Useful flows:
1. Draw shapes (Z = Arrow, R = Rectangle, etc.) → switch tools — selection
   should follow you implicitly when you click an existing shape.
2. Drag from empty canvas with the Pointer tool selected → marquee
   selects intersected shapes.
3. Ctrl+A → select all → Backspace → Ctrl+Z → all restored.
4. Pick an arrow style from the dropdown → draw — should match the chosen
   geometry. Curved/Double should have a draggable middle handle.
5. Pick S/M/L/XL/XXL — sizes should look as expected.

## Branch info

- Repo: `/home/jon/Code/satty`
- Branch: `feature/movable-annotations`
- Upstream: `origin/master` (unpushed)
- The user has explicitly chosen to keep all four design stages on a
  single branch; no PR yet.

## When you pick this back up

1. Read this file.
2. `cd /home/jon/Code/satty && git status` — confirm branch.
3. `cargo build` — should be clean.
4. Pick the next pending item (tooltip reliability is small if you want
   a warmup; arrow icons or pen cursor are the most user-visible
   remaining; multi-drag move is the most architecturally interesting).
5. Don't forget to test in the running binary — type-check is necessary
   but not sufficient for UI work.
