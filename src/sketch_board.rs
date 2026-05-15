use anyhow::anyhow;

use femtovg::imgref::Img;
use femtovg::rgb::{ComponentBytes, RGBA};
use keycode::{KeyMap, KeyMappingId};
use relm4::gtk::gdk_pixbuf::Pixbuf;
use relm4::gtk::gdk_pixbuf::glib::Bytes;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::panic;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::{fs, io};

use gtk::prelude::*;

use relm4::gtk::gdk::{DisplayManager, Key, ModifierType, Texture};
use relm4::{Component, ComponentParts, ComponentSender, RelmWidgetExt, gtk};

use crate::configuration::{APP_CONFIG, Action};
use crate::femtovg_area::FemtoVGArea;
use crate::ime::pango_adapter::spans_from_pango_attrs;
use crate::math::Vec2D;
use crate::notification::log_result;
use crate::style::Style;
use crate::tools::{
    Drawable, DrawableId, DrawableStore, HandleId, Tool, ToolEvent, ToolUpdateResult, Tools,
    ToolsManager,
};
use crate::ui::toolbars::ToolbarEvent;
use xdg::BaseDirectories;

type RenderedImage = Img<Vec<RGBA<u8>>>;
const SAVE_AS_LAST_DIR_FILE: &str = "save_as_last_dir";
const SAVE_AS_LAST_DIR_MAX_BYTES: u64 = 10_000;

#[derive(Debug, Clone)]
pub enum SketchBoardInput {
    InputEvent(InputEvent),
    ToolbarEvent(ToolbarEvent),
    RenderResult(RenderedImage, Vec<Action>),
    RenderResultFollowup(Option<Pixbuf>, Vec<Action>, Option<String>),
    CommitEvent(TextEventMsg),
    Refresh,
    Exit,
    ScaleFactorChanged,
    /// The renderer reports its current effective scale_factor whenever it
    /// changes. We forward this as a `ZoomChanged` output so the
    /// zoom-indicator widget can stay in sync with scroll-wheel zooms.
    ZoomDisplayChanged(f32),
    /// Renderer reports its current pan state after every
    /// `update_transformation` so the visible scrollbars can sync
    /// (visibility + position).
    PanDisplayChanged(PanInfo),
    /// User dragged one of the canvas scrollbars. The bool is true
    /// for the horizontal scrollbar, false for vertical. The f32 is
    /// the new adjustment value (canvas pixels of scroll offset
    /// from the top/left of the scaled image).
    ScrollbarSet(bool, f32),
    /// Trackpad pinch (`GestureZoom`) per-frame multiplicative zoom
    /// factor (1.0 = no change, >1 = spread/zoom-in, <1 = pinch/
    /// zoom-out). Computed by the gesture closure from the
    /// absolute scale GTK reports, divided by the last observed
    /// gesture scale, so we feed `set_zoom_scale` its expected
    /// multiplicative delta.
    PinchZoom(f32),
    /// User interaction with the zoom-indicator dropdown.
    ZoomCommand(ZoomCommand),
    /// Force keyboard focus back onto the canvas. Sent from App at
    /// startup and after popovers/dialogs close so single-key shortcuts
    /// work without the user having to click on the canvas first.
    FocusCanvas,
    /// Sent by the CropTool on Esc when the user wants to leave Crop
    /// mode entirely. SketchBoard translates this to a
    /// `ToolSelected(tool_before_crop or Pointer)` dispatch so the
    /// CropTool itself doesn't have to know which tool was active
    /// before the user switched into Crop.
    ExitCropToPreviousTool,
    /// Mirror the current `style.fill` out to the StyleToolbar after
    /// a programmatic toggle (the `F` keyboard shortcut routes
    /// through `ToolbarEvent::ToggleFill`, which updates
    /// `style.fill` but doesn't touch the toolbar's mirror — that's
    /// done lazily on button click; this sync signal closes the loop).
    SyncFillToToolbar,
    /// External components (the welcome dialog, Preferences) pushed a
    /// new `annotation_size_factor`. Update `self.style` so the next
    /// drawn shape stamps the new factor, then re-broadcast the active
    /// style to every tool so cursors / in-progress strokes resize too.
    SetAnnotationFactor(f32),
    /// Toggle the layer panel's visibility. Fired by the configurable
    /// `layer-panel-shortcut` chord (default `ctrl+l`) and the layers
    /// toolbar button. Triggers a panel-row rebuild on open so the panel
    /// reflects the current stack even if it was opened mid-session.
    ToggleLayerPanel,
    /// Panel row clicked. `additive` is true when Ctrl was held — the
    /// existing selection is preserved and this id is toggled in/out
    /// of it. False replaces selection with just this id.
    PanelSelectDrawable {
        id: DrawableId,
        additive: bool,
    },
    PanelToggleVisible(DrawableId),
    PanelToggleLocked(DrawableId),
    /// Panel reorder buttons (Front/Up/Down/Back) — explicit id form.
    PanelMoveDrawable {
        id: DrawableId,
        direction: PanelMoveDir,
    },
    /// Panel footer reorder buttons act on the *current selection*. With
    /// multiple ids selected, they apply per-id in an order chosen so
    /// adjacent moves don't fight each other (top-down for ToTop/Up,
    /// bottom-up for ToBottom/Down).
    PanelMoveSelected(PanelMoveDir),
    /// Drag-to-reorder dropped: commit `new_order`.
    PanelReorderTo(Vec<DrawableId>),
    /// Convenience drop event: insert `src` immediately above (panel
    /// terms) or below `target`. Sketch_board computes the resulting
    /// full order so the row builder stays ignorant of stack state.
    /// `above_target` is true when the cursor is in the top half of
    /// the target row at drop time (insert above in the panel = move
    /// HIGHER in the back-to-front stack = MORE FORWARD in the canvas).
    PanelDropOnto {
        src: DrawableId,
        target: DrawableId,
        above_target: bool,
    },
    /// User clicked the color swatch on a row. Sketch_board opens a
    /// `gtk::ColorDialog` and applies the picked color to the drawable
    /// via the standard Modify path.
    PanelEditColor(DrawableId),
    /// User picked a new color from the dialog. Applied as a regular
    /// `Modify` so undo treats it like any other style change.
    PanelSetColor {
        id: DrawableId,
        color: crate::style::Color,
    },
    /// User submitted a new custom name (Entry's `activate` signal).
    /// Empty / whitespace-only strings clear the custom name and fall
    /// back to the auto-generated label.
    PanelRename {
        id: DrawableId,
        name: String,
    },
    /// `gtk::Paned`'s `position` property changed (already clamped).
    /// Updates the cached width and persists to state.toml.
    LayerPanelWidthChanged(f32),
    /// Ctrl+V resolved a clipboard read into a Pixbuf. Sketch_board
    /// builds a `PastedImage` drawable at a sensible spot and commits
    /// it to the stack so it shows up as a layer + selectable shape.
    PasteImageFromClipboard(relm4::gtk::gdk_pixbuf::Pixbuf),
    Output(SketchBoardOutput),
}

/// Direction parameter for `PanelMoveDrawable`. Each variant maps to one
/// of the four reorder buttons.
#[derive(Debug, Clone, Copy)]
pub enum PanelMoveDir {
    /// Bring all the way to the top.
    ToTop,
    /// One position toward the top.
    Up,
    /// One position toward the bottom.
    Down,
    /// Send all the way to the bottom.
    ToBottom,
}

/// Multi-select agreement for the brush-smoothness slider. The slider
/// has three states unlike the size slider's binary shared/mixed:
///
/// - `NotApplicable` — at least one selected drawable doesn't carry a
///   `smooth_level` (i.e., the selection includes a non-brush). Hide
///   the slider entirely.
/// - `Shared(v)` — every selected drawable is a brush AND they all
///   have the same level. Show the slider, reflect `v`, allow drag.
/// - `Mixed` — every selected drawable is a brush BUT levels differ.
///   Show the slider but disable it so a stray drag can't collapse
///   the group onto a single value.
#[derive(Debug, Clone, Copy)]
pub enum SmoothLevelMulti {
    NotApplicable,
    Shared(usize),
    Mixed,
}

#[derive(Debug, Clone, Copy)]
pub enum ZoomCommand {
    /// Multiplicative zoom-in by `zoom_factor` from the configuration.
    In,
    /// Multiplicative zoom-out by `1 / zoom_factor`.
    Out,
    /// Reset to auto-fit (scale_factor recomputed from canvas/image dims).
    FitCanvas,
    /// Set absolute scale factor (1.0 = 100%, 0.5 = 50%, 2.0 = 200%).
    Abs(f32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PanInfo {
    /// Current accumulated pan offset in canvas pixels (signed:
    /// positive moves the image down/right within the canvas).
    pub drag_x: f32,
    pub drag_y: f32,
    /// Image dimensions multiplied by the current scale_factor.
    /// Comparing against canvas_* tells the scrollbar whether to show.
    pub image_w_scaled: f32,
    pub image_h_scaled: f32,
    pub canvas_w: f32,
    pub canvas_h: f32,
}

#[derive(Debug, Clone)]
pub enum SketchBoardOutput {
    ToggleToolbarsDisplay,
    ToolSwitchShortcut(Tools),
    ColorSwitchShortcut(u64),
    DimensionsUpdate(Option<(i32, i32)>),
    /// Current rendered scale factor (1.0 = 100%, 0.5 = 50%, etc.) whenever
    /// it changes — driven by the renderer after every `update_transformation`.
    ZoomChanged(f32),
    /// Reports whether a crop is currently present on the canvas
    /// (in either edit mode or committed/zoomed mode). Drives the
    /// "Revert to Original" button's visibility in the bottom
    /// toolbar.
    CropPresenceChanged(bool),
    /// Pan state changed — drives the visible scrollbars' adjustment
    /// values and their show/hide based on whether the image
    /// currently exceeds the canvas on each axis.
    PanChanged(PanInfo),
    /// The single selected drawable changed (or its style mutated) —
    /// emitted so the StyleToolbar's size slider, color chip, fill
    /// toggle, etc., follow whatever shape is currently picked.
    /// `None` means selection has been cleared (nothing selected, or
    /// multi-select — see also `SelectionMultiAgreement`) — the
    /// toolbar then keeps its last single-select value.
    SelectionStyleChanged(Option<Style>),
    /// Per-property "do all the multi-selected drawables share this
    /// value?" report. Emitted whenever a multi-selection is live or
    /// changes. For `size`: `Some(v)` means all selected agree on
    /// `v` (toolbar reflects + enables group-edit), `None` means
    /// they disagree (toolbar disables the slider). For smoothness:
    /// see `SmoothLevelMulti` — the slider also has a "no brushes
    /// in selection, hide entirely" state that `size` doesn't.
    SelectionMultiAgreement {
        size: Option<crate::style::Size>,
        smooth: SmoothLevelMulti,
    },
    /// Sketch board changed the active tool's size programmatically
    /// (e.g. Ctrl+wheel over the canvas with no selection). The
    /// toolbar mirrors the new value into its slider — no SizeSelected
    /// re-emit, because sketch_board already pushed the size to the
    /// active tool via `dispatch_style_change`.
    ToolSizeChanged(crate::style::Size),
    /// The intrinsic size of what's currently displayed on the canvas
    /// changed — emitted on initial layout, crop commit (cropped
    /// region dims), re-enter of crop edit mode (full image dims), and
    /// revert (full image dims). Main resizes the window to (size +
    /// padding) capped to 90 % of the display so the canvas can
    /// render the content at 1:1 whenever it fits.
    ContentSizeChanged { width: f32, height: f32 },
    /// Underlying background-image dimensions changed (startup
    /// seed + every rotate / resize action from the crop-mode top
    /// toolbar). App forwards into ToolsToolbar so its
    /// "Image size: W × H px" label and resize-popover entries
    /// reflect the live value.
    ImageDimensionsChanged { width: i32, height: i32 },
    /// The global Fill-Shape state was toggled from outside the
    /// StyleToolbar (currently: the `F` keyboard shortcut).
    /// Routed through to the toolbar so its icon + tooltip
    /// reflect the new value without the user clicking the
    /// button manually.
    FillShapesChanged(bool),
    /// Live crop-rect dimensions during a drag / typed set. Used
    /// to refresh the crop-mode toolbar's W/H entries WITHOUT
    /// touching the bottom-right output-dimensions readout — the
    /// readout reflects the OUTPUT (full image while editing,
    /// cropped size only after commit) so it doesn't visually
    /// thrash on every drag tick.
    CropEditDimensions { width: i32, height: i32 },
    /// User wants to open the Preferences dialog (gear button or
    /// Ctrl+,). The dialog isn't a child of sketch_board, so we
    /// just forward the intent up to App.
    OpenPreferences,
    /// User clicked the "?" help button next to the annotation
    /// size factor row in Preferences. App re-launches the first-
    /// run welcome dialog so the user can re-read the explanation
    /// and re-pick the factor through the same UI as initial setup.
    OpenWelcomeDialog,
    /// Prefs spin button reported a new annotation size factor.
    /// App centralizes the persist + cross-update (push the new
    /// value into the welcome dialog's spin if it's open) so both
    /// surfaces stay in lockstep without each dialog knowing about
    /// the other.
    AnnotationFactorChanged(f32),
    /// Tool-specific style cycled (double-tap of the tool's
    /// shortcut). Drives the matching StyleToolbar menu/dropdown
    /// so the on-screen affordance keeps up with the variant that
    /// was just promoted in state.toml.
    ArrowStyleCycled(crate::tools::ArrowStyle),
    BlurStyleCycled(crate::tools::BlurStyle),
    TextBackgroundCycled(crate::tools::TextBackground),
    HighlighterStyleCycled(crate::tools::HighlighterStyle),
    /// Announce the just-cycled variant by name (e.g. "Arrow:
    /// Curved"). Caller renders it as a centered toast on the
    /// canvas — separate from the structured style events so the
    /// presentation lives in main.rs / the overlay alongside the
    /// rest of the chrome.
    ShowCycleToast(String),
    /// The selected text drawable's background style — used to
    /// re-seed the StyleToolbar's TextBackground dropdown when the
    /// user clicks between texts with different backgrounds. Silent
    /// path (doesn't re-apply or re-toast); pure UI sync.
    SelectionTextBackgroundChanged(crate::tools::TextBackground),
    /// Same shape for Arrow / Blur — sync the toolbar's
    /// MenuButton preview to the selected drawable's variant so
    /// double-tap / popover-click cycles operate from the
    /// just-selected state.
    SelectionArrowStyleChanged(crate::tools::ArrowStyle),
    SelectionBlurStyleChanged(crate::tools::BlurStyle),
    /// Selection-sync for Brush: the just-selected drawable's
    /// post-stroke smoothing level. Toolbar mirrors it into the
    /// slider (silent path) so the slider matches the annotation
    /// clicked — and so subsequent slider drags re-smooth THAT
    /// annotation rather than fighting an out-of-sync default.
    SelectionBrushPostSmoothChanged(usize),
    /// Tool switch snapped the spotlight-darkness / highlighter-
    /// opacity slider back to the saved default. Toolbar updates
    /// its slider so the on-screen value matches the now-active
    /// style state instead of the previous session's drag.
    SpotlightDarknessReset(f32),
    HighlighterOpacityReset(f32),
    /// Tool switch into Brush snapped the post-stroke smoothing slider
    /// back to the saved default; toolbar updates the slider position
    /// so it matches the now-active APP_CONFIG value.
    BrushPostSmoothReset(usize),
}

#[derive(Debug, Clone)]
pub enum InputEvent {
    Mouse(MouseEventMsg),
    Key(KeyEventMsg),
    KeyRelease(KeyEventMsg),
    Text(TextEventMsg),
}

// from https://flatuicolors.com/palette/au

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
pub enum MouseButton {
    Primary,
    Secondary,
    Middle,
}

#[derive(Debug, Clone, Copy)]
pub struct KeyEventMsg {
    pub key: Key,
    pub code: u32,
    pub modifier: ModifierType,
}
#[derive(Debug, Clone)]
pub enum TextEventMsg {
    Commit(String),
    Preedit {
        text: String,
        cursor_chars: Option<usize>,
        spans: Vec<crate::ime::preedit::PreeditSpan>,
    },
    PreeditEnd,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MouseEventType {
    BeginDrag,
    EndDrag,
    UpdateDrag,
    Click,
    /// A wheel / trackpad scroll event. The delta is packed into
    /// `MouseEventMsg.pos` (`pos.x = dx`, `pos.y = dy`); the input
    /// handler's modifier chain routes it to pan, zoom, resize, or size.
    PanScroll,
    PointerPos,
    Release,
    //Motion(Vec2D),
}

#[derive(Debug, Clone, Copy)]
pub struct MouseEventMsg {
    pub type_: MouseEventType,
    pub button: MouseButton,
    pub modifier: ModifierType,
    /// Image-coord position. For Click/Release/BeginDrag this is an
    /// absolute image-space point; for UpdateDrag/EndDrag it's a
    /// delta in image-space units. `handle_event_mouse_input`
    /// converts the raw widget value into this image-coord form
    /// before dispatch.
    pub pos: Vec2D,
    pub n_pressed: i32,
    pub release: bool,
}

impl SketchBoardInput {
    pub fn new_mouse_event(
        event_type: MouseEventType,
        button: u32,
        n_pressed: i32,
        modifier: ModifierType,
        pos: Vec2D,
        release: bool,
    ) -> SketchBoardInput {
        SketchBoardInput::InputEvent(InputEvent::Mouse(MouseEventMsg {
            type_: event_type,
            button: button.into(),
            n_pressed,
            modifier,
            pos,
            release,
        }))
    }
    pub fn new_key_event(event: KeyEventMsg) -> SketchBoardInput {
        SketchBoardInput::InputEvent(InputEvent::Key(event))
    }

    pub fn new_key_release_event(event: KeyEventMsg) -> SketchBoardInput {
        SketchBoardInput::InputEvent(InputEvent::KeyRelease(event))
    }

    pub fn new_text_event(event: TextEventMsg) -> SketchBoardInput {
        SketchBoardInput::InputEvent(InputEvent::Text(event))
    }

    pub fn new_commit_event(event: TextEventMsg) -> SketchBoardInput {
        SketchBoardInput::CommitEvent(event)
    }

    pub fn new_pan_scroll_event(
        delta_x: f64,
        delta_y: f64,
        modifier: ModifierType,
    ) -> SketchBoardInput {
        SketchBoardInput::InputEvent(InputEvent::Mouse(MouseEventMsg {
            type_: MouseEventType::PanScroll,
            button: MouseButton::Middle,
            n_pressed: 0,
            modifier,
            pos: Vec2D::new(delta_x as f32, delta_y as f32),
            release: false,
        }))
    }

    pub fn new_pinch_zoom_event(factor: f32) -> SketchBoardInput {
        SketchBoardInput::PinchZoom(factor)
    }
}

impl From<u32> for MouseButton {
    fn from(value: u32) -> Self {
        match value {
            gtk::gdk::BUTTON_PRIMARY => MouseButton::Primary,
            gtk::gdk::BUTTON_MIDDLE => MouseButton::Middle,
            gtk::gdk::BUTTON_SECONDARY => MouseButton::Secondary,
            _ => MouseButton::Primary,
        }
    }
}

impl InputEvent {
    fn handle_event_mouse_input(&mut self, renderer: &FemtoVGArea) -> Option<ToolUpdateResult> {
        if let InputEvent::Mouse(me) = self {
            match me.type_ {
                MouseEventType::Click => {
                    me.pos = renderer.abs_canvas_to_image_coordinates(me.pos);
                    None
                }
                MouseEventType::Release => {
                    me.pos = renderer.abs_canvas_to_image_coordinates(me.pos);
                    None
                }
                MouseEventType::BeginDrag => {
                    me.pos = renderer.abs_canvas_to_image_coordinates(me.pos);
                    None
                }
                MouseEventType::EndDrag | MouseEventType::UpdateDrag => {
                    me.pos = renderer.rel_canvas_to_image_coordinates(me.pos);
                    None
                }
                _ => None,
            }
        } else {
            None
        }
    }

    fn handle_mouse_event(&mut self, renderer: &FemtoVGArea) -> Option<ToolUpdateResult> {
        if let InputEvent::Mouse(me) = self {
            match me.type_ {
                MouseEventType::Click => {
                    if me.button == MouseButton::Secondary {
                        renderer.request_render(&APP_CONFIG.read().actions_on_right_click());
                        None
                    } else {
                        None
                    }
                }
                MouseEventType::EndDrag | MouseEventType::UpdateDrag => {
                    if me.button == MouseButton::Middle {
                        renderer.set_drag_offset(me.pos);
                        renderer.set_is_drag(true);

                        if me.type_ == MouseEventType::EndDrag {
                            renderer.store_last_offset();
                            renderer.set_is_drag(false);
                        }
                        renderer.request_render(&[]);
                    }
                    None
                }

                MouseEventType::PanScroll => {
                    // GTK reports scroll deltas pre-corrected for the
                    // OS's natural-scrolling preference (natural-on
                    // inverts at the compositor). Apply them directly
                    // to `drag_offset` so the canvas follows the
                    // user's finger / wheel motion — for a trackpad
                    // that means swipe down moves the canvas down,
                    // matching how every other Wayland app behaves.
                    const SCROLL_PAN_PIXELS: f32 = 48.0;
                    renderer.pan_by(
                        me.pos.x * SCROLL_PAN_PIXELS,
                        me.pos.y * SCROLL_PAN_PIXELS,
                    );
                    // pan_by mutates drag_offset and emits the new
                    // PanInfo so the scrollbars track, but it doesn't
                    // queue a redraw on its own. Return Redraw so the
                    // dispatch loop calls refresh_screen — otherwise
                    // the scrollbar appears to track but the image
                    // itself stays put until the next unrelated event.
                    Some(ToolUpdateResult::Redraw)
                }
                MouseEventType::PointerPos => {
                    renderer.set_pointer_offset(me.pos);
                    None
                }
                _ => None,
            }
        } else {
            None
        }
    }
}

/// Advance a value within a fixed-order cycle by `steps` positions
/// (wrapping at both ends). Used by the alt-slider wheel handler to
/// step the arrow / blur / text-background dropdowns from their
/// current value to a neighbouring one in a single pass — wraps so a
/// fast wheel flick that overshoots still lands on a real variant.
/// Returns the unchanged input if it isn't in the order (defensive —
/// shouldn't happen in practice).
fn wrap_cycle<T: Copy + PartialEq>(order: &[T], current: T, steps: i32) -> T {
    let Some(cur_idx) = order.iter().position(|x| *x == current) else {
        return current;
    };
    let n = order.len() as i32;
    if n <= 0 {
        return current;
    }
    let new_idx = (cur_idx as i32 + steps).rem_euclid(n) as usize;
    order[new_idx]
}

/// Apply `steps` discrete bumps to a `Size`, positive = step_up. Used
/// by the scroll-wheel resize gestures so a single multi-step swipe
/// (or even a wraparound from a fast trackpad flick) lands on the
/// right rung in one pass.
fn apply_size_steps(mut size: crate::style::Size, steps: i32) -> crate::style::Size {
    if steps > 0 {
        for _ in 0..steps {
            size = size.step_up();
        }
    } else if steps < 0 {
        for _ in 0..(-steps) {
            size = size.step_down();
        }
    }
    size
}

pub struct SketchBoard {
    renderer: FemtoVGArea,
    active_tool: Rc<RefCell<dyn Tool>>,
    tools: ToolsManager,
    style: Style,
    im_context: gtk::IMMulticontext,
    last_saved_filepath: RefCell<Option<String>>,
    /// Last (selected drawable id, size, size-factor) tuple we pushed
    /// to the toolbar via `SelectionStyleChanged`. We re-emit when
    /// any of these change — flips of the active selection AND
    /// mutations of the currently selected shape's sizing (e.g. the
    /// scroll-wheel resize gesture) — so the size slider stays in
    /// sync without re-emitting on every redraw.
    last_synced_selection: Option<(DrawableId, crate::style::Size)>,
    /// `true` while the previous sync saw a multi-selection (2+ ids).
    /// Read alongside `last_synced_selection` so multi→empty and
    /// multi→single transitions emit `SelectionStyleChanged` even
    /// when the cache key alone doesn't change (multi and empty both
    /// produce `None` in `last_synced_selection`).
    last_was_multi_selection: bool,
    /// The tool that was active just before the user switched into
    /// Crop. Captured in `handle_toolbar_event` and used by the Esc
    /// path in `CropTool` to return the user to where they were
    /// rather than dropping them on Pointer. `None` means we haven't
    /// recorded anything yet (initial app state) — the fallback is
    /// Pointer in that case.
    tool_before_crop: Option<Tools>,
    /// Accumulator for the scroll-resize gesture (selection-wheel and
    /// Ctrl+wheel). A notched mouse wheel reports |dy| = 1.0 per
    /// click so a step fires every event, but trackpads emit many
    /// small fractional deltas — we add them up and only step the
    /// size when |accum| crosses 1.0, then subtract the consumed
    /// portion. Reset on direction reversal so a flick the other way
    /// doesn't have to chew through the previous direction's leftover.
    scroll_resize_accum: f32,
    /// Last tool-shortcut keypress (the single char + when it fired).
    /// Used to detect a double-tap of the same tool key within
    /// `TOOL_CYCLE_MS` so the press cycles the tool's style instead
    /// of just re-selecting the same tool. The first press always
    /// behaves as a normal select; only the SECOND quick press
    /// cycles, so the user can't accidentally change variants by
    /// pressing the same key once.
    last_tool_press: Option<(char, std::time::Instant)>,
    /// Last image-space pointer position seen by `update_hover_cursor`.
    /// Stashed so events that don't carry a pointer position (zoom
    /// change, tool switch) can refresh the cursor by re-running the
    /// band-aware path at the last-known location rather than falling
    /// back to a style-only cursor. Without this, zooming with the
    /// pointer over a text row would briefly render the cursor at the
    /// style-derived size + pointer-anchored position until the user
    /// nudged the mouse and the next motion event re-detected.
    last_hover_image_pos: Option<crate::math::Vec2D>,
    /// In-session memory for highlighter opacity. When the user has
    /// `sticky_session_defaults` on, re-entering Highlighter restores
    /// this value instead of `state::load_highlighter_opacity()`.
    /// `None` = user hasn't touched the opacity slider this session
    /// yet — fall back to the persisted default. Single Option (not
    /// a per-tool map) because only one tool uses opacity.
    session_highlighter_opacity: Option<f32>,
    /// Same shape for the brush post-stroke smoothing slider. Only
    /// the Brush tool consumes this, so a single Option suffices.
    session_brush_smooth: Option<usize>,
    /// In-session memory for per-tool fill state. Keyed by `Tools`
    /// (only Rectangle / Ellipse populate entries today). Used by
    /// the `switch_active_tool` snap-back path when
    /// `sticky_session_defaults` is on; a missing entry falls back
    /// to `state::load_fill_for_tool` (the saved default).
    session_fill_per_tool: HashMap<Tools, bool>,
    layer_panel_open: bool,
    /// Vertical Box of layer rows. Owned directly here so the rebuild
    /// helper can clear+repopulate it on every drawable-stack change.
    /// Lives inside `layer_panel_paned`'s start_child slot.
    layer_panel_content: gtk::Box,
    /// Cached input sender so panel row builders can forward click
    /// events without threading `ComponentSender` through every helper.
    layer_panel_sender: Option<relm4::Sender<SketchBoardInput>>,
    /// Parsed `layer_panel_shortcut` from APP_CONFIG, captured at init.
    /// `None` when the configured string didn't parse — in that case
    /// only the toolbar button can toggle the panel.
    layer_panel_shortcut: Option<(gtk::gdk::Key, ModifierType)>,
    /// Horizontal `gtk::Paned` whose start_child slot hosts the layer
    /// panel content. The user resizes the panel by dragging Paned's
    /// built-in separator — which lives on a stable widget (not the
    /// panel itself), so the cursor doesn't oscillate the way an
    /// edge-of-panel custom handle did.
    layer_panel_paned: gtk::Paned,
    /// Current pixel width of the panel content. Mirrors the Paned's
    /// `position`. Persisted to state.toml on every change so re-open
    /// puts the divider back where the user left it.
    layer_panel_width: f32,
}

/// Minimum/maximum widths the resize handle will allow (image-coord px).
/// The minimum keeps the row labels readable; the maximum keeps the
/// canvas usable even on small displays.
const LAYER_PANEL_MIN_WIDTH: f32 = 110.0;
const LAYER_PANEL_MAX_WIDTH: f32 = 480.0;
/// Default panel width when no persisted value exists. Tuned narrower
/// than the original 220px after the "too wide" feedback.
const LAYER_PANEL_DEFAULT_WIDTH: f32 = 140.0;

/// Max gap (ms) between two presses of the same tool-shortcut key
/// for the second press to register as a "cycle" instead of a
/// re-selection. Tuned tight so an inadvertent double-press still
/// reads as the user intentionally drumming.
const TOOL_CYCLE_MS: u64 = 500;

impl SketchBoard {
    fn refresh_screen(&mut self) {
        // Rebuild the layer panel BEFORE queuing the canvas render so the
        // panel's row list reflects the same drawable stack the next
        // paint will draw. No-ops when the panel is closed.
        self.rebuild_layer_panel_rows_if_open();
        self.renderer.queue_render();
    }

    /// If the layer panel is open, throw away its current rows and rebuild
    /// from `renderer.all_drawable_ids()` in top-of-stack-first order.
    ///
    /// Each row carries the drawable's tool icon, a color swatch derived
    /// from its `style().color` (if any), and an auto-generated label like
    /// "Rectangle 3" — the ordinal counts occurrences of that kind in
    /// stacking order, so the first rectangle gets "Rectangle 1" and so
    /// on. Rebuild-from-scratch is intentional: with the row count likely
    /// in the tens and the panel hidden by default, the simplicity of
    /// "clear and re-append" outweighs any incremental-update cost.
    fn rebuild_layer_panel_rows_if_open(&mut self) {
        if !self.layer_panel_open {
            return;
        }
        let Some(sender) = self.layer_panel_sender.clone() else {
            return;
        };
        // Clear existing rows + footer.
        while let Some(child) = self.layer_panel_content.first_child() {
            self.layer_panel_content.remove(&child);
        }

        let ids = self.renderer.all_drawable_ids();
        let selected_set: std::collections::HashSet<DrawableId> = self
            .tools
            .get(&Tools::Pointer)
            .borrow()
            .selected_drawables()
            .into_iter()
            .collect();

        // Auto labels use the stable `auto_label_index` stored on each
        // Stacked at commit time. That index never shifts, so reorders
        // don't renumber rows ("Rectangle 3" stays "Rectangle 3" even
        // after being dragged to the top). Numbers can have gaps after
        // deletions; the user can rename to clean those up.
        let mut labels: HashMap<crate::tools::DrawableId, String> = HashMap::new();
        for id in &ids {
            if let Some(d) = self.renderer.clone_drawable(*id) {
                let n = self
                    .renderer
                    .drawable_auto_label_index(*id)
                    .unwrap_or(0);
                labels.insert(*id, format!("{} {n}", d.panel_label_kind()));
            }
        }

        // Render top-of-stack first. `ids` is back-to-front, so reverse.
        for id in ids.into_iter().rev() {
            let Some(d) = self.renderer.clone_drawable(id) else {
                continue;
            };
            let auto_label = labels.remove(&id).unwrap_or_else(|| "Layer".into());
            // Custom name overrides the auto label entirely.
            let label = self
                .renderer
                .drawable_custom_name(id)
                .unwrap_or(auto_label);
            let (visible, locked) = self
                .renderer
                .drawable_flags(id)
                .unwrap_or((true, false));
            let row = build_layer_panel_row(
                LayerRowData {
                    id,
                    icon_name: d.icon_name(),
                    preview: d.panel_preview(),
                    swatch: d.panel_swatch(),
                    label: &label,
                    selected: selected_set.contains(&id),
                    visible,
                    locked,
                },
                sender.clone(),
            );
            self.layer_panel_content.append(&row);
        }

        self.layer_panel_content
            .append(&build_layer_panel_footer(sender));
    }

    /// Hook called after `commit / modify / modify_many / delete /
    /// delete_many` so the canvas re-fits around the original
    /// screenshot plus the current drawable bounds — grows when a
    /// drawable spills past, shrinks back toward the original when
    /// the last spilling drawable is gone. Skipped while the Crop
    /// tool is active (crop is for shrinking — auto-extending
    /// against the crop edit would fight the user). When the renderer
    /// reports a resize, refresh the crop tool's bounds and emit the
    /// dimensions-changed events so the toolbar label and main
    /// window resize around the new content.
    fn auto_resize_canvas(
        &mut self,
        ids_to_exclude: &[crate::tools::DrawableId],
        sender: &ComponentSender<Self>,
    ) {
        if self.active_tool_type() == Tools::Crop {
            return;
        }
        let Some((new_w, new_h)) =
            self.renderer.auto_resize_for_drawables(ids_to_exclude)
        else {
            return;
        };
        let crop_tool = self.tools.get_crop_tool();
        crop_tool
            .borrow_mut()
            .set_image_bounds(crate::math::Vec2D::new(new_w, new_h));
        // Drop any manual zoom + pan so the renderer's auto-fit
        // branch engages on the next frame. The window is about to
        // grow via ContentSizeChanged but is capped at 90 % of the
        // monitor by `window_size_for_content`; once the image
        // outgrows that cap the auto-fit needs to take over and
        // scale it down so the full canvas stays in view. With a
        // non-zero `zoom_scale` the auto-fit branch is skipped, so
        // the extended image would overflow the window.
        self.renderer.reset_size(0.0);
        sender
            .output_sender()
            .emit(SketchBoardOutput::ImageDimensionsChanged {
                width: new_w as i32,
                height: new_h as i32,
            });
        sender
            .output_sender()
            .emit(SketchBoardOutput::ContentSizeChanged {
                width: new_w,
                height: new_h,
            });
        // The window resize triggered by `ContentSizeChanged` can
        // bounce focus off the canvas — GTK may reassign focus to
        // the first focusable child of the toplevel after a relayout.
        // Pull it back so single-key shortcuts keep working after
        // (e.g.) committing a text annotation that pushed past the
        // edge and triggered auto-extend.
        self.renderer.grab_focus();
    }

    fn image_to_pixbuf(image: RenderedImage) -> Pixbuf {
        let (buf, w, h) = image.into_contiguous_buf();

        Pixbuf::from_bytes(
            &Bytes::from(buf.as_bytes()),
            relm4::gtk::gdk_pixbuf::Colorspace::Rgb,
            true,
            8,
            w as i32,
            h as i32,
            w as i32 * 4,
        )
    }

    fn deactivate_active_tool(&mut self) -> bool {
        if !self.active_tool.borrow().active() {
            return false;
        }
        match self.active_tool.borrow_mut().handle_deactivated() {
            ToolUpdateResult::Commit(result) => {
                self.renderer.commit(result);
                true
            }
            // TextTool emits ModifyDrawable when handle_deactivated
            // finalizes a re-edit (edit_target_id set). Replace the
            // existing drawable in-place rather than appending a new one.
            ToolUpdateResult::ModifyDrawable(id, result) => {
                self.renderer.modify(id, result);
                true
            }
            _ => false,
        }
    }

    fn handle_action(&mut self, actions: &[Action]) -> ToolUpdateResult {
        let rv = if self.deactivate_active_tool() {
            ToolUpdateResult::Redraw
        } else {
            ToolUpdateResult::Unmodified
        };
        self.renderer.request_render(actions);
        rv
    }

    fn handle_render_result_with_pixbuf(
        &self,
        pix_buf: Option<Pixbuf>,
        actions: Vec<Action>,
        sender: ComponentSender<Self>,
    ) {
        let mut iter = actions.into_iter();
        let mut early_exit = false;
        while let Some(action) = iter.next() {
            match action {
                Action::CopyFilepathToClipboard => {
                    self.handle_copy_filepath();
                }
                Action::SaveToClipboard => {
                    if let Some(ref pix_buf) = pix_buf {
                        self.handle_copy_clipboard(pix_buf);
                        if !APP_CONFIG.read().auto_copy() {
                            early_exit = APP_CONFIG.read().early_exit();
                        }
                    }
                }
                Action::SaveToFile => {
                    if let Some(ref pix_buf) = pix_buf {
                        self.handle_save(pix_buf);
                        early_exit = APP_CONFIG.read().early_exit();
                    }
                }
                /* SaveToFileAs runs through a callback, so any further actions need to be triggered
                from the callback rather than further iterating actions here */
                Action::SaveToFileAs => {
                    if let Some(pix_buf) = pix_buf {
                        let followup_actions: Vec<Action> = iter.collect();
                        let is_modal =
                            APP_CONFIG.read().early_exit_save_as() || !followup_actions.is_empty();
                        self.handle_save_as(is_modal, pix_buf, sender, followup_actions);
                    }
                    return;
                }
                _ => (),
            }

            if early_exit {
                log_result("Early exit, ignoring further actions.", false);
                self.handle_exit();
                return;
            }
            if action == Action::Exit {
                log_result("Exit action, ignoring further actions.", false);
                self.handle_exit();
                return;
            }
        }
    }

    fn handle_render_result(
        &self,
        image: RenderedImage,
        actions: Vec<Action>,
        sender: ComponentSender<Self>,
    ) {
        let needs_pixbuf = actions.iter().any(|action| {
            matches!(
                action,
                Action::SaveToClipboard | Action::SaveToFile | Action::SaveToFileAs
            )
        });

        let pix_buf = if needs_pixbuf {
            Some(Self::image_to_pixbuf(image))
        } else {
            None
        };

        self.handle_render_result_with_pixbuf(pix_buf, actions, sender);
    }

    fn handle_exit(&self) {
        relm4::main_application().quit();
    }

    fn resolve_output_filename(output_filename: &str) -> Option<String> {
        let delayed_format = chrono::Local::now().format(output_filename);
        let mut output_filename = if panic::catch_unwind(|| delayed_format.to_string()).is_ok() {
            delayed_format.to_string()
        } else {
            eprintln!(
                "Warning: Could not format filename {output_filename} due to chrono format error, falling back to literal filename."
            );
            output_filename.to_owned()
        };

        if let Some(tilde_stripped) =
            output_filename.strip_prefix(&format!("~{}", std::path::MAIN_SEPARATOR_STR))
        {
            if let Some(mut home_dir) = std::env::home_dir() {
                home_dir.push(tilde_stripped);
                output_filename = home_dir.to_string_lossy().into_owned();
            } else {
                log_result(
                    "~ found but could not determine homedir",
                    !APP_CONFIG.read().disable_notifications(),
                );
                return None;
            }
        }

        Some(output_filename)
    }

    fn configured_output_path() -> Option<PathBuf> {
        APP_CONFIG
            .read()
            .output_filename()
            .and_then(|output_filename| {
                if output_filename == "-" {
                    None
                } else {
                    Self::resolve_output_filename(output_filename).map(PathBuf::from)
                }
            })
    }

    fn save_as_last_dir_file() -> Option<PathBuf> {
        let dirs = BaseDirectories::with_prefix(env!("CARGO_PKG_NAME"));
        dirs.get_state_file(SAVE_AS_LAST_DIR_FILE)
    }

    fn save_as_last_dir_file_for_write() -> Option<PathBuf> {
        let dirs = BaseDirectories::with_prefix(env!("CARGO_PKG_NAME"));
        dirs.place_state_file(SAVE_AS_LAST_DIR_FILE).ok()
    }

    fn save_as_initial_dir(
        last_dir_file: Option<&Path>,
        configured_output_path: Option<&Path>,
    ) -> Option<PathBuf> {
        if let Some(last_dir_file) = last_dir_file
            && fs::metadata(last_dir_file).is_ok_and(|metadata| {
                metadata.is_file() && metadata.len() <= SAVE_AS_LAST_DIR_MAX_BYTES
            })
            && let Ok(last_dir) = fs::read_to_string(last_dir_file)
        {
            let last_dir = PathBuf::from(last_dir);
            if last_dir.is_dir() {
                return Some(last_dir);
            }
        }

        configured_output_path
            .and_then(Path::parent)
            .filter(|parent| parent.is_dir())
            .map(Path::to_path_buf)
    }

    fn remember_save_as_dir(output_filename: &Path) {
        let Some(last_dir_file) = Self::save_as_last_dir_file_for_write() else {
            return;
        };
        Self::write_save_as_last_dir(&last_dir_file, output_filename);
    }

    fn write_save_as_last_dir(last_dir_file: &Path, output_filename: &Path) {
        let Some(parent) = output_filename.parent() else {
            return;
        };

        let _ = fs::write(last_dir_file, parent.to_string_lossy().as_bytes());
    }

    fn handle_save(&self, image: &Pixbuf) {
        let output_filename = match APP_CONFIG.read().output_filename() {
            None => {
                println!("No Output filename specified!");
                return;
            }
            Some(o) => o.clone(),
        };

        let Some(output_filename) = Self::resolve_output_filename(&output_filename) else {
            return;
        };

        // TODO: we could support more data types
        if output_filename != "-" && !output_filename.ends_with(".png") {
            log_result(
                "The only supported format is png, but the filename does not end in png",
                !APP_CONFIG.read().disable_notifications(),
            );
            return;
        }

        let data = match image.save_to_bufferv("png", &Vec::new()) {
            Ok(d) => d,
            Err(e) => {
                println!("Error serializing image: {e}");
                return;
            }
        };

        if output_filename == "-" {
            // "-" means stdout
            let stdout = io::stdout();
            let mut handle = stdout.lock();
            if let Err(e) = handle.write_all(&data) {
                eprintln!("Error writing image to stdout: {e}");
            }
            return;
        }
        match fs::write(&output_filename, data) {
            Err(e) => log_result(
                &format!("Error while saving file: {e}"),
                !APP_CONFIG.read().disable_notifications(),
            ),
            Ok(_) => {
                // Store the filepath for copy-filepath action
                *self.last_saved_filepath.borrow_mut() = Some(output_filename.clone());
                log_result(
                    &format!("File saved to '{}'.", &output_filename),
                    !APP_CONFIG.read().disable_notifications(),
                )
            }
        };
    }

    fn handle_save_as(
        &self,
        is_modal: bool,
        pixbuf: Pixbuf,
        sender: ComponentSender<Self>,
        followup_actions: Vec<Action>,
    ) {
        let configured_output_path = Self::configured_output_path();
        let initial_dir = Self::save_as_initial_dir(
            Self::save_as_last_dir_file().as_deref(),
            configured_output_path.as_deref(),
        );
        let suggested_filename = configured_output_path
            .as_deref()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned());

        let data = match pixbuf.save_to_bufferv("png", &Vec::new()) {
            Ok(d) => d,
            Err(e) => {
                println!("Error serializing image: {e}");
                return;
            }
        };

        let root = self.renderer.toplevel_window();

        relm4::spawn_local(async move {
            let builder = gtk::FileChooserNative::builder()
                .modal(is_modal)
                .title("Save Image As")
                .action(gtk::FileChooserAction::Save)
                .accept_label("Save")
                .cancel_label("Cancel");

            let dialog = match root {
                Some(w) => builder.transient_for(&w),
                None => builder,
            }
            .build();

            if let Some(initial_dir) = initial_dir {
                let initial_dir = gtk::gio::File::for_path(initial_dir);
                if let Err(e) = dialog.set_current_folder(Some(&initial_dir)) {
                    eprintln!("Error setting Save As folder: {e}");
                }
            }

            if let Some(filename) = suggested_filename {
                dialog.set_current_name(&filename);
            }

            dialog.connect_response(move |dialog, response| {
                let mut exit_app = false;
                let mut filename: Option<String> = None;
                if response == gtk::ResponseType::Accept
                    && let Some(file) = dialog.file()
                {
                    let output_filename = match file.path() {
                        Some(path) => path.to_string_lossy().into_owned(),
                        None => return,
                    };

                    match fs::write(&output_filename, &data) {
                        Err(e) => log_result(
                            &format!("Error while saving file: {e}"),
                            !APP_CONFIG.read().disable_notifications(),
                        ),
                        Ok(_) => {
                            exit_app = APP_CONFIG.read().early_exit_save_as();
                            filename = Some(output_filename.clone());
                            Self::remember_save_as_dir(Path::new(&output_filename));
                            log_result(
                                &format!("File saved to '{}'.", &output_filename),
                                !APP_CONFIG.read().disable_notifications(),
                            )
                        }
                    };
                }
                if exit_app {
                    log_result("early exit after save as, ignoring further actions.", false);
                    sender.input(SketchBoardInput::Exit);
                } else if filename.is_some() || !followup_actions.is_empty() {
                    let followup_actions_clone = followup_actions.clone();
                    let pixbuf_clone = Some(pixbuf.clone());
                    sender.input(SketchBoardInput::RenderResultFollowup(
                        pixbuf_clone,
                        followup_actions_clone,
                        filename,
                    ));
                }
            });

            dialog.show();
        });
    }

    fn save_texture_to_clipboard(&self, texture: &impl IsA<Texture>) -> anyhow::Result<()> {
        let display = DisplayManager::get()
            .default_display()
            .ok_or(anyhow!("Cannot open default display for clipboard."))?;
        display.clipboard().set_texture(texture);

        Ok(())
    }

    fn save_bytes_to_external_process(&self, bytes: &[u8], command: &str) -> anyhow::Result<()> {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()?;

        let child_stdin = child.stdin.as_mut().unwrap();
        child_stdin.write_all(bytes)?;

        if !child.wait()?.success() {
            return Err(anyhow!("Writing to process '{command}' failed."));
        }

        Ok(())
    }

    fn save_texture_to_external_process(
        &self,
        texture: &impl IsA<Texture>,
        command: &str,
    ) -> anyhow::Result<()> {
        self.save_bytes_to_external_process(texture.save_to_png_bytes().as_ref(), command)
    }

    fn handle_copy_clipboard(&self, image: &Pixbuf) {
        let texture = Texture::for_pixbuf(image);

        let result = if let Some(command) = APP_CONFIG.read().copy_command() {
            self.save_texture_to_external_process(&texture, command)
        } else {
            self.save_texture_to_clipboard(&texture)
        };

        match result {
            Err(e) => println!("Error saving {e}"),
            Ok(()) => {
                log_result(
                    "Copied to clipboard.",
                    !APP_CONFIG.read().disable_notifications(),
                );

                // TODO: rethink order and messaging patterns
                if APP_CONFIG.read().save_after_copy() {
                    self.handle_save(image);
                };
            }
        }
    }

    fn copy_text_to_clipboard(&self, text: &str) -> anyhow::Result<()> {
        let display = DisplayManager::get()
            .default_display()
            .ok_or(anyhow!("Cannot open default display for clipboard."))?;
        display.clipboard().set_text(text);
        Ok(())
    }

    fn copy_text_to_external_process(&self, text: &str, command: &str) -> anyhow::Result<()> {
        self.save_bytes_to_external_process(text.as_bytes(), command)
    }

    fn handle_copy_filepath(&self) {
        let filepath = match self.last_saved_filepath.borrow().clone() {
            Some(path) => path,
            None => return,
        };

        // Copy the filepath to clipboard
        let result = if let Some(command) = APP_CONFIG.read().copy_command() {
            self.copy_text_to_external_process(&filepath, command)
        } else {
            self.copy_text_to_clipboard(&filepath)
        };

        match result {
            Err(e) => log_result(
                &format!("Error copying filepath: {e}"),
                !APP_CONFIG.read().disable_notifications(),
            ),
            Ok(()) => log_result(
                &format!("Filepath copied to clipboard: {}", filepath),
                !APP_CONFIG.read().disable_notifications(),
            ),
        }
    }

    /// Kick off an async clipboard read; once a texture comes back,
    /// convert to Pixbuf and route through `PasteImageFromClipboard`
    /// so the actual commit happens back on the model. Read-from-
    /// clipboard is `async` in gtk4-rs; spawning on the local main
    /// context (GLib's single-threaded scheduler) lets us capture an
    /// `input_sender` without Send bounds.
    fn handle_paste_image(&self, sender: &ComponentSender<Self>) {
        let Some(display) =
            relm4::gtk::gdk::DisplayManager::get().default_display()
        else {
            return;
        };
        let clipboard = display.clipboard();
        let input_sender = sender.input_sender().clone();
        relm4::gtk::glib::spawn_future_local(async move {
            match clipboard.read_texture_future().await {
                Ok(Some(texture)) => {
                    if let Some(pixbuf) =
                        relm4::gtk::gdk::pixbuf_get_from_texture(&texture)
                    {
                        input_sender
                            .send(SketchBoardInput::PasteImageFromClipboard(pixbuf))
                            .ok();
                    }
                }
                Ok(None) => {
                    // No image on the clipboard. Silent no-op rather
                    // than a notification — Ctrl+V on a clipboard
                    // holding just text is benign.
                }
                Err(err) => {
                    eprintln!("Clipboard image read failed: {err}");
                }
            }
        });
    }

    fn handle_undo(&mut self) -> ToolUpdateResult {
        if self.active_tool.borrow().active() {
            self.active_tool.borrow_mut().handle_undo()
        } else if self.renderer.undo() {
            ToolUpdateResult::Redraw
        } else {
            ToolUpdateResult::Unmodified
        }
    }

    fn handle_redo(&mut self) -> ToolUpdateResult {
        if self.active_tool.borrow().active() {
            self.active_tool.borrow_mut().handle_redo()
        } else if self.renderer.redo() {
            ToolUpdateResult::Redraw
        } else {
            ToolUpdateResult::Unmodified
        }
    }

    /// Delete every currently-selected drawable. Same effect as the
    /// existing Backspace / Delete path through the PointerTool —
    /// wired here as a sketch_board-level handler so it can fire
    /// from any active tool's hotkey cascade (e.g. Ctrl+D while a
    /// drawing tool is active and a previous shape is implicitly
    /// selected). Returns `Unmodified` when nothing is selected;
    /// otherwise emits the matching `DeleteDrawable(s)` result so
    /// the update-result loop applies it through the renderer.
    fn delete_selection(&mut self) -> ToolUpdateResult {
        let pointer_tool = self.tools.get(&Tools::Pointer);
        let selected = pointer_tool.borrow().selected_drawables();
        if selected.is_empty() {
            return ToolUpdateResult::Unmodified;
        }
        // Skip locked drawables; keep them selected so the user has
        // something to operate on after unlocking. Mirrors the
        // pointer-tool's own Delete/Backspace handler.
        let (to_delete, to_keep): (Vec<_>, Vec<_>) = selected
            .into_iter()
            .partition(|id| !self.renderer.drawable_flags(*id).map(|f| f.1).unwrap_or(false));
        pointer_tool
            .borrow_mut()
            .set_selected_drawables(to_keep);
        if to_delete.is_empty() {
            return ToolUpdateResult::Unmodified;
        }
        if to_delete.len() == 1 {
            ToolUpdateResult::DeleteDrawable(to_delete[0])
        } else {
            ToolUpdateResult::DeleteDrawables(to_delete)
        }
    }

    /// Duplicate every currently-selected drawable. Each copy is
    /// offset by `(DUPLICATE_DX_PX, DUPLICATE_DY_PX)` — diagonal
    /// enough to read as "another one over here" but close enough
    /// that chained Alt+D's don't fly off the canvas. Per-axis sign
    /// flips when the default direction would push the duplicate
    /// off-canvas and the opposite direction has room. Selection
    /// moves onto the new copies so subsequent edits operate on
    /// the duplicates rather than the originals.
    fn duplicate_selection(&mut self, sender: &ComponentSender<Self>) -> ToolUpdateResult {
        const DUPLICATE_DX_PX: f32 = -100.0;
        const DUPLICATE_DY_PX: f32 = 100.0;
        let pointer_tool = self.tools.get(&Tools::Pointer);
        let selected_ids = pointer_tool.borrow().selected_drawables();
        if selected_ids.is_empty() {
            return ToolUpdateResult::Unmodified;
        }
        let (img_w, img_h) = self.renderer.image_dimensions();
        let (img_w, img_h) = (img_w as f32, img_h as f32);
        let mut new_ids = Vec::with_capacity(selected_ids.len());
        for id in selected_ids {
            let Some(mut d) = self.renderer.clone_drawable(id) else {
                continue;
            };
            // Ergonomics nudge: if the default direction (down-left)
            // would put the duplicate's leading edge past the canvas
            // and there's room to flip that axis to the opposite
            // direction, flip it.
            let (mut dx, mut dy) = (DUPLICATE_DX_PX, DUPLICATE_DY_PX);
            if let Some(b) = d.bounds() {
                // Flip dx if the default direction would clip and the
                // opposite direction has room. Two cases because dx's
                // sign tells us which canvas edge to check (and the
                // bounds-fit test mirrors accordingly).
                let flip_dx = (dx < 0.0
                    && b.pos.x + dx < 0.0
                    && b.pos.x + b.size.x - dx <= img_w)
                    || (dx > 0.0
                        && b.pos.x + b.size.x + dx > img_w
                        && b.pos.x - dx >= 0.0);
                if flip_dx {
                    dx = -dx;
                }
                let flip_dy = (dy > 0.0
                    && b.pos.y + b.size.y + dy > img_h
                    && b.pos.y - dy >= 0.0)
                    || (dy < 0.0
                        && b.pos.y + dy < 0.0
                        && b.pos.y + b.size.y - dy <= img_h);
                if flip_dy {
                    dy = -dy;
                }
            }
            let offset = crate::math::Vec2D::new(dx, dy);
            d.translate(offset);
            let new_id = self.renderer.commit(d);
            new_ids.push(new_id);
        }
        if new_ids.is_empty() {
            return ToolUpdateResult::Unmodified;
        }
        pointer_tool.borrow_mut().set_selected_drawables(new_ids);
        if APP_CONFIG.read().auto_copy() {
            self.renderer.request_render(&[Action::SaveToClipboard]);
        }
        self.refresh_screen();
        self.sync_toolbar_to_selection(sender);
        ToolUpdateResult::Unmodified
    }

    fn handle_reset(&mut self) -> ToolUpdateResult {
        // can't use lazy || here
        if self.deactivate_active_tool() | self.renderer.reset() {
            ToolUpdateResult::Redraw
        } else {
            ToolUpdateResult::Unmodified
        }
    }

    fn handle_resize(&mut self) -> ToolUpdateResult {
        self.renderer.reset_size(0.);
        self.renderer.request_render(&[]);
        ToolUpdateResult::Unmodified
    }

    fn handle_original_scale(&mut self) -> ToolUpdateResult {
        self.renderer.reset_size(1.);
        self.renderer.request_render(&[]);
        ToolUpdateResult::Unmodified
    }

    /// Apply a zoom command from the zoom-indicator dropdown. Each path
    /// triggers a render whose `update_transformation` then pushes a
    /// `ZoomDisplayChanged` back up to keep the indicator in sync.
    fn handle_zoom_command(&mut self, cmd: ZoomCommand) {
        let factor = APP_CONFIG.read().zoom_factor();
        match cmd {
            ZoomCommand::In => self.renderer.set_zoom_scale(factor),
            ZoomCommand::Out => self.renderer.set_zoom_scale(1.0 / factor),
            ZoomCommand::FitCanvas => self.renderer.reset_size(0.0),
            ZoomCommand::Abs(scale) => self.renderer.reset_size(scale),
        }
        // Keep the crop tool's cached render scale fresh so its handle
        // hit-testing stays screen-constant after a zoom command
        // re-frames the canvas.
        if self.active_tool_type() == Tools::Crop {
            let crop_tool = self.tools.get_crop_tool();
            let in_edit = crop_tool.borrow().is_active_edit();
            if in_edit {
                let (eff_scale, _) = self.renderer.render_transform();
                crop_tool.borrow_mut().set_render_scale(eff_scale);
            }
        }
        self.renderer.request_render(&[]);
    }

    // Toolbars = Tools Toolbar + Style Toolbar
    fn handle_toggle_toolbars_display(
        &mut self,
        sender: ComponentSender<Self>,
    ) -> ToolUpdateResult {
        sender
            .output_sender()
            .emit(SketchBoardOutput::ToggleToolbarsDisplay);
        ToolUpdateResult::Unmodified
    }

    /// Walk the current selection, give each clone to `mutate`, and
    /// collect the ones it touched into a single `ToolUpdateResult`.
    /// Returning `false` from `mutate` skips the clone — used to gate
    /// on whether the drawable actually owns the property being
    /// applied (e.g. arrow-style on a non-arrow). One-element results
    /// fold to `ModifyDrawable`, multi-element to `ModifyDrawables`,
    /// empty to `Unmodified` so callers can fall through to a
    /// "treat as default" branch when nothing relevant was selected.
    fn apply_to_selection<F>(&mut self, mut mutate: F) -> ToolUpdateResult
    where
        F: FnMut(&mut dyn Drawable) -> bool,
    {
        let selected_ids = self
            .tools
            .get(&Tools::Pointer)
            .borrow()
            .selected_drawables();
        let mut updates: Vec<(DrawableId, Box<dyn Drawable>)> = Vec::new();
        for id in selected_ids {
            if let Some(mut d) = self.renderer.clone_drawable(id)
                && mutate(d.as_mut())
            {
                updates.push((id, d));
            }
        }
        match updates.len() {
            0 => ToolUpdateResult::Unmodified,
            1 => {
                let (id, d) = updates.pop().unwrap();
                ToolUpdateResult::ModifyDrawable(id, d)
            }
            _ => ToolUpdateResult::ModifyDrawables(updates),
        }
    }

    /// Re-shape every currently-selected Arrow drawable to the given
    /// style. Wired into `ArrowStyleSelected` (popover-click and
    /// double-tap cycle paths both flow through it) so the picker
    /// retroactively edits the canvas instead of only affecting
    /// future strokes. Mirrors the existing TextBackground path.
    fn apply_arrow_style_to_selection(
        &mut self,
        style: crate::tools::ArrowStyle,
    ) -> ToolUpdateResult {
        self.apply_to_selection(|d| {
            if d.arrow_style().is_some() {
                d.set_arrow_style_on_drawable(style);
                true
            } else {
                false
            }
        })
    }

    /// Same as `apply_arrow_style_to_selection` for Blur drawables.
    fn apply_blur_style_to_selection(
        &mut self,
        style: crate::tools::BlurStyle,
    ) -> ToolUpdateResult {
        self.apply_to_selection(|d| {
            if d.blur_style().is_some() {
                d.set_blur_style_on_drawable(style);
                true
            } else {
                false
            }
        })
    }

    /// Re-run the brush smoothing pipeline on every currently-selected
    /// brush annotation at the given level. Gates on the drawable's
    /// `smooth_level()` so the caller can fall through to the
    /// "treat as default" branch when nothing brush is selected.
    fn apply_brush_smooth_to_selection(&mut self, level: usize) -> ToolUpdateResult {
        self.apply_to_selection(|d| {
            if d.smooth_level().is_some() {
                d.set_smooth_level(level);
                true
            } else {
                false
            }
        })
    }

    /// Read the current variant for the tool's cycle. When a single
    /// drawable of the matching type is selected, prefer its style
    /// over the global default — so cycling operates on the thing
    /// the user has on screen, not the stale tool default. Falls
    /// back to the persisted default when nothing relevant is
    /// selected.
    /// Resolve the seed value for a style cycle. If exactly one
    /// drawable is selected and it exposes a property (`extract`
    /// returns Some), use that — so cycling operates on the thing
    /// the user has on screen. Otherwise fall back to the persisted
    /// default loaded via `fallback`, or `S::default()` if even that
    /// returns None.
    fn cycle_seed_from_selection<S, F, G>(&self, extract: F, fallback: G) -> S
    where
        S: Default,
        F: FnOnce(&dyn Drawable) -> Option<S>,
        G: FnOnce() -> Option<S>,
    {
        let selected = self
            .tools
            .get(&Tools::Pointer)
            .borrow()
            .selected_drawables();
        if selected.len() == 1
            && let Some(d) = self.renderer.clone_drawable(selected[0])
            && let Some(s) = extract(d.as_ref())
        {
            return s;
        }
        fallback().unwrap_or_default()
    }

    fn cycle_seed_arrow(&self) -> crate::tools::ArrowStyle {
        self.cycle_seed_from_selection(|d| d.arrow_style(), crate::state::load_arrow_style)
    }
    fn cycle_seed_blur(&self) -> crate::tools::BlurStyle {
        self.cycle_seed_from_selection(|d| d.blur_style(), crate::state::load_blur_style)
    }
    fn cycle_seed_text(&self) -> crate::tools::TextBackground {
        self.cycle_seed_from_selection(|d| d.text_background(), crate::state::load_text_background)
    }

    /// Seed for the highlighter style cycle. Unlike Arrow/Blur/Text
    /// where the style is a baked drawable property, the highlighter's
    /// HighlighterStyle is a *tool* setting only — committed strokes
    /// don't remember which mode they were drawn in. So the seed
    /// comes from the active tool's current style (which already
    /// reflects state.toml after init).
    fn cycle_seed_highlighter(&self) -> crate::tools::HighlighterStyle {
        self.tools
            .get(&Tools::Highlighter)
            .borrow()
            .highlighter_style()
            .unwrap_or_default()
    }

    /// double-press of the tool's shortcut key (see the
    /// `TextEventMsg::Commit` handler). Tools without per-tool style
    /// variants (Pointer, Crop, Brush, etc.) are no-ops; the
    /// double-press still gets consumed (no visible change) which is
    /// the desired behavior — re-selecting is harmless.
    fn cycle_tool_style(&mut self, tool: Tools, sender: &ComponentSender<Self>) {
        // The cycle path is intentionally toast-free: emitting the
        // `*Cycled` output below routes through the StyleToolbar's
        // `Set*Style`/`SetTextBackground` arms, which fan back out
        // as the regular `*Selected` toolbar events. Those handlers
        // own the toast emission, so a single user action shows a
        // single toast regardless of whether the trigger was the
        // double-tap, the popover row, or the dropdown.
        use crate::tools::{ArrowStyle, BlurStyle, TextBackground};
        match tool {
            Tools::Arrow => {
                // Seed off the selected arrow (if any) so cycling
                // operates on what the user is actually editing.
                // Falls back to the persisted default when nothing
                // matching is selected.
                let next = match self.cycle_seed_arrow() {
                    ArrowStyle::Standard => ArrowStyle::Pointy,
                    ArrowStyle::Pointy => ArrowStyle::Curved,
                    ArrowStyle::Curved => ArrowStyle::Double,
                    ArrowStyle::Double => ArrowStyle::Standard,
                };
                self.tools
                    .get(&Tools::Arrow)
                    .borrow_mut()
                    .set_arrow_style(next);
                crate::state::save_arrow_style(next);
                sender
                    .output_sender()
                    .emit(SketchBoardOutput::ArrowStyleCycled(next));
            }
            Tools::Blur => {
                let next = match self.cycle_seed_blur() {
                    BlurStyle::Pixelate => BlurStyle::SecureBlur,
                    BlurStyle::SecureBlur => BlurStyle::Gaussian,
                    BlurStyle::Gaussian => BlurStyle::BlackOut,
                    BlurStyle::BlackOut => BlurStyle::Pixelate,
                };
                self.tools
                    .get(&Tools::Blur)
                    .borrow_mut()
                    .set_blur_style(next);
                crate::state::save_blur_style(next);
                sender
                    .output_sender()
                    .emit(SketchBoardOutput::BlurStyleCycled(next));
            }
            Tools::Text => {
                let next = match self.cycle_seed_text() {
                    TextBackground::Plain => TextBackground::Rounded,
                    TextBackground::Rounded => TextBackground::Plain,
                };
                self.tools
                    .get(&Tools::Text)
                    .borrow_mut()
                    .set_text_background(next);
                crate::state::save_text_background(next);
                sender
                    .output_sender()
                    .emit(SketchBoardOutput::TextBackgroundCycled(next));
            }
            Tools::Highlighter => {
                let next = self.cycle_seed_highlighter().next();
                self.tools
                    .get(&Tools::Highlighter)
                    .borrow_mut()
                    .set_highlighter_style(next);
                crate::state::save_highlighter_style(next);
                sender
                    .output_sender()
                    .emit(SketchBoardOutput::HighlighterStyleCycled(next));
            }
            _ => {
                // Tools without per-tool variants — pointer, crop,
                // brush, line, rectangle, ellipse, marker, highlighter,
                // spotlight. Nothing to cycle; the double-tap just
                // gets absorbed.
            }
        }
    }

    /// Switch the active tool with all the snapback / deactivate /
    /// activate side effects. Does NOT touch the current selection —
    /// explicit user-driven tool changes (the `ToolbarEvent::ToolSelected`
    /// arm) clear selection before calling this; selection-driven auto-
    /// switch (`sync_toolbar_to_selection`) calls this directly so the
    /// just-made selection survives.
    fn switch_active_tool(
        &mut self,
        tool: Tools,
        sender: ComponentSender<Self>,
    ) -> ToolUpdateResult {
        // Capture the prior non-Crop tool right before we switch — the
        // Esc handler in `CropTool` uses this to restore the user to
        // the tool they had before entering Crop, rather than dropping
        // them on Pointer.
        let current_tool = self.active_tool_type();
        if tool == Tools::Crop && current_tool != Tools::Crop {
            self.tool_before_crop = Some(current_tool);
        }
        // Re-entering Highlighter snaps the slider back to the saved
        // default (or the system detent if no save). In-session edits
        // during a single tool stretch persist across multiple new
        // strokes; switching away wipes them so the next entry starts
        // from a known baseline.
        //
        // Spotlight is the exception: its darkness is a global property
        // of the canvas, not a per-stroke setting (all spotlight shapes
        // share one inverse-mask overlay), so an in-session adjustment
        // is meant to stick for the whole session — switching tools and
        // coming back must NOT reset it.
        if tool != current_tool {
            let sticky = APP_CONFIG.read().sticky_session_defaults();
            match tool {
                Tools::Highlighter => {
                    // When sticky-defaults is on AND the user has
                    // touched the opacity slider this session, restore
                    // that value instead of the saved persistent
                    // default — same intent as the size slider's
                    // session memory.
                    let saved = if sticky
                        && let Some(v) = self.session_highlighter_opacity
                    {
                        v
                    } else {
                        crate::state::load_highlighter_opacity().unwrap_or(0.40)
                    };
                    self.style.highlighter_opacity = saved;
                    sender
                        .output_sender()
                        .emit(SketchBoardOutput::HighlighterOpacityReset(saved));
                }
                Tools::Brush => {
                    // Same snapback semantics as the other tool-specific
                    // sliders: re-entering Brush pulls the saved default
                    // off state.toml (falling back to the config / built-in
                    // 2) and pushes it into APP_CONFIG so the next stroke
                    // uses it.
                    //
                    // BUT: if the user got here because they clicked an
                    // existing brush annotation (the selection-driven
                    // auto-switch via this helper from
                    // `sync_toolbar_to_selection`), use THAT annotation's
                    // stored level instead — so the slider lands on the
                    // value the selected stroke was drawn with. Subsequent
                    // slider tweaks re-smooth the selected stroke; re-
                    // entering Brush without a selection (the user-driven
                    // `ToolbarEvent::ToolSelected` path, which deselects
                    // first) always falls back to the saved default.
                    let selected_level = {
                        let pt = self.tools.get(&Tools::Pointer);
                        let selected = pt.borrow().selected_drawables();
                        if selected.len() == 1 {
                            self.renderer
                                .clone_drawable(selected[0])
                                .and_then(|d| d.smooth_level())
                        } else {
                            None
                        }
                    };
                    let saved = selected_level.unwrap_or_else(|| {
                        // Sticky-defaults: prefer the in-session value
                        // when present, falling through to the saved
                        // default → config / built-in.
                        if sticky
                            && let Some(v) = self.session_brush_smooth
                        {
                            v
                        } else {
                            crate::state::load_brush_post_smooth_iterations()
                                .unwrap_or_else(|| APP_CONFIG.read().brush_post_smooth_iterations())
                        }
                    });
                    // Only update APP_CONFIG when this is a genuine snapback
                    // (no selection driving the value) — otherwise the user's
                    // slider tweaks for the selected annotation would bleed
                    // into the default for the *next* new stroke.
                    if selected_level.is_none() {
                        APP_CONFIG.write().set_brush_post_smooth_iterations(saved);
                    }
                    sender
                        .output_sender()
                        .emit(SketchBoardOutput::BrushPostSmoothReset(saved));
                }
                Tools::Rectangle | Tools::Ellipse => {
                    // Same snapback for per-tool fill. Saved default wins
                    // if the user has explicitly pinned one for THIS shape
                    // tool; otherwise leave style.fill alone so an in-
                    // session toggle survives switching between Rectangle
                    // and Ellipse.
                    //
                    // Sticky-defaults: prefer the in-session value for
                    // THIS specific shape tool (Rect ≠ Ellipse here, even
                    // though they share `style.fill`) over the saved
                    // default, so the user's in-session toggle survives
                    // a round trip through other tools.
                    let saved_default = if sticky
                        && let Some(v) = self.session_fill_per_tool.get(&tool).copied()
                    {
                        Some(v)
                    } else {
                        crate::state::load_fill_for_tool(tool)
                    };
                    if let Some(saved) = saved_default
                        && saved != self.style.fill
                    {
                        self.style.fill = saved;
                        sender
                            .output_sender()
                            .emit(SketchBoardOutput::FillShapesChanged(saved));
                    }
                }
                _ => {}
            }
        }
        // Notify the parent so the style toolbar can re-evaluate
        // tool-specific controls (e.g. the arrow-style dropdown).
        sender
            .output_sender()
            .emit(SketchBoardOutput::ToolSwitchShortcut(tool));
        // deactivate old tool and save drawable, if any
        let old_tool = self.active_tool.clone();
        let mut deactivate_result =
            old_tool.borrow_mut().handle_event(ToolEvent::Deactivated);

        old_tool.borrow_mut().set_im_context(None);

        match deactivate_result {
            ToolUpdateResult::Commit(d) => {
                self.renderer.commit(d);
                if APP_CONFIG.read().auto_copy() {
                    self.renderer.request_render(&[Action::SaveToClipboard]);
                }
                // we handle commit directly and "downgrade" to a simple redraw result
                deactivate_result = ToolUpdateResult::Redraw;
            }
            // TextTool emits ModifyDrawable on tool-switch when finalizing
            // a re-edit; replace the existing drawable in-place.
            ToolUpdateResult::ModifyDrawable(id, d) => {
                self.renderer.modify(id, d);
                if APP_CONFIG.read().auto_copy() {
                    self.renderer.request_render(&[Action::SaveToClipboard]);
                }
                deactivate_result = ToolUpdateResult::Redraw;
            }
            _ => {}
        }

        // change active tool
        self.active_tool = self.tools.get(&tool);
        self.renderer.set_active_tool(self.active_tool.clone());
        let widget_ref: gtk::Widget = self.renderer.clone().upcast();
        self.active_tool
            .borrow_mut()
            .set_im_context(Some(crate::tools::InputContext {
                im_context: self.im_context.clone(),
                widget: widget_ref,
            }));

        // set sender for tool
        self.active_tool
            .borrow_mut()
            .set_sender(sender.input_sender().clone());

        // give the tool a handle to query the drawable stack (hit-test, etc.)
        let store: Rc<dyn DrawableStore> = Rc::new(self.renderer.clone());
        self.active_tool.borrow_mut().set_drawable_store(store);

        // send style event
        self.active_tool
            .borrow_mut()
            .handle_event(ToolEvent::StyleChanged(self.style));

        // send activated event
        let activate_result = self
            .active_tool
            .borrow_mut()
            .handle_event(ToolEvent::Activated);

        // Update cursor immediately so the user gets the crosshair
        // (or arrow for pointer/crop) without waiting for mouse move.
        self.apply_idle_cursor();

        // Push the renderer's current image→canvas scale into the crop
        // tool so handle hit-testing is screen-constant from the first
        // click. `handle_activated` may have just un-committed a crop,
        // leaving the cached scale describing the old zoomed-in view —
        // so run a synchronous re-layout first, then sample.
        if tool == Tools::Crop {
            self.renderer.refresh_transform();
            let (eff_scale, _) = self.renderer.render_transform();
            self.tools
                .get_crop_tool()
                .borrow_mut()
                .set_render_scale(eff_scale);
        }

        match activate_result {
            ToolUpdateResult::Unmodified => deactivate_result,
            _ => activate_result,
        }
    }

    fn handle_toolbar_event(
        &mut self,
        toolbar_event: ToolbarEvent,
        sender: ComponentSender<Self>,
    ) -> ToolUpdateResult {
        match toolbar_event {
            ToolbarEvent::ToolSelected(tool) => {
                // Explicit user-driven tool change always clears any
                // existing selection — "I'm switching tools" reads as a
                // start-fresh action. The selection-driven auto-switch
                // in `sync_toolbar_to_selection` deliberately skips this
                // arm and calls `switch_active_tool` directly so the
                // just-made selection survives.
                let pointer = self.tools.get(&Tools::Pointer);
                let had_selection = !pointer.borrow().selected_drawables().is_empty();
                pointer.borrow_mut().set_selected_drawables(Vec::new());
                let result = self.switch_active_tool(tool, sender);
                // `switch_active_tool` typically returns Unmodified
                // (the tool's Activated/Deactivated handlers don't ask
                // for a redraw). If we just cleared a non-empty
                // selection, force a redraw — otherwise the stale
                // SelectionOverlay handles linger on the canvas until
                // the next event happens to invalidate them.
                if had_selection && matches!(result, ToolUpdateResult::Unmodified) {
                    ToolUpdateResult::Redraw
                } else {
                    result
                }
            }
            ToolbarEvent::ColorSelected(color) => {
                self.style.color = color;
                self.dispatch_style_change()
            }
            ToolbarEvent::SizeSelected(size) => {
                self.style.size = size;
                self.dispatch_style_change()
            }
            ToolbarEvent::SaveFile => self.handle_action(&[Action::SaveToFile]),
            ToolbarEvent::CopyClipboard => self.handle_action(&[Action::SaveToClipboard]),
            ToolbarEvent::Undo => self.handle_undo(),
            ToolbarEvent::Redo => self.handle_redo(),
            ToolbarEvent::Reset => self.handle_reset(),
            ToolbarEvent::ToggleFill => {
                // Pre-sync `self.style.fill` to the currently-selected
                // SHAPE's fill state (Rect / Ellipse only) before
                // flipping it. Without this, the first press might
                // just bring the global into sync with the shape and
                // produce no visible change. Brush / arrow / text /
                // etc. selections don't participate in the pre-sync
                // because they don't carry a meaningful fill state.
                let selected = self
                    .tools
                    .get(&Tools::Pointer)
                    .borrow()
                    .selected_drawables();
                // F (and the alt-cycle / button paths that fan in
                // here) is a Rect/Ellipse-only gesture. Bail when the
                // active tool isn't fillable AND there's no fillable
                // shape in the selection — without this, pressing F
                // while Line / Brush / Arrow / Text / etc. is active
                // would still flip the global fill flag, fire the
                // "Fill shape" toast, and refresh the (hidden) button
                // mirror, even though no fillable shape exists in the
                // current context. Mirrors the gating that already
                // skips fill for those tools elsewhere (button
                // visibility, alt-slider cycle, save-as-default).
                let active = self.active_tool_type();
                let fillable_active = matches!(active, Tools::Rectangle | Tools::Ellipse);
                let selection_has_fillable = selected.iter().any(|id| {
                    self.renderer
                        .clone_drawable(*id)
                        .map(|d| {
                            matches!(
                                d.tool_type(),
                                Some(Tools::Rectangle) | Some(Tools::Ellipse)
                            )
                        })
                        .unwrap_or(false)
                });
                if !fillable_active && !selection_has_fillable {
                    return ToolUpdateResult::Unmodified;
                }
                if let Some(fill) = selected.iter().find_map(|id| {
                    self.renderer.clone_drawable(*id).and_then(|d| {
                        if matches!(
                            d.tool_type(),
                            Some(Tools::Rectangle) | Some(Tools::Ellipse)
                        ) {
                            d.style().map(|s| s.fill)
                        } else {
                            None
                        }
                    })
                }) {
                    self.style.fill = fill;
                }
                self.style.fill = !self.style.fill;
                let new_fill = self.style.fill;
                // Record the in-session per-tool fill so the
                // sticky-defaults snap-back uses this on return.
                // Only relevant to Rect / Ellipse — those are the
                // only tools whose snap-back consults this map.
                let active = self.active_tool_type();
                if matches!(active, Tools::Rectangle | Tools::Ellipse) {
                    self.session_fill_per_tool.insert(active, new_fill);
                }
                // Toast announces the new state so a keyboard toggle
                // (`F`) reads as feedback, not a silent change.
                let label = if new_fill { "Fill shape" } else { "No fill" };
                sender
                    .output_sender()
                    .emit(SketchBoardOutput::ShowCycleToast(label.to_string()));
                // Mirror the new fill out to the toolbar so the icon
                // refreshes.
                sender
                    .output_sender()
                    .emit(SketchBoardOutput::FillShapesChanged(new_fill));
                // Forward to the active tool so its next-stroke style
                // picks up the new fill — but skip Pointer, which
                // would otherwise apply self.style to every selected
                // drawable wholesale (overwriting brushes' color /
                // size / etc., not just toggling fill on Rect/Ellipse).
                if self.active_tool_type() != Tools::Pointer {
                    self.active_tool
                        .borrow_mut()
                        .handle_event(ToolEvent::StyleChanged(self.style));
                }
                // Apply ONLY to fillable selection: Rect/Ellipse get
                // their `fill` flipped without disturbing their other
                // style fields; brushes / arrows / text / etc. in the
                // selection are left untouched.
                self.apply_to_selection(|d| {
                    if !matches!(
                        d.tool_type(),
                        Some(Tools::Rectangle) | Some(Tools::Ellipse)
                    ) {
                        return false;
                    }
                    let Some(mut style) = d.style() else {
                        return false;
                    };
                    if style.fill == new_fill {
                        return false;
                    }
                    style.fill = new_fill;
                    d.set_style(style);
                    true
                })
            }
            ToolbarEvent::SaveFileAs => self.handle_action(&[Action::SaveToFileAs]),
            ToolbarEvent::Resize => self.handle_resize(),
            ToolbarEvent::OriginalScale => self.handle_original_scale(),
            ToolbarEvent::OpenPreferences => {
                sender
                    .output_sender()
                    .emit(SketchBoardOutput::OpenPreferences);
                ToolUpdateResult::Unmodified
            }
            ToolbarEvent::ToggleLayerPanel => {
                self.layer_panel_open = !self.layer_panel_open;
                self.layer_panel_content
                    .set_visible(self.layer_panel_open);
                if self.layer_panel_open {
                    self.rebuild_layer_panel_rows_if_open();
                }
                ToolUpdateResult::Unmodified
            }
            ToolbarEvent::FocusCanvas => {
                // Same idle re-grab as the SketchBoardInput variant —
                // popover-dismissal focus restores override an
                // immediate-only grab, so schedule a second attempt
                // after GTK's pending focus work has settled.
                self.renderer.grab_focus();
                let renderer = self.renderer.clone();
                relm4::gtk::glib::idle_add_local_once(move || {
                    renderer.grab_focus();
                });
                ToolUpdateResult::Unmodified
            }
            ToolbarEvent::ArrowStyleSelected(style) => {
                self.tools
                    .get(&Tools::Arrow)
                    .borrow_mut()
                    .set_arrow_style(style);
                // Auto-persist the last-chosen geometry so re-opening
                // the Arrow tool (this session or next launch) starts
                // on the same variant.
                crate::state::save_arrow_style(style);
                // Toast fires here so the popover-click path and the
                // double-tap cycle (which routes through the
                // StyleToolbar → SetArrowStyle → upstream emit chain)
                // both end up showing a single, consistent toast.
                sender
                    .output_sender()
                    .emit(SketchBoardOutput::ShowCycleToast(format!(
                        "Arrow: {}",
                        style.display_name()
                    )));
                // Also re-style any currently-selected arrow drawables.
                // Mirrors the `TextBackgroundSelected` retroactive
                // path: changing the picker should re-shape what's
                // already on the canvas, not only future strokes.
                self.apply_arrow_style_to_selection(style)
            }
            ToolbarEvent::HighlighterStyleSelected(style) => {
                self.tools
                    .get(&Tools::Highlighter)
                    .borrow_mut()
                    .set_highlighter_style(style);
                // Auto-persist: cycling becomes the new default for
                // the next launch.
                crate::state::save_highlighter_style(style);
                sender
                    .output_sender()
                    .emit(SketchBoardOutput::ShowCycleToast(format!(
                        "Highlighter: {}",
                        style.display_name()
                    )));
                // Highlighter style is a *tool* setting, not a
                // drawable property — committed highlight strokes
                // baked in their forced_width at the time of commit.
                // So there's no "apply to selection" here; the
                // change only affects future strokes.
                ToolUpdateResult::Unmodified
            }
            ToolbarEvent::BlurStyleSelected(style) => {
                self.tools
                    .get(&Tools::Blur)
                    .borrow_mut()
                    .set_blur_style(style);
                // Same auto-save semantics as arrow style — last-used
                // algorithm becomes the new default.
                crate::state::save_blur_style(style);
                sender
                    .output_sender()
                    .emit(SketchBoardOutput::ShowCycleToast(format!(
                        "Blur: {}",
                        style.display_name()
                    )));
                self.apply_blur_style_to_selection(style)
            }
            ToolbarEvent::SnapToEdgesChanged(value) => {
                self.tools
                    .get_crop_tool()
                    .borrow_mut()
                    .set_snap_to_edges(value);
                crate::state::save_snap_to_edges(value);
                ToolUpdateResult::Unmodified
            }
            ToolbarEvent::SpotlightDarknessChanged(value) => {
                self.style.spotlight_darkness = value;
                // Push the new value into the renderer so the next
                // frame uses it. The dispatch_style_change call also
                // triggers a redraw via the active spotlight tool's
                // handle_style_event, which returns Redraw.
                self.renderer.set_spotlight_darkness(value);
                // No auto-save: the slider snaps back to the saved
                // default on each launch. Right-click → "Save as
                // default" on the slider is the only path that
                // updates state.toml.
                self.dispatch_style_change()
            }
            ToolbarEvent::HighlighterOpacityChanged(value) => {
                self.style.highlighter_opacity = value;
                // Record the in-session value so the next time the
                // Highlighter tool is re-entered (with sticky-defaults
                // on) it restores this opacity instead of snapping
                // back to the saved default. Unconditional record —
                // costs nothing while the pref is off, and lets
                // toggling the pref mid-session pick up the user's
                // already-made tweak.
                self.session_highlighter_opacity = Some(value);
                // Same no-auto-save rule as spotlight darkness.
                self.dispatch_style_change()
            }
            ToolbarEvent::SaveSpotlightDarknessAsDefault => {
                crate::state::save_spotlight_darkness(self.style.spotlight_darkness);
                ToolUpdateResult::Unmodified
            }
            ToolbarEvent::SaveHighlighterOpacityAsDefault => {
                crate::state::save_highlighter_opacity(self.style.highlighter_opacity);
                ToolUpdateResult::Unmodified
            }
            ToolbarEvent::BrushPostSmoothChanged(value) => {
                // Two paths:
                //   1. If the user has a brush annotation selected,
                //      re-smooth THAT annotation in place — the slider
                //      becomes an "edit this stroke" control.
                //      `BrushDrawable::smooth_post_stroke` always works
                //      from the cached raw input so the stroke morphs
                //      progressively without compounding smoothing.
                //   2. Otherwise, treat as a default for the next
                //      stroke and live-update APP_CONFIG. No persist
                //      on every nudge — right-click is the persist gate.
                let selection_result = self.apply_brush_smooth_to_selection(value);
                if matches!(selection_result, ToolUpdateResult::Unmodified) {
                    APP_CONFIG.write().set_brush_post_smooth_iterations(value);
                    // No selection → this IS a next-stroke default
                    // adjustment, so record into the session cache for
                    // sticky-defaults restoration. We deliberately
                    // skip the with-selection branch: those edits
                    // intentionally don't bleed into APP_CONFIG and
                    // shouldn't bleed into the session cache either.
                    self.session_brush_smooth = Some(value);
                }
                selection_result
            }
            ToolbarEvent::SaveBrushPostSmoothAsDefault(value) => {
                // Persist the slider's current value (carried in the
                // event) AND promote it to APP_CONFIG so the snapback
                // on the next Brush re-entry sees the just-saved
                // default. Reading APP_CONFIG here would be wrong: when
                // the user adjusts smoothness with a brush stroke
                // selected, those edits intentionally don't bleed into
                // APP_CONFIG, so APP_CONFIG carries a stale value.
                crate::state::save_brush_post_smooth_iterations(value);
                APP_CONFIG.write().set_brush_post_smooth_iterations(value);
                ToolUpdateResult::Unmodified
            }
            ToolbarEvent::SaveFillAsDefault => {
                // Only Rectangle / Ellipse honor fill; save against
                // whichever of those two is active. If the user
                // somehow right-clicks the (then-hidden) button from
                // a different tool, skip — there's nothing meaningful
                // to persist for, e.g., Brush.
                let tool = self.active_tool_type();
                if matches!(tool, Tools::Rectangle | Tools::Ellipse) {
                    crate::state::save_fill_for_tool(tool, self.style.fill);
                }
                ToolUpdateResult::Unmodified
            }
            ToolbarEvent::TextBackgroundSelected(bg) => {
                // Update the TextTool default so subsequent NEW texts
                // pick up the chosen style.
                self.tools
                    .get(&Tools::Text)
                    .borrow_mut()
                    .set_text_background(bg);
                // Auto-save: the last-chosen background becomes the
                // default for the next launch. Same pattern as arrow
                // and blur style.
                crate::state::save_text_background(bg);
                // Toast — fires for BOTH the dropdown path and the
                // double-tap cycle so the user gets consistent
                // feedback regardless of which affordance changed
                // the value. (Cycle's own emit path is suppressed
                // because the cycle handler already emits the same
                // toast text; doing it here would double-show.)
                sender
                    .output_sender()
                    .emit(SketchBoardOutput::ShowCycleToast(format!(
                        "Text: {}",
                        bg.display_name()
                    )));

                // Also apply retroactively to any selected text
                // drawables — without this the dropdown only takes
                // effect on creation, not when restyling an existing
                // text the user has just selected.
                let selected_ids = self
                    .tools
                    .get(&Tools::Pointer)
                    .borrow()
                    .selected_drawables();
                let mut updates: Vec<(DrawableId, Box<dyn Drawable>)> = Vec::new();
                for id in selected_ids {
                    if let Some(mut d) = self.renderer.clone_drawable(id) {
                        d.set_text_background(bg);
                        updates.push((id, d));
                    }
                }
                match updates.len() {
                    0 => ToolUpdateResult::Redraw,
                    1 => {
                        let (id, d) = updates.pop().unwrap();
                        ToolUpdateResult::ModifyDrawable(id, d)
                    }
                    _ => ToolUpdateResult::ModifyDrawables(updates),
                }
            }
            ToolbarEvent::RevertCrop => {
                // Two behaviors depending on where the click came from:
                //   * Inside Crop tool — reset to the fresh-entry seed
                //     so the user can immediately drag a new region
                //     without leaving the tool.
                //   * Outside Crop tool — drop the crop entirely so
                //     the committed-rect transform clears and the
                //     Revert button disappears with it.
                if self.active_tool_type() == Tools::Crop {
                    self.tools.get_crop_tool().borrow_mut().revert_to_seed(true);
                } else {
                    self.tools.get_crop_tool().borrow_mut().revert();
                    sender
                        .output_sender()
                        .emit(SketchBoardOutput::CropPresenceChanged(false));
                }
                ToolUpdateResult::Redraw
            }
            ToolbarEvent::CancelCrop => self.tools.get_crop_tool().borrow_mut().cancel(),
            ToolbarEvent::ApplyCrop => self.tools.get_crop_tool().borrow_mut().commit(),
            ToolbarEvent::CropAspectRatioChanged(ratio) => {
                self.tools
                    .get_crop_tool()
                    .borrow_mut()
                    .set_aspect_ratio(ratio);
                ToolUpdateResult::Redraw
            }
            ToolbarEvent::CropDimensionsSet { width, height } => {
                self.tools
                    .get_crop_tool()
                    .borrow_mut()
                    .set_dimensions(width as f32, height as f32);
                ToolUpdateResult::Redraw
            }
            ToolbarEvent::CropBgColorChanged(bg) => {
                self.tools.get_crop_tool().borrow_mut().set_bg_color(bg);
                ToolUpdateResult::Redraw
            }
            ToolbarEvent::FlipHorizontal => {
                self.renderer.flip_image_horizontal();
                ToolUpdateResult::Redraw
            }
            ToolbarEvent::RotateImage => {
                // Snapshot the on-screen scale BEFORE the rotation
                // so the post-rotation `reset_size` can restore it
                // verbatim instead of dropping back to auto-fit.
                // `current_render_scale` returns the same number
                // the zoom indicator was showing, so the user's
                // "22%" stays 22% across the turn.
                let preserved_zoom = self.renderer.current_render_scale();
                if let Some((new_w, new_h)) = self.renderer.rotate_image_ccw() {
                    let crop_tool = self.tools.get_crop_tool();
                    let mut ct = crop_tool.borrow_mut();
                    // Update bounds the snap-to-edges + seed paths
                    // read from, then reseed so the crop rect lands
                    // fresh on the rotated image (the old rect's
                    // (pos, size) refer to coordinates that no
                    // longer exist in the new orientation).
                    // `emit_resize: false` because we emit our own
                    // zoom-scaled ContentSizeChanged below — the
                    // seed path's native-pixel emit would otherwise
                    // win the race and blow the window up to the
                    // rotated image's full size.
                    ct.set_image_bounds(crate::math::Vec2D::new(new_w, new_h));
                    ct.revert_to_seed(false);
                    drop(ct);
                    // Drop the drag offset (the old offset points
                    // at image-space coords that no longer exist
                    // in the rotated orientation) but keep the
                    // pre-rotation zoom so the on-screen scale
                    // doesn't snap back to auto-fit mid-edit.
                    self.renderer.reset_size(preserved_zoom);
                    sender
                        .output_sender()
                        .emit(SketchBoardOutput::ImageDimensionsChanged {
                            width: new_w as i32,
                            height: new_h as i32,
                        });
                    // Re-fit the window around the rotated content
                    // AT THE PRESERVED ZOOM, not at native pixels.
                    // Otherwise a 22 % preview rotated from
                    // portrait → landscape would blow the window
                    // up to the full landscape pixel size and the
                    // user would see a tiny image floating in a
                    // huge canvas. Scaling by `preserved_zoom`
                    // means the window only grows by the small
                    // aspect-flip delta in on-screen pixels.
                    sender
                        .output_sender()
                        .emit(SketchBoardOutput::ContentSizeChanged {
                            width: new_w * preserved_zoom,
                            height: new_h * preserved_zoom,
                        });
                }
                ToolUpdateResult::Redraw
            }
            ToolbarEvent::ResizeImage { width, height } => {
                if let Some((new_w, new_h)) = self.renderer.resize_image(width, height) {
                    let crop_tool = self.tools.get_crop_tool();
                    let mut ct = crop_tool.borrow_mut();
                    ct.set_image_bounds(crate::math::Vec2D::new(new_w, new_h));
                    ct.revert_to_seed(true);
                    drop(ct);
                    // Drop any prior user zoom so the renderer's
                    // auto-fit-with-padding cascade re-engages for the
                    // new image size — same fit-to-screen treatment a
                    // fresh screenshot gets. `revert_to_seed` already
                    // emitted ContentSizeChanged to grow the window to
                    // 90 % viewport; auto-fit handles the case where
                    // the resized image still exceeds the window.
                    self.renderer.reset_size(0.0);
                    sender
                        .output_sender()
                        .emit(SketchBoardOutput::ImageDimensionsChanged {
                            width: new_w as i32,
                            height: new_h as i32,
                        });
                }
                ToolUpdateResult::Redraw
            }
            /*            ToolbarEvent::CropDimensionsUpdated(dimensions) => {
                sender
                    .output_sender()
                    .emit(SketchBoardOutput::DimensionsUpdate(Some(dimensions)));
                ToolUpdateResult::Unmodified
            }*/
        }
    }

    fn handle_text_commit(
        &mut self,
        event: TextEventMsg,
        sender: ComponentSender<Self>,
    ) -> ToolUpdateResult {
        match event {
            TextEventMsg::Commit(txt) => {
                // NOTE:
                // If there's an IMContext binded to the controller, single letter-key events will
                // always go through it first, denying a bypass, so the only way we can do single-key
                // bindings is to act upon the IMMulticontext's commit event itself.
                // NOTE:
                // Here we're basically bypassing the IMMulticontext. If the text tool is active
                // and wants text inputs, we're interested in the single-letter keypress as a text character.
                // If not, we parse it as a shortcut event.
                if self.active_tool_type() == Tools::Text
                    && self.active_tool.borrow().input_enabled()
                {
                    sender.input(SketchBoardInput::new_text_event(TextEventMsg::Commit(
                        txt.to_string(),
                    )));
                } else if txt == "f" || txt == "F" {
                    // `F` toggles Fill Shape. Handled here (rather
                    // than in the key-pressed chain below) because
                    // GTK's IM context consumes printable letter
                    // keys before they reach the EventControllerKey
                    // path — same reason `p`, `c`, etc. tool
                    // shortcuts are matched off `TextEventMsg::Commit`.
                    // Route via the existing `ToggleFill` event so
                    // sketch_board's `&mut self` handler does the
                    // flip + dispatch, and follow up with a sync
                    // signal to the toolbar (the button-click path
                    // updates its own mirror locally; from a
                    // keyboard toggle, we have to push instead).
                    sender.input(SketchBoardInput::ToolbarEvent(
                        ToolbarEvent::ToggleFill,
                    ));
                    sender.input(SketchBoardInput::SyncFillToToolbar);
                } else if let Some(ch) = txt.chars().next()
                    && let Some(tool) = APP_CONFIG.read().keybinds().get_tool(ch)
                {
                    // Double-press cycle: if the user presses the
                    // SAME tool key twice within TOOL_CYCLE_MS AND
                    // the tool was already active when the second
                    // press fired, advance the tool's style variant
                    // instead of re-selecting. First press always
                    // behaves as a normal select — guards against
                    // accidental cycling from a single tap.
                    let now = std::time::Instant::now();
                    let is_cycle = matches!(
                        self.last_tool_press,
                        Some((prev_ch, prev_t))
                            if prev_ch == ch
                                && now.duration_since(prev_t).as_millis()
                                    <= TOOL_CYCLE_MS as u128
                                && self.active_tool_type() == tool
                    );
                    self.last_tool_press = Some((ch, now));
                    if is_cycle {
                        self.cycle_tool_style(tool, &sender);
                        // Clear the press history so a THIRD quick
                        // press doesn't double-cycle — each cycle
                        // needs a fresh double-tap.
                        self.last_tool_press = None;
                    } else {
                        sender.input(SketchBoardInput::ToolbarEvent(
                            ToolbarEvent::ToolSelected(tool),
                        ));
                        sender
                            .output_sender()
                            .emit(SketchBoardOutput::ToolSwitchShortcut(tool));
                    }
                } else if let Some(hotkey_digit) =
                    txt.chars().next().and_then(|char| char.to_digit(10))
                {
                    // Crop tool claims 1–5 as quadrant presets
                    // (1=UL, 2=UR, 3=LL, 4=LR, 5=centered quarter).
                    // Combined with Shift+arrow nudging, this lets
                    // the whole crop flow run from the keyboard
                    // without ever touching the mouse. Other digits
                    // (6–9, 0) and 1–5 outside Crop still fall
                    // through to the color-picker shortcut.
                    let crop_consumed = if self.active_tool_type() == Tools::Crop
                        && (1..=5).contains(&hotkey_digit)
                    {
                        let crop_tool = self.tools.get_crop_tool();
                        let applied =
                            crop_tool.borrow_mut().apply_quadrant_preset(hotkey_digit);
                        if applied {
                            self.renderer.request_render(&[]);
                        }
                        applied
                    } else {
                        false
                    };
                    if !crop_consumed {
                        let index_digit = if hotkey_digit == 0 {
                            9
                        } else {
                            hotkey_digit - 1
                        };
                        if APP_CONFIG.read().color_palette().palette().len()
                            >= (index_digit + 1) as usize
                        {
                            sender
                                .output_sender()
                                .emit(SketchBoardOutput::ColorSwitchShortcut(
                                    index_digit as u64,
                                ));
                        }
                    }
                }
            }
            TextEventMsg::Preedit {
                text,
                cursor_chars,
                spans,
            } => {
                if self.active_tool_type() == Tools::Text
                    && self.active_tool.borrow().input_enabled()
                {
                    sender.input(SketchBoardInput::new_text_event(TextEventMsg::Preedit {
                        text,
                        cursor_chars,
                        spans,
                    }));
                }
            }
            TextEventMsg::PreeditEnd => {
                if self.active_tool_type() == Tools::Text
                    && self.active_tool.borrow().input_enabled()
                {
                    sender.input(SketchBoardInput::new_text_event(TextEventMsg::PreeditEnd));
                }
            }
        }
        ToolUpdateResult::Unmodified
    }

    pub fn active_tool_type(&self) -> Tools {
        self.active_tool.borrow().get_tool_type()
    }

    /// If the pointer tool's selection has changed since we last
    /// synced the toolbar — either the selected drawable id flipped
    /// or its sizing was mutated (scroll-resize) — emit
    /// `SelectionStyleChanged` with the new drawable's style for the
    /// single-select case, and `SelectionMultiAgreement` for the
    /// multi-select case (per-property "do they all share this?"
    /// report driving slider enable/disable + value).
    fn sync_toolbar_to_selection(&mut self, sender: &ComponentSender<Self>) {
        let pointer_tool = self.tools.get(&Tools::Pointer);
        let selected = pointer_tool.borrow().selected_drawables();
        let new_style = if selected.len() == 1 {
            self.renderer
                .clone_drawable(selected[0])
                .and_then(|d| d.style())
                .map(|s| (selected[0], s))
        } else {
            None
        };
        let new_key = new_style
            .as_ref()
            .map(|(id, s)| (*id, s.size));
        let single_changed = new_key != self.last_synced_selection;
        // Detect multi-state transitions even when both the previous
        // and current sync had a `None` key (multi → empty looks like
        // None → None on the cache alone). Without this, ToolChanged-
        // initiated multi→empty transitions wouldn't fire
        // `SyncToToolDefault`, leaving stale multi-only flags like
        // `brush_smooth_slider_show_for_multi` stuck and the
        // smoothness slider visible after the user switched tools.
        let was_multi = self.last_was_multi_selection;
        let is_multi = selected.len() >= 2;
        self.last_was_multi_selection = is_multi;
        if single_changed || was_multi != is_multi {
            self.last_synced_selection = new_key;
            sender
                .output_sender()
                .emit(SketchBoardOutput::SelectionStyleChanged(
                    new_style.map(|(_, s)| s),
                ));
        }
        // Multi-select per-property agreement: when 2+ drawables are
        // selected, walk each and check whether they all share the
        // same size / smooth_level. Some(v) for "yes, they share v";
        // None for "they disagree, disable that control." Always
        // emitted in the multi case, regardless of `single_changed` —
        // the agreement can shift while the single-select cache key
        // stays `None` (e.g., scroll-resize on a multi-selection
        // mutates values without changing the selected ids).
        if selected.len() >= 2 {
            let drawables: Vec<Box<dyn Drawable>> = selected
                .iter()
                .filter_map(|id| self.renderer.clone_drawable(*id))
                .collect();
            let shared_size = drawables
                .first()
                .and_then(|d| d.style())
                .map(|s| s.size)
                .filter(|first_size| {
                    drawables
                        .iter()
                        .all(|d| d.style().map(|s| s.size) == Some(*first_size))
                });
            // smooth_level only exists on brush strokes. Three cases:
            // every drawable is a brush + all same level → Shared;
            // every drawable is a brush + differing levels → Mixed;
            // any non-brush in the selection → NotApplicable (hide
            // the slider, since smoothness is meaningless for the
            // current set).
            let levels: Vec<Option<usize>> =
                drawables.iter().map(|d| d.smooth_level()).collect();
            let smooth = if levels.iter().any(|l| l.is_none()) {
                SmoothLevelMulti::NotApplicable
            } else {
                let first = levels[0].unwrap();
                if levels.iter().all(|l| *l == Some(first)) {
                    SmoothLevelMulti::Shared(first)
                } else {
                    SmoothLevelMulti::Mixed
                }
            };
            sender
                .output_sender()
                .emit(SketchBoardOutput::SelectionMultiAgreement {
                    size: shared_size,
                    smooth,
                });
        }
        // If the just-selected drawable carries a variant (text
        // background, arrow geometry, blur algorithm), push that
        // value into the toolbar so its menu / dropdown reflects
        // the selected drawable. Lets the user click between two
        // arrows / blurs / texts and have the toolbar agree, then
        // double-tap or click the picker to cycle from there.
        // Silent path — no toast, no re-apply.
        if selected.len() == 1
            && let Some(d) = self.renderer.clone_drawable(selected[0])
        {
            if let Some(bg) = d.text_background() {
                sender
                    .output_sender()
                    .emit(SketchBoardOutput::SelectionTextBackgroundChanged(bg));
            }
            if let Some(s) = d.arrow_style() {
                sender
                    .output_sender()
                    .emit(SketchBoardOutput::SelectionArrowStyleChanged(s));
            }
            if let Some(s) = d.blur_style() {
                sender
                    .output_sender()
                    .emit(SketchBoardOutput::SelectionBlurStyleChanged(s));
            }
            if let Some(level) = d.smooth_level() {
                sender
                    .output_sender()
                    .emit(SketchBoardOutput::SelectionBrushPostSmoothChanged(level));
            }
            // Auto-switch the active tool to whatever created the
            // selected drawable so the StyleToolbar's tool-specific
            // controls (arrow style chip, blur algorithm dropdown,
            // text-background DropDown, etc.) become visible.
            // Crop is excluded because entering Crop is a dedicated,
            // user-initiated mode, not something a selection should
            // trigger.
            if let Some(target) = d.tool_type()
                && target != Tools::Crop
                && target != self.active_tool_type()
            {
                // Selection-driven auto-switch: call `switch_active_tool`
                // directly (NOT via `ToolbarEvent::ToolSelected`) so the
                // just-made selection that triggered this branch isn't
                // cleared by the user-driven deselect at that arm's top.
                let _ = self.switch_active_tool(target, sender.clone());
            }
        }
    }

    /// Convert an accumulated scroll-delta into N discrete +1 (smaller)
    /// or -1 (bigger) steps. dy carries the sign GTK reports — negative
    /// means scroll-up, which we want to map to "bigger". The
    /// accumulator (`self.scroll_resize_accum`) is the per-instance
    /// buffer so trackpads (many small dy events) accumulate to the
    /// same number of steps a notched wheel (|dy|=1.0) emits per
    /// click. Returns the signed step count, where +1 = step_up and
    /// -1 = step_down.
    fn drain_scroll_resize_steps(&mut self, dy: f32) -> i32 {
        // Reset on direction reversal so a flick the other way doesn't
        // have to chew through the previous direction's leftover.
        if self.scroll_resize_accum != 0.0 && (self.scroll_resize_accum.signum() != (-dy).signum())
        {
            self.scroll_resize_accum = 0.0;
        }
        // GTK reports dy>0 for scroll-down, dy<0 for scroll-up. We want
        // scroll-up → step_up (bigger), so negate the sign.
        self.scroll_resize_accum += -dy;
        let mut steps = 0;
        while self.scroll_resize_accum >= 1.0 {
            self.scroll_resize_accum -= 1.0;
            steps += 1;
        }
        while self.scroll_resize_accum <= -1.0 {
            self.scroll_resize_accum += 1.0;
            steps -= 1;
        }
        steps
    }

    /// Resize all currently-selected drawables by `dy`-derived steps.
    /// Falls through cleanly when the accumulated dy hasn't reached a
    /// full step yet — typical for trackpad scrolling.
    fn scroll_resize_selection(
        &mut self,
        selected: &[DrawableId],
        dy: f32,
        outer_sender: &ComponentSender<Self>,
    ) {
        let steps = self.drain_scroll_resize_steps(dy);
        if steps == 0 {
            return;
        }
        let mut updates: Vec<(DrawableId, Box<dyn Drawable>)> = Vec::with_capacity(selected.len());
        for id in selected {
            let Some(mut d) = self.renderer.clone_drawable(*id) else {
                continue;
            };
            let Some(mut s) = d.style() else {
                continue;
            };
            let new_size = apply_size_steps(s.size, steps);
            if new_size == s.size {
                continue;
            }
            s.size = new_size;
            d.set_style(s);
            updates.push((*id, d));
        }
        match updates.len() {
            0 => {}
            1 => {
                let (id, d) = updates.pop().unwrap();
                self.renderer.modify(id, d);
                self.refresh_screen();
            }
            _ => {
                self.renderer.modify_many(updates);
                self.refresh_screen();
            }
        }
        self.sync_toolbar_to_selection(outer_sender);
    }

    /// Pick which tool's "alt" control the Ctrl+Shift+wheel gesture
    /// should drive. With a live selection that's all-one-tool, the
    /// alt follows the selection (a Pointer-tool multi-brush selection
    /// targets `Brush`). With an empty selection (or a mixed-tool
    /// selection that doesn't agree), fall back to the active tool.
    fn alt_slider_target_tool(&self) -> Option<Tools> {
        let selected = self
            .tools
            .get(&Tools::Pointer)
            .borrow()
            .selected_drawables();
        if selected.is_empty() {
            return Some(self.active_tool_type());
        }
        // All selected drawables share a tool_type → use it.
        let tools: Vec<Option<Tools>> = selected
            .iter()
            .map(|id| {
                self.renderer
                    .clone_drawable(*id)
                    .and_then(|d| d.tool_type())
            })
            .collect();
        let first = tools.first().and_then(|t| *t)?;
        if tools.iter().all(|t| *t == Some(first)) {
            Some(first)
        } else {
            // Mixed selection — no single alt control makes sense.
            None
        }
    }

    /// Bump or cycle the active tool's "alternate" control — the
    /// per-tool widget in the bottom-right cluster: brush smoothness
    /// slider, spotlight darkness slider, highlighter opacity slider,
    /// or one of the dropdown pickers (arrow style, blur style, text
    /// background). Applies to a live selection where the property is
    /// per-drawable, and persists / updates the toolbar widget either
    /// way. Tools without an alt control (Pointer / Crop / Rect /
    /// Ellipse) silently absorb the gesture so an accidental
    /// Ctrl+Shift+wheel doesn't surprise-pan the canvas.
    fn scroll_alt_slider(&mut self, dy: f32, outer_sender: &ComponentSender<Self>) {
        let steps = self.drain_scroll_resize_steps(dy);
        if steps == 0 {
            return;
        }
        // Dispatch by the *target* tool — when the user has a
        // selection, the alt control follows the selected drawables
        // (a Pointer-tool multi-brush selection should bump
        // smoothness, not "Pointer has no alt control"). With no
        // selection, fall back to the active tool's alt control for
        // the next-stroke default.
        let target_tool = self.alt_slider_target_tool();
        match target_tool {
            Some(Tools::Brush) => {
                // Integer 0..=6 — one notch per wheel step. Seed
                // depends on whether the user has brush strokes
                // selected: with selection, start from the selected
                // brush's current level (so wheel-up nudges UP from
                // what they're editing); without selection, start
                // from APP_CONFIG (the next-stroke default).
                let selected_ids = self
                    .tools
                    .get(&Tools::Pointer)
                    .borrow()
                    .selected_drawables();
                let levels: Vec<Option<usize>> = selected_ids
                    .iter()
                    .map(|id| {
                        self.renderer
                            .clone_drawable(*id)
                            .and_then(|d| d.smooth_level())
                    })
                    .collect();
                let cur = if selected_ids.is_empty() {
                    APP_CONFIG.read().brush_post_smooth_iterations()
                } else {
                    // Refuse to operate on a mixed-or-non-brush
                    // selection — same gate as the slider's
                    // `brush_smooth_slider_disabled` state.
                    if levels.iter().any(|l| l.is_none()) {
                        return;
                    }
                    let first = levels[0].unwrap();
                    if !levels.iter().all(|l| *l == Some(first)) {
                        return;
                    }
                    first
                };
                let new_val_i = (cur as i32 + steps).clamp(0, 6);
                if new_val_i == cur as i32 {
                    return;
                }
                let new_val = new_val_i as usize;
                // Apply directly via renderer.modify* — the
                // `apply_brush_smooth_to_selection` helper returns
                // a `ToolUpdateResult` for callers that thread it
                // back to the framework's update-loop, but
                // `scroll_alt_slider` is invoked from a side path
                // (input event handler) where that result would be
                // discarded — which would leave the drawable
                // unmodified and the sync_toolbar_to_selection at
                // the end of update() would bounce the slider back
                // to the old value.
                let mut updates: Vec<(DrawableId, Box<dyn Drawable>)> = Vec::new();
                for id in selected_ids {
                    if let Some(mut d) = self.renderer.clone_drawable(id)
                        && d.smooth_level().is_some()
                    {
                        d.set_smooth_level(new_val);
                        updates.push((id, d));
                    }
                }
                match updates.len() {
                    0 => {
                        // No selection or no brush-typed drawable —
                        // wheel updates the next-stroke default. Also
                        // record into the session cache so the next
                        // Brush re-entry restores this value when
                        // sticky-defaults is on.
                        APP_CONFIG
                            .write()
                            .set_brush_post_smooth_iterations(new_val);
                        self.session_brush_smooth = Some(new_val);
                    }
                    1 => {
                        let (id, d) = updates.pop().unwrap();
                        self.renderer.modify(id, d);
                    }
                    _ => {
                        self.renderer.modify_many(updates);
                    }
                }
                outer_sender
                    .output_sender()
                    .emit(SketchBoardOutput::BrushPostSmoothReset(new_val));
                self.refresh_screen();
            }
            Some(Tools::Spotlight) => {
                // 0.10..=0.90 — coarse stride per notch so a typical
                // wrist-flick of 3–4 clicks traverses meaningful range.
                const SPOTLIGHT_STEP: f32 = 0.05;
                let cur = self.style.spotlight_darkness;
                let new_val = (cur + steps as f32 * SPOTLIGHT_STEP).clamp(0.10, 0.90);
                if (new_val - cur).abs() < f32::EPSILON {
                    return;
                }
                self.style.spotlight_darkness = new_val;
                self.renderer.set_spotlight_darkness(new_val);
                outer_sender
                    .output_sender()
                    .emit(SketchBoardOutput::SpotlightDarknessReset(new_val));
                self.refresh_screen();
            }
            Some(Tools::Highlighter) => {
                // 0.10..=1.00 — same stride logic as spotlight.
                const HIGHLIGHTER_STEP: f32 = 0.05;
                let cur = self.style.highlighter_opacity;
                let new_val = (cur + steps as f32 * HIGHLIGHTER_STEP).clamp(0.10, 1.00);
                if (new_val - cur).abs() < f32::EPSILON {
                    return;
                }
                self.style.highlighter_opacity = new_val;
                // Wheel updates the next-stroke default in addition
                // to any selected highlighters below — mirror the
                // slider-driven path and record into the session
                // cache for sticky-defaults restoration.
                self.session_highlighter_opacity = Some(new_val);
                outer_sender
                    .output_sender()
                    .emit(SketchBoardOutput::HighlighterOpacityReset(new_val));
                // Opacity-only update on selected highlighter
                // drawables. Skipping the full dispatch_style_change
                // path (which would replace the entire style on each
                // selected drawable) so non-highlighter selection
                // members keep their original style.
                let selected_ids = self
                    .tools
                    .get(&Tools::Pointer)
                    .borrow()
                    .selected_drawables();
                let mut updates: Vec<(DrawableId, Box<dyn Drawable>)> = Vec::new();
                for id in selected_ids {
                    if let Some(mut d) = self.renderer.clone_drawable(id)
                        && d.tool_type() == Some(Tools::Highlighter)
                        && let Some(mut style) = d.style()
                    {
                        style.highlighter_opacity = new_val;
                        d.set_style(style);
                        updates.push((id, d));
                    }
                }
                match updates.len() {
                    0 => {}
                    1 => {
                        let (id, d) = updates.pop().unwrap();
                        self.renderer.modify(id, d);
                    }
                    _ => {
                        self.renderer.modify_many(updates);
                    }
                }
                // Push to the active Highlighter tool so the in-flight
                // and next strokes use the new opacity.
                self.active_tool
                    .borrow_mut()
                    .handle_event(ToolEvent::StyleChanged(self.style));
                self.refresh_screen();
            }
            Some(Tools::Arrow) => {
                use crate::tools::ArrowStyle;
                let order = [
                    ArrowStyle::Standard,
                    ArrowStyle::Pointy,
                    ArrowStyle::Curved,
                    ArrowStyle::Double,
                ];
                let current = self.cycle_seed_arrow();
                let next = wrap_cycle(&order, current, steps);
                if next == current {
                    return;
                }
                // Emit `*Cycled` rather than synthesizing the
                // `*Selected` event via `handle_toolbar_event`:
                // *Selected updates sketch_board state (tool style,
                // save, toast, apply-to-selection) but DOES NOT
                // update the toolbar's dropdown widget visually.
                // *Cycled rides through main.rs → toolbar
                // `SetArrowStyle { emit_upstream: true }`, which both
                // refreshes the dropdown's displayed value AND
                // re-emits *Selected upstream so the sketch_board
                // side effects still happen.
                outer_sender
                    .output_sender()
                    .emit(SketchBoardOutput::ArrowStyleCycled(next));
            }
            Some(Tools::Blur) => {
                use crate::tools::BlurStyle;
                let order = [
                    BlurStyle::Pixelate,
                    BlurStyle::SecureBlur,
                    BlurStyle::Gaussian,
                    BlurStyle::BlackOut,
                ];
                let current = self.cycle_seed_blur();
                let next = wrap_cycle(&order, current, steps);
                if next == current {
                    return;
                }
                outer_sender
                    .output_sender()
                    .emit(SketchBoardOutput::BlurStyleCycled(next));
            }
            Some(Tools::Text) => {
                use crate::tools::TextBackground;
                let order = [TextBackground::Rounded, TextBackground::Plain];
                let current = self.cycle_seed_text();
                let next = wrap_cycle(&order, current, steps);
                if next == current {
                    return;
                }
                outer_sender
                    .output_sender()
                    .emit(SketchBoardOutput::TextBackgroundCycled(next));
            }
            Some(Tools::Rectangle) | Some(Tools::Ellipse) => {
                // Rect/Ellipse alt control is the fill toggle.
                // Map wheel direction to a definite state (up = filled,
                // down = outline) rather than a step-by-step flip —
                // a multi-notch wheel would otherwise zig-zag the
                // state and surprise the user. The toggle is no-op
                // when already in the requested state.
                let want_filled = steps > 0;
                if want_filled == self.style.fill {
                    return;
                }
                let _ = self.handle_toolbar_event(
                    ToolbarEvent::ToggleFill,
                    outer_sender.clone(),
                );
            }
            _ => {
                // No alt control for the remaining tools (Pointer /
                // Crop). Silently absorb the gesture so the user
                // doesn't get a surprise pan when their fingers slip
                // onto Ctrl+Shift over a non-cluster tool.
            }
        }
    }

    /// Bump the annotation multiplier (`style.annotation_size_factor`)
    /// by `dy`-derived steps. Applies the new factor to every selected
    /// drawable, to the active tool's next-stroke style, and persists
    /// it to state.toml (the multiplier no longer has a toolbar surface,
    /// so persistence on every adjust keeps a fresh launch picking up
    /// the user's last in-session value). A toast announces the new
    /// value since there's no pill to read it off of anymore.
    fn scroll_annotation_multiplier(&mut self, dy: f32, outer_sender: &ComponentSender<Self>) {
        let steps = self.drain_scroll_resize_steps(dy);
        if steps == 0 {
            return;
        }
        // 0.1-unit detents with a 0.1..=10.0 clamp — mirrors the
        // Preferences SpinButton's adjustment so canvas-side bumps and
        // Preferences-side edits land on the same grid.
        const ANNOTATION_STEP: f32 = 0.1;
        const ANNOTATION_MIN: f32 = 0.10;
        const ANNOTATION_MAX: f32 = 10.0;
        let cur = self.style.annotation_size_factor;
        let new_val =
            (cur + steps as f32 * ANNOTATION_STEP).clamp(ANNOTATION_MIN, ANNOTATION_MAX);
        // Round to the nearest step so trackpad accumulation doesn't
        // park between detents.
        let new_val = (new_val / ANNOTATION_STEP).round() * ANNOTATION_STEP;
        if (new_val - cur).abs() < f32::EPSILON {
            return;
        }
        self.style.annotation_size_factor = new_val;
        // Apply factor-only updates manually to every selected
        // drawable. Going through `dispatch_style_change` would
        // replace other style fields on selected drawables too.
        let selected_ids = self
            .tools
            .get(&Tools::Pointer)
            .borrow()
            .selected_drawables();
        let mut updates: Vec<(DrawableId, Box<dyn Drawable>)> = Vec::new();
        for id in selected_ids {
            if let Some(mut d) = self.renderer.clone_drawable(id)
                && let Some(mut style) = d.style()
            {
                if (style.annotation_size_factor - new_val).abs() < f32::EPSILON {
                    continue;
                }
                style.annotation_size_factor = new_val;
                d.set_style(style);
                updates.push((id, d));
            }
        }
        match updates.len() {
            0 => {}
            1 => {
                let (id, d) = updates.pop().unwrap();
                self.renderer.modify(id, d);
            }
            _ => {
                self.renderer.modify_many(updates);
            }
        }
        // Push the new style to the active tool so its next stroke
        // picks up the new factor.
        self.active_tool
            .borrow_mut()
            .handle_event(ToolEvent::StyleChanged(self.style));
        // Persist + update APP_CONFIG so the value survives a
        // restart and any subsequent welcome-dialog logic sees the
        // current live value.
        crate::state::save_annotation_size_factor(new_val);
        APP_CONFIG.write().set_annotation_size_factor(new_val);
        // The user has no on-screen value indicator now that the pill
        // is gone — surface a transient toast announcing the new
        // factor so the bump isn't silent.
        outer_sender
            .output_sender()
            .emit(SketchBoardOutput::ShowCycleToast(format!(
                "Annotation size: {new_val:.1}×"
            )));
        self.refresh_screen();
    }

    /// Bump the active tool's `style.size` by `dy`-derived steps.
    /// Notifies the toolbar so the slider stays in sync.
    fn scroll_resize_tool_size(&mut self, dy: f32, outer_sender: &ComponentSender<Self>) {
        let steps = self.drain_scroll_resize_steps(dy);
        if steps == 0 {
            return;
        }
        let new_size = apply_size_steps(self.style.size, steps);
        if new_size == self.style.size {
            return;
        }
        self.style.size = new_size;
        self.dispatch_style_change();
        outer_sender
            .output_sender()
            .emit(SketchBoardOutput::ToolSizeChanged(new_size));
    }

    /// Dispatch a StyleChanged event so the toolbar's color/size/fill controls
    /// affect both future drawings (via the active tool) and any current
    /// selection (via the pointer tool's implicit selection state).
    fn dispatch_style_change(&mut self) -> ToolUpdateResult {
        let active_type = self.active_tool_type();
        let pointer_result = if active_type != Tools::Pointer {
            self.tools
                .get(&Tools::Pointer)
                .borrow_mut()
                .handle_event(ToolEvent::StyleChanged(self.style))
        } else {
            ToolUpdateResult::Unmodified
        };
        let active_result = self
            .active_tool
            .borrow_mut()
            .handle_event(ToolEvent::StyleChanged(self.style));

        // Brush/Highlighter cursors are sized from the active style;
        // a size change must rebuild the cursor immediately so the
        // user sees the new diameter before they move the mouse. Skip
        // for tools without a custom cursor — apply_idle_cursor
        // handles that path correctly.
        if matches!(active_type, Tools::Brush | Tools::Highlighter) {
            self.apply_idle_cursor();
        }

        // If the pointer applied the change to a selected drawable, that
        // result is what should land on the undo stack.
        match pointer_result {
            ToolUpdateResult::ModifyDrawable(_, _)
            | ToolUpdateResult::ModifyDrawables(_)
            | ToolUpdateResult::ModifyDrawableCoalesce(_, _)
            | ToolUpdateResult::ModifyDrawablesCoalesce(_) => pointer_result,
            _ => active_result,
        }
    }

    /// Switch the active tool to Text and resume editing the committed
    /// drawable identified by `id`. Triggered by a double-click on a
    /// Text drawable (PointerTool emits `EditTextDrawable`). The
    /// committed drawable stays in the stack — `TextTool` marks it as
    /// the edit target via `dragging_drawable_id` so the renderer hides
    /// the original while the editing copy is shown.
    fn enter_text_edit_mode(&mut self, id: DrawableId, sender: ComponentSender<Self>) {
        let Some(drawable) = self.renderer.clone_drawable(id) else {
            return;
        };
        // Reuse the toolbar-switch path so all the side effects (focus,
        // cursor, output notifications) happen exactly as on a manual
        // tool change.
        self.handle_toolbar_event(ToolbarEvent::ToolSelected(Tools::Text), sender);
        let text_tool = self.tools.get(&Tools::Text);
        text_tool.borrow_mut().enter_text_edit_mode(id, drawable);
        self.refresh_screen();
    }

    /// Update the canvas cursor based on what the mouse is hovering over.
    /// Called on PointerPos events so users see "grab" over existing shapes
    /// and resize cursors over handles. Drawing tools (anything except
    /// Pointer / Crop) show "crosshair" when not over an existing shape so
    /// the canvas hints where new geometry will land. Brush and Highlighter
    /// override the crosshair with a custom double-ring cursor sized to
    /// their stroke width (see `crate::ui::cursor`).
    fn update_hover_cursor(&mut self, image_pos: Vec2D) {
        self.last_hover_image_pos = Some(image_pos);
        let pointer_tool = self.tools.get(&Tools::Pointer);
        let pt = pointer_tool.borrow();
        if pt.dragging_drawable_id().is_some() {
            // Hide the cursor entirely during a resize-handle drag so
            // the user can see exactly where the dragged edge / corner
            // lands. Body (move) drags keep the cursor visible — the
            // user wants to track where the shape's reference point is
            // moving to.
            if pt.is_resizing() {
                self.renderer.set_cursor_from_name(Some("none"));
            }
            return;
        }

        // 0. Crop tool is the active tool — its overlay sits on top of
        //    everything else and has its own affordance vocabulary.
        //    Handle → `pointer` (the link-style hand cursor signaling
        //    "you can interact with this"); body → `grab` (signaling
        //    "click and drag to move the crop"). The crop drawable
        //    isn't in the regular stack so the hit_test below would
        //    miss it.
        let mut cursor: Option<&'static str> = None;
        if self.active_tool_type() == Tools::Crop {
            let crop_tool = self.tools.get_crop_tool();
            let ct = crop_tool.borrow();
            if let Some(crop) = ct.get_crop()
                && !crop.is_committed()
            {
                // Pass the current image→canvas scale so the handle
                // hit zone stays at a constant CSS-pixel size — without
                // this, an auto-fit-scaled-down screenshot has tiny
                // hit zones that miss the visible handle bracket.
                let scale = self.renderer.current_render_scale();
                cursor = match crop.hit_kind(image_pos, scale) {
                    Some(crate::tools::CropHit::Handle(h)) => Some(h.resize_cursor()),
                    Some(crate::tools::CropHit::Body) => Some("grab"),
                    None => None,
                };
            }
        }

        // 1. Hovering a handle of the current selection wins.
        if cursor.is_none()
            && let Some(id) = pt.selected_drawable()
            && let Some(drawable) = self.renderer.clone_drawable(id)
        {
            for h in drawable.handles() {
                if h.pos.distance_to(&image_pos) <= h.hit_radius {
                    cursor = Some(cursor_for_handle(h.id));
                    break;
                }
            }
        }
        drop(pt);

        // 1.5. Editing-mode handles from the active tool (e.g. Text while
        //      editing). Reuses the same resize-cursor mapping.
        if cursor.is_none() {
            let at = self.active_tool.borrow();
            for h in at.editing_handles() {
                if h.pos.distance_to(&image_pos) <= h.hit_radius {
                    cursor = Some(cursor_for_handle(h.id));
                    break;
                }
            }
        }

        // 1.6. Inside the active tool's editing body (e.g. Text wrap
        //      area) → i-beam, signaling "click here to place the
        //      caret". Lives between the handle and drawable checks so
        //      the resize cursor still wins on handle hover.
        if cursor.is_none() {
            let at = self.active_tool.borrow();
            if let Some(body) = at.editing_body_rect()
                && body.contains(image_pos)
            {
                cursor = Some("text");
            }
        }

        // 2. Otherwise, a drawable under the pointer → grab — but only
        //    when the active tool would actually grab it. With a non-
        //    Pointer drawing tool active, a body click on a different-
        //    typed drawable falls through so the user can place a new
        //    annotation on top; the cursor follows that semantics and
        //    stays on the tool's default (crosshair / custom).
        if cursor.is_none()
            && let Some(id) = self.renderer.hit_test(image_pos, crate::tools::HIT_TOLERANCE)
        {
            let active = self.active_tool_type();
            let same_type = active == Tools::Pointer
                || self
                    .renderer
                    .clone_drawable(id)
                    .and_then(|d| d.tool_type())
                    .map(|t| t == active)
                    .unwrap_or(false);
            if same_type {
                cursor = Some("grab");
            }
        }

        // 3. Tool-specific default for empty canvas. Brush/Highlighter
        //    take a custom-rendered cursor that previews stroke
        //    geometry; everything else falls through to a named cursor.
        //    For Highlighter, also check the detected text band at the
        //    current pointer y — when the pointer is over a band, the
        //    cursor's height matches the band's height AND its render
        //    position is anchored to the band's center (via the
        //    hotspot offset). That way the preview capsule sits over
        //    the text row the click would highlight, no matter where
        //    inside the band the pointer actually is.
        if cursor.is_none() {
            let (band_height, band_v_offset) =
                if self.active_tool_type() == Tools::Highlighter {
                    // While a drag is in flight, the tool's
                    // `locked_text_band()` (set at BeginDrag in
                    // TextLocked mode) takes precedence — the
                    // cursor stays at the band the stroke started
                    // on no matter where the pointer wanders.
                    // When idle, the current `highlighter_style()`
                    // decides whether to even attempt a band lookup:
                    //   * TextLocked → query `detect_local_band` and
                    //     anchor the cursor to that band.
                    //   * Normal → no band, no anchor — the cursor
                    //     is the freehand style.size-derived capsule
                    //     centered on the pointer.
                    let active_tool = self.active_tool.borrow();
                    let locked = active_tool.locked_text_band();
                    let style = active_tool
                        .highlighter_style()
                        .unwrap_or_default();
                    drop(active_tool);
                    let band = match (locked, style) {
                        (Some(b), _) => Some(b),
                        (None, crate::tools::HighlighterStyle::TextLocked) => {
                            crate::text_bands::detect_local_band(
                                image_pos.x,
                                image_pos.y,
                            )
                        }
                        (None, crate::tools::HighlighterStyle::Normal) => None,
                    };
                    match band {
                        Some(b) => {
                            let pad =
                                2.0 * b.height() * crate::text_bands::BAND_PAD_PERCENT_PER_SIDE;
                            (Some(b.height() + pad), b.center_y() - image_pos.y)
                        }
                        None => (None, 0.0),
                    }
                } else {
                    (None, 0.0)
                };
            if let Some(custom) = self.custom_drawing_cursor(band_height, band_v_offset) {
                self.renderer.set_cursor(Some(&custom));
                return;
            }
            cursor = self.idle_cursor_for_active_tool();
        }

        self.renderer.set_cursor_from_name(cursor);
    }

    /// Cursor to show when nothing is under the pointer.
    fn idle_cursor_for_active_tool(&self) -> Option<&'static str> {
        match self.active_tool_type() {
            // Pointer + Crop use the default arrow — they manipulate or
            // frame the image rather than draw new geometry.
            Tools::Pointer | Tools::Crop => None,
            _ => Some("crosshair"),
        }
    }

    /// Build a custom drawing cursor for tools that have one (Brush,
    /// Highlighter). Returns `None` for tools that should keep a
    /// stock named cursor. `band_height_image_px` overrides the
    /// Highlighter cursor's height to match a detected text band
    /// under the pointer — the "smart highlighter" preview. Pass
    /// `None` to use the style's stroke width as the cursor height.
    fn custom_drawing_cursor(
        &self,
        band_height_image_px: Option<f32>,
        band_vertical_offset_image_px: f32,
    ) -> Option<gtk::gdk::Cursor> {
        let render_scale = self.renderer.current_render_scale() as f64;
        // GTK4 paints cursor textures at a HiDPI-scaled on-screen size,
        // so we divide by DPR inside the cursor builders to keep the
        // cursor visually in lock-step with the stroke that comes out
        // of it.
        let dpr = crate::femtovg_area::current_device_pixel_ratio() as f64;
        crate::ui::cursor::drawing_tool_cursor(
            self.active_tool_type(),
            &self.style,
            render_scale,
            dpr,
            band_height_image_px,
            band_vertical_offset_image_px,
        )
    }

    /// Apply the idle cursor — used on tool switch, zoom change, and
    /// anywhere else we need to refresh without a motion event. When
    /// we have a remembered hover position (from a prior motion under
    /// any tool), we look up the band there so the cursor reflects
    /// the current under-the-pointer text row immediately instead of
    /// showing the style-derived size until the next motion. First
    /// invocation of the app (no prior motion) falls through to the
    /// style cursor — same behavior as before.
    fn apply_idle_cursor(&mut self) {
        if let Some(pos) = self.last_hover_image_pos {
            self.update_hover_cursor(pos);
            return;
        }
        if let Some(custom) = self.custom_drawing_cursor(None, 0.0) {
            self.renderer.set_cursor(Some(&custom));
            return;
        }
        self.renderer
            .set_cursor_from_name(self.idle_cursor_for_active_tool());
    }
}

/// Per-row layer-panel data passed to `build_layer_panel_row`.
struct LayerRowData<'a> {
    id: DrawableId,
    icon_name: &'a str,
    preview: crate::tools::PanelPreview,
    swatch: crate::tools::PanelSwatch,
    label: &'a str,
    selected: bool,
    visible: bool,
    locked: bool,
}

/// Paint a cairo silhouette for `preview` into the row's kind-icon
/// slot. Color is the widget's current foreground (theme color) — the
/// swatch beside it carries the per-drawable color so the icon stays
/// neutral and just communicates shape + fill state.
fn draw_panel_preview(
    cr: &relm4::gtk::cairo::Context,
    w: f64,
    h: f64,
    preview: crate::tools::PanelPreview,
    rgba: (f64, f64, f64, f64),
) {
    use crate::tools::PanelPreview;
    cr.save().ok();
    cr.set_source_rgba(rgba.0, rgba.1, rgba.2, rgba.3);
    match preview {
        PanelPreview::Icon => {}
        PanelPreview::Rectangle { filled } => {
            let pad = 2.0;
            let radius = 2.0;
            let x = pad + 0.5;
            let y = pad + 0.5;
            let rw = (w - 2.0 * pad - 1.0).max(1.0);
            let rh = (h - 2.0 * pad - 1.0).max(1.0);
            cr.new_sub_path();
            cr.arc(x + rw - radius, y + radius, radius, -1.5708, 0.0);
            cr.arc(x + rw - radius, y + rh - radius, radius, 0.0, 1.5708);
            cr.arc(x + radius, y + rh - radius, radius, 1.5708, 3.1416);
            cr.arc(x + radius, y + radius, radius, 3.1416, 4.7124);
            cr.close_path();
            if filled {
                cr.fill().ok();
            } else {
                cr.set_line_width(1.5);
                cr.stroke().ok();
            }
        }
        PanelPreview::Ellipse { filled } => {
            let pad = 1.5;
            let cx = w * 0.5;
            let cy = h * 0.5;
            let rx = (w * 0.5 - pad).max(1.0);
            let ry = (h * 0.5 - pad).max(1.0);
            cr.save().ok();
            cr.translate(cx, cy);
            cr.scale(rx, ry);
            cr.arc(0.0, 0.0, 1.0, 0.0, 2.0 * std::f64::consts::PI);
            cr.restore().ok();
            if filled {
                cr.fill().ok();
            } else {
                cr.set_line_width(1.5);
                cr.stroke().ok();
            }
        }
        PanelPreview::Line => {
            let pad = 2.0;
            let mid_y = h * 0.5;
            cr.set_line_width(2.0);
            cr.set_line_cap(relm4::gtk::cairo::LineCap::Round);
            cr.move_to(pad, mid_y);
            cr.line_to(w - pad, mid_y);
            cr.stroke().ok();
        }
        PanelPreview::Arrow(style) => {
            crate::ui::toolbars::draw_arrow_preview_cairo(cr, style, w, h, rgba);
        }
    }
    cr.restore().ok();
}

/// Build a single layer-panel row: kind icon (cairo arrow preview when
/// the drawable is an Arrow, otherwise the icon-name image), color
/// swatch (clickable → opens a color dialog), a Stack containing the
/// label and an inline Entry for click-to-rename, eye + lock buttons,
/// drag source + drop target for reorder.
///
/// All gestures land on the row widget itself so GTK4 can coordinate
/// click vs drag cleanly — earlier the click controller lived on an
/// inner body Box, which captured the press before the DragSource on
/// the row could see it.
fn build_layer_panel_row(
    data: LayerRowData<'_>,
    sender: relm4::Sender<SketchBoardInput>,
) -> gtk::Box {
    let mut classes: Vec<&str> = vec!["layer_row"];
    if data.selected {
        classes.push("selected");
    }
    if !data.visible {
        classes.push("hidden_layer");
    }
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .css_classes(classes)
        .build();
    // Tag the row with its drawable id so the panel-level drop target
    // can recover the target id from the widget under the cursor.
    // Using `widget_name` is the cheapest path that doesn't need a
    // side HashMap — drop happens infrequently so the string parse
    // cost is negligible.
    row.set_widget_name(&format!("layer-row-{}", data.id.0));

    let row_id = data.id;

    // Swatch slot — paints one of three variants per
    // `Drawable::panel_swatch`:
    //   - Color: filled, clickable color picker.
    //   - None: grey-outlined empty box for "no mutable color"
    //     drawables (Image, Crop) — keeps row layout balanced.
    //   - Icon: small themed image for drawables whose effect doesn't
    //     reduce to a color (Blur's pixelated tile).
    use crate::tools::PanelSwatch;
    match data.swatch {
        PanelSwatch::Color(c) => {
            let swatch = gtk::DrawingArea::new();
            swatch.set_size_request(14, 14);
            swatch.set_valign(gtk::Align::Center);
            swatch.set_cursor_from_name(Some("pointer"));
            swatch.set_draw_func(move |_da, cr, w, h| {
                let rgba: relm4::gtk::gdk::RGBA = c.into();
                cr.set_source_rgba(
                    rgba.red() as f64,
                    rgba.green() as f64,
                    rgba.blue() as f64,
                    rgba.alpha() as f64,
                );
                cr.rectangle(0.0, 0.0, w as f64, h as f64);
                let _ = cr.fill();
            });
            let swatch_click = gtk::GestureClick::new();
            let sender = sender.clone();
            swatch_click.connect_pressed(move |g, _n, _x, _y| {
                g.set_state(gtk::EventSequenceState::Claimed);
                sender.send(SketchBoardInput::PanelEditColor(row_id)).ok();
            });
            swatch.add_controller(swatch_click);
            row.append(&swatch);
        }
        PanelSwatch::None => {
            let swatch = gtk::DrawingArea::new();
            swatch.set_size_request(14, 14);
            swatch.set_valign(gtk::Align::Center);
            swatch.set_draw_func(|_da, cr, w, h| {
                cr.set_source_rgba(0.4, 0.4, 0.4, 0.5);
                cr.rectangle(0.5, 0.5, (w - 1) as f64, (h - 1) as f64);
                cr.set_line_width(1.0);
                let _ = cr.stroke();
            });
            row.append(&swatch);
        }
        PanelSwatch::Checkerboard => {
            // 2×2 transparency-style checkerboard. Reads as "alpha"
            // and communicates "the canvas shows through here" — the
            // right metaphor for Blur, whose effect is a content
            // filter rather than a color.
            let swatch = gtk::DrawingArea::new();
            swatch.set_size_request(14, 14);
            swatch.set_valign(gtk::Align::Center);
            swatch.set_draw_func(|_da, cr, w, h| {
                let tile = (w.min(h) as f64 / 2.0).max(2.0);
                cr.set_source_rgb(0.78, 0.78, 0.78);
                cr.rectangle(0.0, 0.0, w as f64, h as f64);
                let _ = cr.fill();
                cr.set_source_rgb(0.5, 0.5, 0.5);
                let rows = ((h as f64 / tile).ceil() as i32) + 1;
                let cols = ((w as f64 / tile).ceil() as i32) + 1;
                for ty in 0..rows {
                    for tx in 0..cols {
                        if (tx + ty) % 2 == 1 {
                            cr.rectangle(
                                tx as f64 * tile,
                                ty as f64 * tile,
                                tile,
                                tile,
                            );
                        }
                    }
                }
                let _ = cr.fill();
            });
            row.append(&swatch);
        }
        PanelSwatch::SpotlightOverlay => {
            // Dim fill + light 1px border so the swatch is legible
            // against the panel's dark background, plus a small white
            // rounded rectangle inside representing the "highlighted
            // region" that the spotlight cutout reveals. At 14px the
            // inner rect is ~8×5 with 2px corners — reads as a
            // diorama of the actual spotlight effect.
            let swatch = gtk::DrawingArea::new();
            swatch.set_size_request(14, 14);
            swatch.set_valign(gtk::Align::Center);
            swatch.set_draw_func(|_da, cr, w, h| {
                let w = w as f64;
                let h = h as f64;
                // Dim fill.
                cr.set_source_rgba(0.18, 0.18, 0.18, 1.0);
                cr.rectangle(0.5, 0.5, w - 1.0, h - 1.0);
                let _ = cr.fill();
                // Outer border, light enough to stand off the dark panel.
                cr.set_source_rgba(0.7, 0.7, 0.7, 0.9);
                cr.rectangle(0.5, 0.5, w - 1.0, h - 1.0);
                cr.set_line_width(1.0);
                let _ = cr.stroke();
                // Inner "highlighted region" — white rounded rect.
                let inset = 3.0;
                let rx = inset;
                let ry = inset + 1.0;
                let rw = w - 2.0 * rx;
                let rh = h - 2.0 * ry;
                let r = 2.0;
                cr.set_source_rgb(0.95, 0.95, 0.95);
                cr.new_sub_path();
                cr.arc(rx + rw - r, ry + r, r, -1.5708, 0.0);
                cr.arc(rx + rw - r, ry + rh - r, r, 0.0, 1.5708);
                cr.arc(rx + r, ry + rh - r, r, 1.5708, 3.1416);
                cr.arc(rx + r, ry + r, r, 3.1416, 4.7124);
                cr.close_path();
                let _ = cr.fill();
            });
            row.append(&swatch);
        }
    }

    // Kind icon — always rendered after the swatch. Cairo silhouette
    // when the drawable supplies one (shape + fill state per-
    // instance), plain themed gtk::Image otherwise. The cairo paint
    // uses a fixed light gray since the swatch on the left handles
    // "what color".
    if matches!(data.preview, crate::tools::PanelPreview::Icon) {
        let icon = gtk::Image::from_icon_name(data.icon_name);
        icon.set_pixel_size(16);
        row.append(&icon);
    } else {
        let da = gtk::DrawingArea::new();
        let (slot_w, slot_h) = match data.preview {
            crate::tools::PanelPreview::Arrow(_) => (28, 14),
            _ => (18, 14),
        };
        da.set_size_request(slot_w, slot_h);
        da.set_valign(gtk::Align::Center);
        let preview = data.preview;
        da.set_draw_func(move |_da, cr, w, h| {
            // `gnome_42` doesn't expose `Widget::color()`, so we
            // settle on a fixed tone rather than tracking the
            // current theme's foreground.
            let rgba = (0.85, 0.85, 0.85, 1.0);
            draw_panel_preview(cr, w as f64, h as f64, preview, rgba);
        });
        row.append(&da);
    }

    // Name area: Stack with Label and Entry. Default visible child is
    // the label; double-click flips to the entry, focuses it, and
    // selects all text so the user can immediately type to replace.
    let label_widget = gtk::Label::builder()
        .label(data.label)
        .halign(gtk::Align::Start)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    let entry_widget = gtk::Entry::builder()
        .text(data.label)
        .hexpand(true)
        .build();
    let name_stack = gtk::Stack::new();
    name_stack.set_hexpand(true);
    name_stack.add_named(&label_widget, Some("label"));
    name_stack.add_named(&entry_widget, Some("entry"));
    name_stack.set_visible_child_name("label");
    row.append(&name_stack);

    // Entry's `activate` (Enter pressed) → commit and swap back to
    // label. The PanelRename handler in sketch_board normalises the
    // string (empty → clear custom name).
    {
        let sender = sender.clone();
        let stack = name_stack.clone();
        entry_widget.connect_activate(move |e| {
            sender
                .send(SketchBoardInput::PanelRename {
                    id: row_id,
                    name: e.text().to_string(),
                })
                .ok();
            stack.set_visible_child_name("label");
        });
    }
    // Esc inside the entry cancels the edit — restore the original
    // text and switch back to the label without sending PanelRename.
    {
        let key = gtk::EventControllerKey::new();
        let stack = name_stack.clone();
        let entry = entry_widget.clone();
        let original = data.label.to_string();
        key.connect_key_pressed(move |_c, k, _kc, _m| {
            if k == gtk::gdk::Key::Escape {
                entry.set_text(&original);
                stack.set_visible_child_name("label");
                relm4::gtk::glib::Propagation::Stop
            } else {
                relm4::gtk::glib::Propagation::Proceed
            }
        });
        entry_widget.add_controller(key);
    }
    // Focus-out while in edit mode commits (Finder-style). Without
    // this, clicking elsewhere would leave the entry visible with
    // stale text. `has_focus_notify` fires both on focus-in and
    // focus-out — we only want to act on the loss.
    {
        let sender = sender.clone();
        let stack = name_stack.clone();
        entry_widget.connect_has_focus_notify(move |entry| {
            if entry.has_focus() {
                return;
            }
            // Only commit if we're actually in edit mode — otherwise
            // an initial focus-loss during widget construction would
            // submit an empty rename.
            if stack
                .visible_child_name()
                .map(|n| n.as_str() == "entry")
                .unwrap_or(false)
            {
                sender
                    .send(SketchBoardInput::PanelRename {
                        id: row_id,
                        name: entry.text().to_string(),
                    })
                    .ok();
                stack.set_visible_child_name("label");
            }
        });
    }

    // Eye toggle.
    let eye_icon = if data.visible {
        "eye-regular"
    } else {
        "eye-off-regular"
    };
    let eye_btn = gtk::Button::builder()
        .icon_name(eye_icon)
        .focusable(false)
        .css_classes(["layer_icon_btn", "flat"])
        .build();
    eye_btn.set_tooltip_text(Some(if data.visible {
        "Hide layer"
    } else {
        "Show layer"
    }));
    {
        let sender = sender.clone();
        eye_btn.connect_clicked(move |_| {
            sender
                .send(SketchBoardInput::PanelToggleVisible(row_id))
                .ok();
        });
    }
    row.append(&eye_btn);

    // Lock toggle.
    let lock_icon = if data.locked {
        "lock-closed-regular"
    } else {
        "lock-open-regular"
    };
    let lock_btn = gtk::Button::builder()
        .icon_name(lock_icon)
        .focusable(false)
        .css_classes(["layer_icon_btn", "flat"])
        .build();
    lock_btn.set_tooltip_text(Some(if data.locked {
        "Unlock layer"
    } else {
        "Lock layer"
    }));
    {
        let sender = sender.clone();
        lock_btn.connect_clicked(move |_| {
            sender
                .send(SketchBoardInput::PanelToggleLocked(row_id))
                .ok();
        });
    }
    row.append(&lock_btn);

    // Row-level click: Finder-style two-stage rename. First click on
    // an unselected row selects it (selection-emit on release so it
    // doesn't race with drag start). A subsequent plain click on the
    // already-selected row enters rename mode. Ctrl-click always
    // toggles multi-select and never enters rename, so users can
    // refine a selection without accidentally renaming.
    //
    // Selection emit on `released` (not `pressed`) is load-bearing:
    // emitting on press synchronously triggers a panel rebuild that
    // destroys this row mid-drag, and the in-progress `DragSource`
    // would lose its source widget before reaching the drag-start
    // threshold — which is the bug that made drag-and-drop look
    // broken before.
    let row_was_selected = data.selected;
    {
        let click = gtk::GestureClick::new();
        let sender = sender.clone();
        let stack = name_stack.clone();
        let entry = entry_widget.clone();
        let label_for_entry = data.label.to_string();
        click.connect_released(move |controller, _n, _x, _y| {
            let ctrl = controller
                .current_event_state()
                .contains(gtk::gdk::ModifierType::CONTROL_MASK);
            if !ctrl && row_was_selected {
                entry.set_text(&label_for_entry);
                stack.set_visible_child_name("entry");
                entry.grab_focus();
                entry.select_region(0, -1);
                return;
            }
            sender
                .send(SketchBoardInput::PanelSelectDrawable {
                    id: row_id,
                    additive: ctrl,
                })
                .ok();
        });
        row.add_controller(click);
    }

    // Drag source — payload is the row's u64 drawable id. `connect_
    // prepare` builds the content lazily (value captured fresh per
    // drag), and matches the canonical gtk4-rs example pattern.
    //
    // On drag begin we (1) install a `WidgetPaintable` of this row
    // as the drag icon so the dragged ghost looks like the row's
    // actual content, and (2) tag the row with a `dragging` CSS
    // class to dim it as an in-place placeholder. The class is
    // cleared on drag end whether the drop completed or was
    // cancelled.
    let drag = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::MOVE)
        .build();
    {
        use relm4::gtk::glib::value::ToValue;
        let id_u64 = row_id.0;
        drag.connect_prepare(move |_source, _x, _y| {
            Some(gtk::gdk::ContentProvider::for_value(&id_u64.to_value()))
        });
    }
    {
        let row_weak = row.downgrade();
        drag.connect_drag_begin(move |source, _drag| {
            if let Some(r) = row_weak.upgrade() {
                let paintable = gtk::WidgetPaintable::new(Some(&r));
                source.set_icon(Some(&paintable), 0, 0);
                r.add_css_class("dragging");
            }
        });
    }
    {
        let row_weak = row.downgrade();
        drag.connect_drag_end(move |_source, _drag, _delete| {
            if let Some(r) = row_weak.upgrade() {
                r.remove_css_class("dragging");
            }
        });
    }
    row.add_controller(drag);

    // No per-row drop target: a single panel-level DropTarget on
    // `layer_panel_content` owns the drop indicator state, so the
    // hovered row always has a single line (never one row showing
    // "below" while the next shows "above" because of overlapping
    // enter/leave events).

    row
}

/// Footer row of reorder buttons: Front / Up / Down / Back. Each emits
/// `PanelMoveSelected`, which acts on whatever ids are currently
/// selected (no-op when nothing is selected).
fn build_layer_panel_footer(sender: relm4::Sender<SketchBoardInput>) -> gtk::Box {
    let footer = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(2)
        .homogeneous(true)
        .css_classes(["layer_panel_footer"])
        .build();

    let make_btn = |icon: &str, tip: &str, dir: PanelMoveDir| -> gtk::Button {
        let b = gtk::Button::builder()
            .icon_name(icon)
            .focusable(false)
            .css_classes(["layer_icon_btn", "flat"])
            .tooltip_text(tip)
            .build();
        let s = sender.clone();
        b.connect_clicked(move |_| {
            s.send(SketchBoardInput::PanelMoveSelected(dir)).ok();
        });
        b
    };

    footer.append(&make_btn(
        "chevron-double-up-regular",
        "Bring to front",
        PanelMoveDir::ToTop,
    ));
    footer.append(&make_btn(
        "arrow-up-regular",
        "Bring forward",
        PanelMoveDir::Up,
    ));
    footer.append(&make_btn(
        "arrow-down-regular",
        "Send backward",
        PanelMoveDir::Down,
    ));
    footer.append(&make_btn(
        "chevron-double-down-regular",
        "Send to back",
        PanelMoveDir::ToBottom,
    ));
    footer
}

/// Parse a config-style keyboard shortcut into a `(Key, ModifierType)`
/// pair suitable for direct equality-matching against a `KeyEventMsg`.
///
/// Accepted format: `[mod+]*key` where each `mod` is `ctrl` / `control`,
/// `shift`, `alt`, or `super` / `meta`. The trailing key token is either
/// an F-key name (`f1`..`f24`, case-insensitive) or a single character
/// (case-insensitive — matched against the unshifted key, so `ctrl+l`
/// fires whether or not Shift is held by mistake). Returns `None` on a
/// malformed string so the layer-panel toggle silently disables rather
/// than panicking on a typo.
fn parse_shortcut(s: &str) -> Option<(gtk::gdk::Key, ModifierType)> {
    use gtk::gdk::Key;
    let mut mods = ModifierType::empty();
    let mut key: Option<Key> = None;
    for token in s.split('+').map(|t| t.trim()) {
        if token.is_empty() {
            return None;
        }
        let lc = token.to_ascii_lowercase();
        match lc.as_str() {
            "ctrl" | "control" => mods |= ModifierType::CONTROL_MASK,
            "shift" => mods |= ModifierType::SHIFT_MASK,
            "alt" => mods |= ModifierType::ALT_MASK,
            "super" | "meta" | "mod4" => mods |= ModifierType::SUPER_MASK,
            _ => {
                if key.is_some() {
                    return None;
                }
                // F-keys must use capital F in GDK names; single letters
                // are lowercase. `key_from_name` returns `None` on
                // unrecognised names — propagate that as a parse failure.
                let normalized = if lc.starts_with('f')
                    && lc.len() >= 2
                    && lc[1..].chars().all(|c| c.is_ascii_digit())
                {
                    let mut s = String::from("F");
                    s.push_str(&lc[1..]);
                    s
                } else {
                    lc
                };
                key = Key::from_name(&normalized);
            }
        }
    }
    key.map(|k| (k, mods))
}

/// Walk the layer panel's children and clear both drop-indicator CSS
/// classes from every row. Called on drag enter / motion (before
/// applying the indicator to the row under the cursor) and on leave /
/// drop, so we never end up with stale lines on adjacent rows.
fn clear_drop_indicators(content: &gtk::Box) {
    let mut child = content.first_child();
    while let Some(w) = child {
        w.remove_css_class("drop_above");
        w.remove_css_class("drop_below");
        child = w.next_sibling();
    }
}

/// Resolve the drop-target row for cursor `y` (in panel-content coords).
/// Returns `(row_widget, above)` where:
/// - `above = true` puts the drop line at this row's top edge (insert
///   the dragged layer above this row).
/// - `above = false` puts the drop line at the row's bottom edge, and
///   is only ever returned for the last row — that's the special case
///   for dropping below the bottommost layer.
///
/// Each "gap between rows" produces ONE canonical position, not two:
/// hovering the lower half of row A or the upper half of row B both
/// resolve to "above row B". Before this collapse, the lower half of
/// A reported "below A" (line at A's bottom) and the gap reported
/// "above B" (line at B's top), producing two visually distinct drop
/// indicators 1–2px apart that the user could oscillate between.
///
/// The pivot is each row's vertical midpoint: cursor above midpoint →
/// drop above that row; cursor below midpoint → defer to the next
/// row's "above" zone. The last row absorbs everything below its
/// midpoint into its single "below" position.
fn drop_target_row(content: &gtk::Box, y: f64) -> Option<(gtk::Widget, bool)> {
    let mut child = content.first_child();
    let mut last_row: Option<gtk::Widget> = None;
    while let Some(w) = child {
        if w.css_classes().iter().any(|c| c == "layer_row") {
            if let Some(bounds) = w.compute_bounds(content) {
                let top = bounds.y() as f64;
                let h = bounds.height() as f64;
                if y < top + h * 0.5 {
                    return Some((w, true));
                }
                last_row = Some(w.clone());
            }
        }
        child = w.next_sibling();
    }
    last_row.map(|w| (w, false))
}

fn cursor_for_handle(handle: HandleId) -> &'static str {
    match handle {
        HandleId::Start | HandleId::End | HandleId::Control => "move",
        HandleId::TopLeft | HandleId::BottomRight => "nwse-resize",
        HandleId::TopRight | HandleId::BottomLeft => "nesw-resize",
        HandleId::Top | HandleId::Bottom => "ns-resize",
        HandleId::Left | HandleId::Right => "ew-resize",
    }
}

#[relm4::component(pub)]
impl Component for SketchBoard {
    type CommandOutput = ();
    type Input = SketchBoardInput;
    type Output = SketchBoardOutput;
    type Init = Pixbuf;

    view! {
        // Outer Box is a thin wrapper around the layer-panel Paned —
        // relm4's view! wants a widget definition at the root, not a
        // local_ref, so the Box exists only to host the Paned. The
        // Paned itself is owned by the model so init() can build it
        // (and its position-notify handler) before the view! macro
        // re-parents it via `#[local_ref]`.
        gtk::Box {
            set_orientation: gtk::Orientation::Horizontal,
            #[local_ref]
            layer_panel_paned_ref -> gtk::Paned {
                set_hexpand: true,
                set_vexpand: true,
                #[wrap(Some)]
                set_end_child = &gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    #[local_ref]
                    area -> FemtoVGArea {
                set_vexpand: true,
                set_hexpand: true,
                set_can_focus: true,
                set_focusable: true,
                grab_focus: (),

                // Controller order matters: GTK4 dispatches gestures in
                // reverse-registration order, so the *last-added* gesture
                // gets the press event first. We need GestureDrag's
                // drag-begin to fire before GestureClick's pressed —
                // otherwise GestureClick.pressed → MarkerTool.Click commits
                // a marker, and the subsequent BeginDrag's hit-test picks
                // up the just-created marker as a "click on existing
                // shape" and auto-selects it.
                add_controller = gtk::GestureClick {
                    set_button: 0,
                    connect_pressed[sender] => move |controller, n_pressed, x, y| {
                        sender.input(SketchBoardInput::new_mouse_event(
                            MouseEventType::Click,
                            controller.current_button(),
                            n_pressed,
                            controller.current_event_state(),
                            Vec2D::new(x as f32, y as f32),
                            false,
                        ));
                    },
                    connect_released[sender] => move |controller, n_released, x, y| {
                        sender.input(SketchBoardInput::new_mouse_event(
                            MouseEventType::Release,
                            controller.current_button(),
                            n_released,
                            controller.current_event_state(),
                            Vec2D::new(x as f32, y as f32),
                            true,
                        ));
                    }
                },

                add_controller = gtk::GestureDrag {
                        set_button: 0,
                        connect_drag_begin[sender] => move |controller, x, y| {
                            sender.input(SketchBoardInput::new_mouse_event(
                                MouseEventType::BeginDrag,
                                controller.current_button(),
                                1,
                                controller.current_event_state(),
                                Vec2D::new(x as f32, y as f32),
                                false,
                            ));

                        },
                        connect_drag_update[sender] => move |controller, x, y| {
                            sender.input(SketchBoardInput::new_mouse_event(
                                MouseEventType::UpdateDrag,
                                controller.current_button(),
                                1,
                                controller.current_event_state(),
                                Vec2D::new(x as f32, y as f32),
                                false,
                            ));
                        },
                        connect_drag_end[sender] => move |controller, x, y| {
                            sender.input(SketchBoardInput::new_mouse_event(
                                MouseEventType::EndDrag,
                                controller.current_button(),
                                1,
                                controller.current_event_state(),
                                Vec2D::new(x as f32, y as f32),
                                false
                            ));
                        }
                },

                add_controller = gtk::GestureZoom {
                    // Two-finger trackpad pinch → zoom. GTK reports an
                    // absolute `scale` relative to the gesture start
                    // (1.0 at begin, >1 as fingers spread, <1 as they
                    // pinch). We convert each tick into a multiplicative
                    // delta (current / previous) so the existing
                    // `set_zoom_scale` (which is itself multiplicative)
                    // sees a clean per-frame factor. State lives in an
                    // `Rc<Cell<f32>>` cloned into both callbacks.
                    connect_begin[pinch_last] => move |_gesture, _seq| {
                        pinch_last.set(1.0);
                    },
                    connect_scale_changed[sender, pinch_last] => move |_gesture, scale| {
                        let prev = pinch_last.get();
                        let scale_f = scale as f32;
                        if scale_f <= 0.0 || prev <= 0.0 {
                            return;
                        }
                        let delta = scale_f / prev;
                        pinch_last.set(scale_f);
                        sender.input(SketchBoardInput::new_pinch_zoom_event(delta));
                    },
                },

                add_controller = gtk::EventControllerScroll{
                    // BOTH_AXES — modern trackpads + tiltable mouse
                    // wheels emit horizontal scroll deltas alongside
                    // vertical, so we listen for both and pass them
                    // to the renderer's pan_by.
                    set_flags: gtk::EventControllerScrollFlags::BOTH_AXES,
                    connect_scroll[sender] => move |controller, dx, dy| {
                        let modifier = controller.current_event_state();
                        // Single inversion site for the canvas — flips
                        // both axes so pan, zoom, and the scroll-resize
                        // gestures all reverse together when the
                        // invert-scrolling preference is set.
                        let (dx, dy) = if APP_CONFIG.read().invert_scrolling() {
                            (-dx, -dy)
                        } else {
                            (dx, dy)
                        };
                        // Shift+vertical-wheel → horizontal pan: the
                        // standard "shift-flips-axis" remap. Only remap a
                        // pure vertical delta (a trackpad swipe already
                        // carries `dx`). Skip it when Ctrl or Alt is also
                        // held — Ctrl+Shift+wheel is the alt-slider chord
                        // and Alt+Shift+wheel the size-factor chord; both
                        // need the vertical dy intact.
                        let (dx, dy) = if modifier
                            .contains(gtk::gdk::ModifierType::SHIFT_MASK)
                            && !modifier.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                            && !modifier.contains(gtk::gdk::ModifierType::ALT_MASK)
                            && dx == 0.0
                        {
                            (dy, 0.0)
                        } else {
                            (dx, dy)
                        };
                        // Every wheel event becomes a PanScroll; the
                        // input handler's modifier chain routes it to
                        // pan / zoom / resize / size. Super is left
                        // untouched so the compositor's Super+wheel
                        // workspace bind keeps working.
                        sender.input(SketchBoardInput::new_pan_scroll_event(
                            dx, dy, modifier,
                        ));
                        relm4::gtk::glib::Propagation::Stop
                    },
                },

                add_controller = gtk::EventControllerKey {
                    connect_key_pressed[sender] => move |controller, key, code, modifier | {
                        // Any chord that involves the Super modifier
                        // belongs to the window manager — Hyprland uses
                        // it as a global prefix (Super+W to close,
                        // Super+1..0 to switch workspaces, etc.) and
                        // satty has no keyboard bindings on Super.
                        // Returning Proceed lets GTK forward the event
                        // to the WM instead of swallowing it at the
                        // canvas. We don't even emit it as a
                        // SketchBoardInput so it can't get
                        // misinterpreted as a single-key tool shortcut.
                        if modifier.contains(gtk::gdk::ModifierType::SUPER_MASK) {
                            return relm4::gtk::glib::Propagation::Proceed;
                        }
                        if let Some(im_context) = controller.im_context() {
                            im_context.focus_in();
                            if !im_context.filter_keypress(controller.current_event().unwrap()) {
                                sender.input(SketchBoardInput::new_key_event(KeyEventMsg::new(key, code, modifier)));
                            }
                        } else {
                            sender.input(SketchBoardInput::new_key_event(KeyEventMsg::new(key, code, modifier)));
                        }
                        relm4::gtk::glib::Propagation::Stop
                    },

                    connect_key_released[sender] => move |controller, key, code, modifier | {
                        // Mirror the press handler: don't process Super
                        // chord releases either.
                        if modifier.contains(gtk::gdk::ModifierType::SUPER_MASK) {
                            return;
                        }
                        if let Some(im_context) = controller.im_context() {
                            im_context.focus_in();
                            if !im_context.filter_keypress(controller.current_event().unwrap()) {
                                sender.input(SketchBoardInput::new_key_release_event(KeyEventMsg::new(key, code, modifier)));
                            }
                        } else {
                            sender.input(SketchBoardInput::new_key_release_event(KeyEventMsg::new(key, code, modifier)));
                        }
                    },
                    set_im_context: Some(&model.im_context),
                },

                add_controller = gtk::EventControllerMotion {
                    connect_motion[sender] => move |controller, x, y| {
                        sender.input(SketchBoardInput::new_mouse_event(
                            MouseEventType::PointerPos,
                            0,
                            0,
                            controller.current_event_state(),
                            Vec2D::new(x as f32, y as f32),
                            false
                        ));
                    }
                }
                    },  // end FemtoVGArea
                },  // end set_end_child wrapper Box
            },  // end Paned
        },
    }

    fn update(&mut self, msg: SketchBoardInput, sender: ComponentSender<Self>, _root: &Self::Root) {
        // `sender` is consumed by individual arms below; clone once so
        // the result-processing match at the bottom can still use it
        // (e.g. for `EditTextDrawable` which triggers a tool switch).
        let outer_sender = sender.clone();
        // handle resize ourselves, pass everything else to tool
        let result = match msg {
            SketchBoardInput::InputEvent(mut ie) => {
                if let InputEvent::Key(ke) = ie {
                    // Implicit selection: route Delete / Escape through the
                    // pointer tool first when a non-Pointer tool is active,
                    // so a selected drawable can be deleted/deselected without
                    // switching tools.
                    let active_type = self.active_tool_type();
                    let pointer_key_consumed = if active_type != Tools::Pointer {
                        let r = self
                            .tools
                            .get(&Tools::Pointer)
                            .borrow_mut()
                            .handle_event(ToolEvent::Input(ie.clone()));
                        match r {
                            ToolUpdateResult::StopPropagation
                            | ToolUpdateResult::RedrawAndStopPropagation
                            | ToolUpdateResult::RaiseAndRedrawStop(_)
                            | ToolUpdateResult::ModifyDrawable(_, _)
                            | ToolUpdateResult::ModifyDrawables(_)
                            | ToolUpdateResult::ModifyDrawableCoalesce(_, _)
                            | ToolUpdateResult::ModifyDrawablesCoalesce(_)
                            | ToolUpdateResult::DeleteDrawable(_)
                            | ToolUpdateResult::DeleteDrawables(_) => Some(r),
                            _ => None,
                        }
                    } else {
                        None
                    };

                    let active_tool_result = if let Some(r) = pointer_key_consumed {
                        r
                    } else {
                        self.active_tool
                            .borrow_mut()
                            .handle_event(ToolEvent::Input(ie.clone()))
                    };

                    match active_tool_result {
                        ToolUpdateResult::StopPropagation
                        | ToolUpdateResult::RedrawAndStopPropagation
                        | ToolUpdateResult::RaiseAndRedrawStop(_)
                        | ToolUpdateResult::DeleteDrawable(_)
                        | ToolUpdateResult::DeleteDrawables(_)
                        | ToolUpdateResult::ModifyDrawable(_, _)
                        | ToolUpdateResult::ModifyDrawables(_)
                        | ToolUpdateResult::ModifyDrawableCoalesce(_, _)
                        | ToolUpdateResult::ModifyDrawablesCoalesce(_)
                        | ToolUpdateResult::Commit(_) => active_tool_result,
                        _ => {
                            if self
                                .layer_panel_shortcut
                                .is_some_and(|(k, m)| ke.key == k && ke.modifier == m)
                            {
                                // Configurable layer-panel toggle. Default
                                // `ctrl+l` per `config.toml`'s
                                // `layer-panel-shortcut`; see
                                // `parse_shortcut`. Comparison is exact —
                                // an extra modifier (e.g. Ctrl+Shift+L)
                                // won't fire `Ctrl+L`.
                                sender.input(SketchBoardInput::ToggleLayerPanel);
                                ToolUpdateResult::Unmodified
                            } else if ke.is_one_of(Key::z, KeyMappingId::UsZ)
                                && ke.modifier == ModifierType::CONTROL_MASK
                            {
                                self.handle_undo()
                            } else if ke.is_one_of(Key::y, KeyMappingId::UsY)
                                && ke.modifier == ModifierType::CONTROL_MASK
                            {
                                self.handle_redo()
                            } else if ke.is_one_of(Key::v, KeyMappingId::UsV)
                                && ke.modifier == ModifierType::CONTROL_MASK
                            {
                                // Ctrl+V paste — the Text tool consumes
                                // Ctrl+V (returning StopPropagation)
                                // while editing, so we only get here
                                // when no tool wanted the press. Read
                                // the clipboard's image asynchronously
                                // and commit a `PastedImage` drawable.
                                self.handle_paste_image(&outer_sender);
                                ToolUpdateResult::Unmodified
                            } else if ke.is_one_of(Key::d, KeyMappingId::UsD)
                                && ke.modifier == ModifierType::ALT_MASK
                            {
                                // Alt+D = duplicate selection.
                                // Originally wanted Shift+D for the
                                // single-handed-ergonomics reason,
                                // but fcitx5 (and IMs in general)
                                // intercept Shift+letter at the
                                // Wayland text-input level — the
                                // keypress never reaches GTK, so
                                // satty can't see it. Alt+letter
                                // chords reach the application
                                // reliably and are still a left-hand
                                // single-key press.
                                self.duplicate_selection(&outer_sender)
                            } else if ke.is_one_of(Key::d, KeyMappingId::UsD)
                                && ke.modifier == ModifierType::CONTROL_MASK
                            {
                                // Ctrl+D = delete currently-selected
                                // drawable(s). Same effect as the
                                // Delete / Backspace keys, just an
                                // alternative for single-handed
                                // operation (no reach to the far
                                // side of the keyboard).
                                self.delete_selection()
                            } else if ke.is_one_of(Key::t, KeyMappingId::UsT)
                                && ke.modifier == ModifierType::CONTROL_MASK
                            {
                                self.handle_toggle_toolbars_display(sender)
                            } else if ke.key == Key::comma
                                && ke.modifier == ModifierType::CONTROL_MASK
                            {
                                // Ctrl+, → open Preferences. Mirrors the
                                // gear button in the top toolbar's end
                                // cluster.
                                sender
                                    .output_sender()
                                    .emit(SketchBoardOutput::OpenPreferences);
                                ToolUpdateResult::Unmodified
                            } else if ke.is_one_of(Key::s, KeyMappingId::UsS)
                                && ke.modifier == ModifierType::CONTROL_MASK
                            {
                                self.renderer.request_render(&[Action::SaveToFile]);
                                ToolUpdateResult::Unmodified
                            } else if ke.is_one_of(Key::s, KeyMappingId::UsS)
                                && ke.modifier
                                    == (ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK)
                            {
                                self.renderer.request_render(&[Action::SaveToFileAs]);
                                ToolUpdateResult::Unmodified
                            } else if ke.is_one_of(Key::c, KeyMappingId::UsC)
                                && ke.modifier == ModifierType::CONTROL_MASK
                            {
                                self.renderer.request_render(&[Action::SaveToClipboard]);
                                ToolUpdateResult::Unmodified
                            } else if ke.is_one_of(Key::c, KeyMappingId::UsC)
                                && ke.modifier
                                    == (ModifierType::CONTROL_MASK | ModifierType::ALT_MASK)
                            {
                                self.renderer
                                    .request_render(&[Action::CopyFilepathToClipboard]);
                                ToolUpdateResult::Unmodified
                            } else if (ke.is_one_of(Key::equal, KeyMappingId::Equal)
                                || ke.is_one_of(Key::plus, KeyMappingId::Equal))
                                && ke.modifier == ModifierType::CONTROL_MASK
                            {
                                self.handle_zoom_command(ZoomCommand::In);
                                ToolUpdateResult::Unmodified
                            } else if ke.is_one_of(Key::minus, KeyMappingId::Minus)
                                && ke.modifier == ModifierType::CONTROL_MASK
                            {
                                self.handle_zoom_command(ZoomCommand::Out);
                                ToolUpdateResult::Unmodified
                            } else if ke.is_one_of(Key::_0, KeyMappingId::Digit0)
                                && ke.modifier == ModifierType::CONTROL_MASK
                            {
                                self.handle_zoom_command(ZoomCommand::Abs(1.0));
                                ToolUpdateResult::Unmodified
                            } else if ke.is_one_of(Key::_1, KeyMappingId::Digit1)
                                && ke.modifier == ModifierType::CONTROL_MASK
                            {
                                self.handle_zoom_command(ZoomCommand::FitCanvas);
                                ToolUpdateResult::Unmodified
                            } else if ke.is_one_of(Key::_2, KeyMappingId::Digit2)
                                && ke.modifier == ModifierType::CONTROL_MASK
                            {
                                self.handle_zoom_command(ZoomCommand::Abs(2.0));
                                ToolUpdateResult::Unmodified
                            } else if ke.is_one_of(Key::_3, KeyMappingId::Digit3)
                                && ke.modifier == ModifierType::CONTROL_MASK
                            {
                                self.handle_zoom_command(ZoomCommand::Abs(3.0));
                                ToolUpdateResult::Unmodified
                            } else if ke.is_one_of(Key::_4, KeyMappingId::Digit4)
                                && ke.modifier == ModifierType::CONTROL_MASK
                            {
                                self.handle_zoom_command(ZoomCommand::Abs(4.0));
                                ToolUpdateResult::Unmodified
                            } else if ke.is_one_of(Key::_5, KeyMappingId::Digit5)
                                && ke.modifier == ModifierType::CONTROL_MASK
                            {
                                self.handle_zoom_command(ZoomCommand::Abs(5.0));
                                ToolUpdateResult::Unmodified
                            } else if ke.is_one_of(Key::_9, KeyMappingId::Digit9)
                                && ke.modifier == ModifierType::CONTROL_MASK
                            {
                                self.handle_zoom_command(ZoomCommand::Abs(0.5));
                                ToolUpdateResult::Unmodified
                            } else if (ke.is_one_of(Key::d, KeyMappingId::UsD)
                                || ke.is_one_of(Key::i, KeyMappingId::UsI))
                                && ke.modifier
                                    == (ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK)
                            {
                                /* GTK does not appear to offer any tracking for this, so
                                we'd have to track the state ourselves. But since the user may
                                just choose to close the inspector window, doing so adds little
                                benefit.

                                Just enable it everytime, and let the user close the window if they
                                so wish.
                                 */
                                gtk::Window::set_interactive_debugging(true);
                                ToolUpdateResult::Unmodified
                            } else if (ke.is_one_of(Key::leftarrow, KeyMappingId::ArrowLeft)
                                || ke.is_one_of(Key::rightarrow, KeyMappingId::ArrowRight)
                                || ke.is_one_of(Key::uparrow, KeyMappingId::ArrowUp)
                                || ke.is_one_of(Key::downarrow, KeyMappingId::ArrowDown))
                                && ke.modifier == ModifierType::ALT_MASK
                            {
                                let pan_step_size = APP_CONFIG.read().pan_step_size();
                                match ke.key {
                                    Key::Left => self
                                        .renderer
                                        .set_drag_offset(Vec2D::new(-pan_step_size, 0.)),
                                    Key::Right => {
                                        self.renderer.set_drag_offset(Vec2D::new(pan_step_size, 0.))
                                    }
                                    Key::Up => self
                                        .renderer
                                        .set_drag_offset(Vec2D::new(0., -pan_step_size)),
                                    Key::Down => {
                                        self.renderer.set_drag_offset(Vec2D::new(0., pan_step_size))
                                    }
                                    _ => { /* unreachable */ }
                                }

                                self.renderer.store_last_offset();
                                self.renderer.request_render(&[]);
                                ToolUpdateResult::Unmodified
                            } else if ke.modifier.is_empty() && ke.key == Key::Delete {
                                self.handle_reset()
                            } else if ke.modifier.is_empty()
                                && (ke.key == Key::Escape
                                    || ke.key == Key::Return
                                    || ke.key == Key::KP_Enter)
                            {
                                // First, let the tool handle the event. If the tool does nothing, we can do our thing (otherwise require a second keyboard press)
                                // Relying on ToolUpdateResult::Unmodified is probably not a good idea, but it's the only way at the moment. See discussion in #144
                                if let ToolUpdateResult::Unmodified = active_tool_result {
                                    let actions = if ke.key == Key::Escape {
                                        // Start with whatever the user
                                        // configured for Esc, then add the
                                        // implicit Exit only when the
                                        // "Close on Esc" preference is on.
                                        // Defaults to off so a stray Esc
                                        // doesn't kill the window mid-
                                        // annotation.
                                        let mut a =
                                            APP_CONFIG.read().actions_on_escape();
                                        if APP_CONFIG.read().close_on_esc()
                                            && !a.contains(&Action::Exit)
                                        {
                                            a.push(Action::Exit);
                                        }
                                        a
                                    } else {
                                        APP_CONFIG.read().actions_on_enter()
                                    };
                                    self.renderer.request_render(&actions);
                                };
                                active_tool_result
                            } else {
                                active_tool_result
                            }
                        }
                    }
                } else {
                    // Scroll-resize gesture takes precedence over the
                    // pan handler — running pan first would shove the
                    // canvas around while the user is trying to
                    // resize. So we sniff the event up front, and
                    // only delegate to the pan handler if the gesture
                    // ISN'T a resize.
                    let resize_consumed = if let InputEvent::Mouse(me) = &ie
                        && me.type_ == MouseEventType::PanScroll
                        && me.pos.y.abs() > 0.0
                    {
                        let selected = self
                            .tools
                            .get(&Tools::Pointer)
                            .borrow()
                            .selected_drawables();
                        let ctrl_held = me.modifier.contains(ModifierType::CONTROL_MASK);
                        let shift_held = me.modifier.contains(ModifierType::SHIFT_MASK);
                        let alt_held = me.modifier.contains(ModifierType::ALT_MASK);
                        let no_mods = !ctrl_held && !shift_held && !alt_held;
                        if self.active_tool_type() == Tools::Spotlight && no_mods {
                            // Plain wheel in Spotlight tool → darkness.
                            // Size and factor don't affect spotlight
                            // rendering, so the unmodified wheel drives
                            // its primary control instead of pan.
                            self.scroll_alt_slider(me.pos.y, &outer_sender);
                            true
                        } else if ctrl_held && shift_held {
                            // Ctrl+Shift+wheel → the active tool's
                            // "alternate" slider (brush smoothness /
                            // spotlight darkness / highlighter opacity).
                            self.scroll_alt_slider(me.pos.y, &outer_sender);
                            true
                        } else if alt_held && shift_held {
                            // Alt+Shift+wheel → the annotation
                            // size-factor (a display-scale calibration).
                            // A deliberately awkward chord: the factor
                            // is a near-one-time setting, not a
                            // per-stroke knob.
                            self.scroll_annotation_multiplier(me.pos.y, &outer_sender);
                            true
                        } else if alt_held {
                            // Alt+wheel → the active tool's size for the
                            // next stroke (mirrors the bottom toolbar's
                            // size slider).
                            self.scroll_resize_tool_size(me.pos.y, &outer_sender);
                            true
                        } else if ctrl_held && self.active_tool_type() == Tools::Crop {
                            // Ctrl+wheel in Crop edit → shrink / grow the
                            // crop rect from all four sides, anchored on
                            // its center and clamped to the image bounds.
                            // Scroll up grows toward the canvas outsides;
                            // scroll down shrinks toward the middle.
                            let crop_tool = self.tools.get_crop_tool();
                            let in_edit = crop_tool.borrow().is_active_edit();
                            if in_edit {
                                let factor = APP_CONFIG.read().zoom_factor();
                                let multiplier = factor.powf(-me.pos.y);
                                if crop_tool
                                    .borrow_mut()
                                    .resize_proportional(multiplier)
                                {
                                    self.renderer.request_render(&[]);
                                }
                                true
                            } else {
                                false
                            }
                        } else if ctrl_held {
                            // Ctrl+wheel → zoom the canvas, anchored on
                            // the cursor. The delta is a continuous
                            // exponent, so a notched wheel (|dy| = 1 per
                            // click) and a trackpad swipe (many |dy| ≪ 1
                            // events) settle on the same zoom per
                            // gesture.
                            if me.pos.y != 0.0 {
                                let factor = APP_CONFIG.read().zoom_factor();
                                self.renderer
                                    .set_zoom_scale_at_cursor(factor.powf(-me.pos.y));
                                self.renderer.request_render(&[]);
                            }
                            true
                        } else if !selected.is_empty() {
                            // Plain wheel with a selection → resize the
                            // selected drawable(s).
                            self.scroll_resize_selection(&selected, me.pos.y, &outer_sender);
                            true
                        } else {
                            // Clear residual accumulation when no resize
                            // path is active — keeps a later resize
                            // gesture from inheriting stale delta from a
                            // pan.
                            self.scroll_resize_accum = 0.0;
                            false
                        }
                    } else {
                        false
                    };

                    if resize_consumed {
                        return;
                    }

                    ie.handle_event_mouse_input(&self.renderer);

                    // Update hover cursor on motion AND on drag-end —
                    // a resize-handle drag hides the cursor (so the user
                    // can see where the dragged edge lands), and the
                    // hide stays in effect until the next motion event
                    // unless we also refresh on release.
                    if let InputEvent::Mouse(me) = &ie
                        && (me.type_ == MouseEventType::PointerPos
                            || me.type_ == MouseEventType::EndDrag)
                    {
                        let image_pos =
                            self.renderer.abs_canvas_to_image_coordinates(me.pos);
                        self.update_hover_cursor(image_pos);
                    }

                    // Implicit selection: when a non-Pointer tool is active,
                    // give the pointer tool first crack at mouse events so
                    // clicks on existing drawables select/manipulate them
                    // without forcing the user to switch to the pointer tool.
                    // The pointer tool returns *AndStopPropagation results
                    // when it actually grabs a handle/shape; on empty canvas
                    // it falls through (Unmodified/Redraw) so the active
                    // drawing tool can start a new shape.
                    let active_type = self.active_tool_type();

                    // BUT — when the active tool is editing a body (e.g.
                    // TextTool while a text is in edit mode), gestures
                    // that *land inside that body* belong to the active
                    // tool: clicking to place the caret, dragging to
                    // select text. Without this gate the PointerTool's
                    // hit-test on the committed stack would steal the
                    // click and select whatever drawable sits behind the
                    // edited text (e.g. another text box overlapping it).
                    let in_active_editing_body = if let InputEvent::Mouse(me) = &ie {
                        matches!(
                            me.type_,
                            MouseEventType::Click | MouseEventType::BeginDrag
                        ) && self
                            .active_tool
                            .borrow()
                            .editing_body_rect()
                            .map(|r| r.contains(me.pos))
                            .unwrap_or(false)
                    } else {
                        false
                    };

                    let pointer_consumed = if active_type != Tools::Pointer
                        && !in_active_editing_body
                    {
                        // Hint to the pointer which drawing tool is active
                        // so it can pass body-grabs through on type-mismatch
                        // (letting the user place a new annotation on top
                        // of a different-typed existing one).
                        self.tools
                            .get(&Tools::Pointer)
                            .borrow_mut()
                            .set_implicit_other_tool(Some(active_type));
                        let r = self
                            .tools
                            .get(&Tools::Pointer)
                            .borrow_mut()
                            .handle_event(ToolEvent::Input(ie.clone()));
                        match r {
                            ToolUpdateResult::StopPropagation
                            | ToolUpdateResult::RedrawAndStopPropagation
                            | ToolUpdateResult::RaiseAndRedrawStop(_)
                            | ToolUpdateResult::ModifyDrawable(_, _)
                            | ToolUpdateResult::ModifyDrawables(_)
                            | ToolUpdateResult::ModifyDrawableCoalesce(_, _)
                            | ToolUpdateResult::ModifyDrawablesCoalesce(_)
                            | ToolUpdateResult::DeleteDrawable(_)
                            | ToolUpdateResult::DeleteDrawables(_)
                            | ToolUpdateResult::EditTextDrawable(_) => Some(r),
                            _ => None,
                        }
                    } else {
                        None
                    };

                    if let Some(r) = pointer_consumed {
                        r
                    } else {
                        let active_tool_result = self
                            .active_tool
                            .borrow_mut()
                            .handle_event(ToolEvent::Input(ie.clone()));

                        match active_tool_result {
                            ToolUpdateResult::StopPropagation
                            | ToolUpdateResult::RedrawAndStopPropagation
                            | ToolUpdateResult::RaiseAndRedrawStop(_)
                            | ToolUpdateResult::DeleteDrawable(_)
                            | ToolUpdateResult::DeleteDrawables(_)
                            | ToolUpdateResult::ModifyDrawable(_, _)
                            | ToolUpdateResult::ModifyDrawables(_)
                            | ToolUpdateResult::ModifyDrawableCoalesce(_, _)
                            | ToolUpdateResult::ModifyDrawablesCoalesce(_)
                            | ToolUpdateResult::EditTextDrawable(_)
                            | ToolUpdateResult::Commit(_) => active_tool_result,
                            _ => {
                                if let Some(result) = ie.handle_mouse_event(&self.renderer) {
                                    result
                                } else {
                                    active_tool_result
                                }
                            }
                        }
                    }
                }
            }
            SketchBoardInput::ToolbarEvent(toolbar_event) => {
                self.handle_toolbar_event(toolbar_event, sender)
            }
            SketchBoardInput::RenderResult(img, action) => {
                self.handle_render_result(img, action, sender);
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::RenderResultFollowup(pix_buf, action, filename) => {
                if filename.is_some() {
                    *self.last_saved_filepath.borrow_mut() = filename;
                }
                self.handle_render_result_with_pixbuf(pix_buf, action, sender);
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::CommitEvent(txt) => {
                self.handle_text_commit(txt, sender);
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::Refresh => ToolUpdateResult::Redraw,
            SketchBoardInput::Exit => {
                self.handle_exit();
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::ScaleFactorChanged => {
                self.renderer.resize(0, 0);
                ToolUpdateResult::Redraw
            }
            SketchBoardInput::ZoomDisplayChanged(scale) => {
                sender
                    .output_sender()
                    .emit(SketchBoardOutput::ZoomChanged(scale));
                // Drawing cursors (Brush, Highlighter) are sized to the
                // rendered stroke at the current zoom — rebuild so the
                // double-ring matches the on-screen geometry after the
                // user zooms in or out.
                //
                // The stashed `last_hover_image_pos` is in image
                // coordinates, but zoom changes how the (unchanged)
                // screen pointer maps into image space. So the stored
                // image pos is stale after zoom; clear it (and the
                // band cache) so the cursor falls back to a style
                // size momentarily until the next motion event
                // re-runs detection at the now-correct image pos.
                // Better than rendering an anchored cursor at the
                // wrong band — that would visibly snap to a row the
                // pointer isn't actually over.
                self.last_hover_image_pos = None;
                crate::text_bands::clear_local_band_cache();
                if matches!(self.active_tool_type(), Tools::Brush | Tools::Highlighter) {
                    self.apply_idle_cursor();
                }
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::PanDisplayChanged(info) => {
                sender
                    .output_sender()
                    .emit(SketchBoardOutput::PanChanged(info));
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::ScrollbarSet(is_horizontal, value) => {
                self.renderer.set_pan_from_scrollbar(is_horizontal, value);
                ToolUpdateResult::Redraw
            }
            SketchBoardInput::PinchZoom(factor) => {
                // Each pinch tick is already a multiplicative delta
                // (relative to the previous gesture position), so
                // route it through the multiplicative zoom path —
                // accumulating across ticks produces the absolute
                // gesture scale.
                if factor > 0.0 && (factor - 1.0).abs() > f32::EPSILON {
                    self.renderer.set_zoom_scale(factor);
                    self.renderer.request_render(&[]);
                }
                ToolUpdateResult::Redraw
            }
            SketchBoardInput::ZoomCommand(cmd) => {
                self.handle_zoom_command(cmd);
                ToolUpdateResult::Redraw
            }
            SketchBoardInput::FocusCanvas => {
                // Grab immediately AND on the next idle tick. GTK
                // queues a focus restore when a popover (e.g. the
                // arrow-style dropdown) dismisses, and that restore
                // runs *after* this handler returns — so an
                // immediate-only grab loses the race and focus
                // lands back on whatever the popover came from. The
                // idle re-grab runs after the restore, reclaiming
                // focus for the canvas so the next keystroke / wheel
                // gesture works without an extra click.
                self.renderer.grab_focus();
                let renderer = self.renderer.clone();
                relm4::gtk::glib::idle_add_local_once(move || {
                    renderer.grab_focus();
                });
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::SyncFillToToolbar => {
                sender
                    .output_sender()
                    .emit(SketchBoardOutput::FillShapesChanged(self.style.fill));
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::SetAnnotationFactor(value) => {
                // Pushed by the welcome dialog Save handler and the
                // Preferences row when the user adjusts the factor.
                // Updates `self.style` so the next stroke is stamped
                // with the new factor and re-broadcasts the style to
                // the active tool so any in-progress preview (brush
                // cursor diameter, etc.) re-derives from it.
                if (self.style.annotation_size_factor - value).abs() >= f32::EPSILON {
                    self.style.annotation_size_factor = value;
                    self.active_tool
                        .borrow_mut()
                        .handle_event(ToolEvent::StyleChanged(self.style));
                    self.refresh_screen();
                }
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::ExitCropToPreviousTool => {
                // Restore whatever non-Crop tool the user had active
                // before they switched into Crop, falling back to
                // Pointer if we never recorded one (initial app state
                // where Crop is somehow the first tool picked).
                let target = self.tool_before_crop.unwrap_or(Tools::Pointer);
                self.handle_toolbar_event(
                    ToolbarEvent::ToolSelected(target),
                    sender,
                )
            }
            SketchBoardInput::ToggleLayerPanel => {
                self.layer_panel_open = !self.layer_panel_open;
                // The Paned auto-collapses its start_child slot (and
                // hides the divider) when the start_child widget is
                // not visible, so toggling visibility on
                // `layer_panel_content` is enough to show/hide the
                // whole panel column.
                self.layer_panel_content
                    .set_visible(self.layer_panel_open);
                if self.layer_panel_open {
                    // Restore the saved width so re-opening lands at
                    // the user's chosen size, not whatever the Paned
                    // happens to have remembered.
                    self.layer_panel_paned
                        .set_position(self.layer_panel_width as i32);
                    self.rebuild_layer_panel_rows_if_open();
                }
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::PanelSelectDrawable { id, additive } => {
                let pointer = self.tools.get(&Tools::Pointer);
                let mut sel = pointer.borrow().selected_drawables();
                if additive {
                    if let Some(pos) = sel.iter().position(|x| *x == id) {
                        sel.remove(pos);
                    } else {
                        sel.push(id);
                    }
                } else {
                    sel = vec![id];
                }
                pointer.borrow_mut().set_selected_drawables(sel);
                self.refresh_screen();
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::PanelToggleVisible(id) => {
                if let Some((vis, locked)) = self.renderer.drawable_flags(id) {
                    self.renderer.set_drawable_flags(id, !vis, locked);
                    self.refresh_screen();
                }
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::PanelToggleLocked(id) => {
                if let Some((vis, locked)) = self.renderer.drawable_flags(id) {
                    self.renderer.set_drawable_flags(id, vis, !locked);
                    // Locking the currently-selected drawable should drop
                    // it from the selection so the user can't continue to
                    // act on a now-frozen layer via the canvas.
                    if !locked {
                        let pointer = self.tools.get(&Tools::Pointer);
                        let mut sel = pointer.borrow().selected_drawables();
                        sel.retain(|x| *x != id);
                        pointer.borrow_mut().set_selected_drawables(sel);
                    }
                    self.refresh_screen();
                }
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::PanelMoveDrawable { id, direction } => {
                let moved = match direction {
                    PanelMoveDir::ToTop => self.renderer.move_drawable_to_top(id),
                    PanelMoveDir::Up => self.renderer.move_drawable_up(id),
                    PanelMoveDir::Down => self.renderer.move_drawable_down(id),
                    PanelMoveDir::ToBottom => self.renderer.move_drawable_to_bottom(id),
                };
                if moved {
                    self.refresh_screen();
                }
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::PanelReorderTo(new_order) => {
                if self.renderer.reorder_to(new_order) {
                    self.refresh_screen();
                }
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::PanelEditColor(id) => {
                // Open a color chooser dialog with the drawable's
                // current color pre-selected. Using `ColorChooserDialog`
                // (deprecated-but-shipped in GTK 4.10) rather than
                // `ColorDialog` because this codebase pins the
                // `gnome_42` gtk feature, which doesn't include v4.10.
                if let Some(d) = self.renderer.clone_drawable(id)
                    && let Some(style) = d.style()
                {
                    use gtk::prelude::ColorChooserExt;
                    let initial: relm4::gtk::gdk::RGBA = style.color.into();
                    let parent = self
                        .renderer
                        .root()
                        .and_then(|r| r.downcast::<gtk::Window>().ok());
                    let dialog =
                        gtk::ColorChooserDialog::new(Some("Pick color"), parent.as_ref());
                    dialog.set_use_alpha(true);
                    dialog.set_rgba(&initial);
                    let sender = outer_sender.input_sender().clone();
                    dialog.connect_response(move |dlg, resp| {
                        if resp == gtk::ResponseType::Ok {
                            let rgba = dlg.rgba();
                            sender
                                .send(SketchBoardInput::PanelSetColor {
                                    id,
                                    color: rgba.into(),
                                })
                                .ok();
                        }
                        dlg.close();
                    });
                    dialog.present();
                }
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::PanelSetColor { id, color } => {
                if let Some(d) = self.renderer.clone_drawable(id)
                    && let Some(mut style) = d.style()
                {
                    style.color = color;
                    let mut updated = d.clone_box();
                    updated.set_style(style);
                    self.renderer.modify(id, updated);
                    self.refresh_screen();
                }
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::PanelRename { id, name } => {
                let trimmed = name.trim();
                let new = if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                };
                if self.renderer.set_drawable_custom_name(id, new) {
                    self.refresh_screen();
                }
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::LayerPanelWidthChanged(width) => {
                if (self.layer_panel_width - width).abs() > 0.5 {
                    self.layer_panel_width = width;
                    crate::state::save_layer_panel_width(width);
                }
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::PasteImageFromClipboard(pixbuf) => {
                // Size the paste so its on-screen footprint equals the
                // pixbuf's pixel dimensions at the *current* canvas
                // scale — the same math works for both cases the user
                // cares about:
                //
                //   - Canvas screenshot: the pixbuf is already at
                //     current_zoom × DPR, so dividing by effective_scale
                //     gives an image-coord size that re-overlays the
                //     source region exactly.
                //
                //   - External screenshot: the pixbuf is at desktop
                //     resolution. Dividing by effective_scale yields
                //     an image-coord size larger than the pixbuf, so
                //     on-screen the paste lands at the captured CSS
                //     size regardless of canvas zoom — i.e. an
                //     external screenshot of 500 CSS px shows as
                //     500 CSS px in satty no matter how zoomed-out
                //     the canvas is.
                //
                // The previous "external uses pixbuf / DPR" branch
                // shrank external pastes with the canvas zoom, which
                // made them feel too small on zoomed-out canvases.
                // Collapsing to a single formula keeps "captured
                // screen size" the unified outcome.
                let scale = self.renderer.current_render_scale().max(0.001);
                let pw = pixbuf.width() as f32;
                let ph = pixbuf.height() as f32;
                let display_w = pw / scale;
                let display_h = ph / scale;

                let (img_w, img_h) = self.renderer.image_dimensions();
                let x = ((img_w as f32 - display_w) * 0.5).max(0.0);
                let y = ((img_h as f32 - display_h) * 0.5).max(0.0);
                let image = crate::tools::PastedImage::from_pixbuf(
                    &pixbuf,
                    Vec2D::new(x, y),
                    Vec2D::new(display_w, display_h),
                );
                let id = self.renderer.commit(Box::new(image));
                self.auto_resize_canvas(&[id], &outer_sender);
                // Select the just-pasted image so the user can resize
                // / move it without first clicking it.
                self.tools
                    .get(&Tools::Pointer)
                    .borrow_mut()
                    .set_selected_drawables(vec![id]);
                self.refresh_screen();
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::PanelDropOnto {
                src,
                target,
                above_target,
            } => {
                if src != target {
                    let mut order = self.renderer.all_drawable_ids();
                    let src_pos = order.iter().position(|x| *x == src);
                    let target_pos = order.iter().position(|x| *x == target);
                    if let (Some(src_pos), Some(target_pos)) = (src_pos, target_pos) {
                        // The panel lists top-of-stack first (reverse
                        // of `all_drawable_ids`), so "above target in
                        // the panel" maps to "after target in the
                        // back-to-front stack" — index target+1. "Below
                        // target in the panel" maps to target's
                        // current index (the row pushes target up).
                        let raw_insert = if above_target {
                            target_pos + 1
                        } else {
                            target_pos
                        };
                        let item = order.remove(src_pos);
                        // Removing src shifts everything after it
                        // left by one; compensate the insert index so
                        // it still resolves to the right slot.
                        let insert_at = if src_pos < raw_insert {
                            raw_insert - 1
                        } else {
                            raw_insert
                        };
                        order.insert(insert_at.min(order.len()), item);
                        if self.renderer.reorder_to(order) {
                            self.refresh_screen();
                        }
                    }
                }
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::PanelMoveSelected(direction) => {
                let pointer = self.tools.get(&Tools::Pointer);
                let sel = pointer.borrow().selected_drawables();
                if !sel.is_empty() {
                    // Iterate by current stack position so a multi-select
                    // shift doesn't have adjacent ids fighting each other
                    // (e.g. ToTop on [A, B] should leave them in their
                    // pre-move *relative* order at the top of the stack).
                    let all = self.renderer.all_drawable_ids();
                    let positions = |id: DrawableId| -> usize {
                        all.iter().position(|x| *x == id).unwrap_or(0)
                    };
                    let mut sorted: Vec<DrawableId> = sel.clone();
                    match direction {
                        PanelMoveDir::ToTop | PanelMoveDir::Up => {
                            // Move the topmost first so the lower ones still
                            // have room to move up after.
                            sorted.sort_by_key(|id| std::cmp::Reverse(positions(*id)));
                        }
                        PanelMoveDir::ToBottom | PanelMoveDir::Down => {
                            sorted.sort_by_key(|id| positions(*id));
                        }
                    }
                    let mut moved = false;
                    for id in sorted {
                        moved |= match direction {
                            PanelMoveDir::ToTop => self.renderer.move_drawable_to_top(id),
                            PanelMoveDir::Up => self.renderer.move_drawable_up(id),
                            PanelMoveDir::Down => self.renderer.move_drawable_down(id),
                            PanelMoveDir::ToBottom => {
                                self.renderer.move_drawable_to_bottom(id)
                            }
                        };
                    }
                    if moved {
                        self.refresh_screen();
                    }
                }
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::Output(output) => {
                sender.output_sender().emit(output);
                ToolUpdateResult::Unmodified
            }
        };

        // println!(" Result={:?}", result);
        match result {
            ToolUpdateResult::Commit(drawable) => {
                let id = self.renderer.commit(drawable);
                self.auto_resize_canvas(&[id], &outer_sender);
                if APP_CONFIG.read().auto_copy() {
                    self.renderer.request_render(&[Action::SaveToClipboard]);
                }
                self.refresh_screen();
            }
            ToolUpdateResult::ModifyDrawable(id, drawable) => {
                self.renderer.modify(id, drawable);
                self.auto_resize_canvas(&[id], &outer_sender);
                if APP_CONFIG.read().auto_copy() {
                    self.renderer.request_render(&[Action::SaveToClipboard]);
                }
                self.refresh_screen();
            }
            ToolUpdateResult::ModifyDrawables(updates) => {
                let ids: Vec<crate::tools::DrawableId> =
                    updates.iter().map(|(id, _)| *id).collect();
                self.renderer.modify_many(updates);
                self.auto_resize_canvas(&ids, &outer_sender);
                if APP_CONFIG.read().auto_copy() {
                    self.renderer.request_render(&[Action::SaveToClipboard]);
                }
                self.refresh_screen();
            }
            ToolUpdateResult::ModifyDrawableCoalesce(id, drawable) => {
                self.renderer.modify_coalesce(id, drawable);
                self.auto_resize_canvas(&[id], &outer_sender);
                if APP_CONFIG.read().auto_copy() {
                    self.renderer.request_render(&[Action::SaveToClipboard]);
                }
                self.refresh_screen();
            }
            ToolUpdateResult::ModifyDrawablesCoalesce(updates) => {
                let ids: Vec<crate::tools::DrawableId> =
                    updates.iter().map(|(id, _)| *id).collect();
                self.renderer.modify_many_coalesce(updates);
                self.auto_resize_canvas(&ids, &outer_sender);
                if APP_CONFIG.read().auto_copy() {
                    self.renderer.request_render(&[Action::SaveToClipboard]);
                }
                self.refresh_screen();
            }
            ToolUpdateResult::DeleteDrawable(id) => {
                self.renderer.delete(id);
                self.auto_resize_canvas(&[id], &outer_sender);
                if APP_CONFIG.read().auto_copy() {
                    self.renderer.request_render(&[Action::SaveToClipboard]);
                }
                self.refresh_screen();
            }
            ToolUpdateResult::DeleteDrawables(ids) => {
                self.renderer.delete_many(&ids);
                self.auto_resize_canvas(&ids, &outer_sender);
                if APP_CONFIG.read().auto_copy() {
                    self.renderer.request_render(&[Action::SaveToClipboard]);
                }
                self.refresh_screen();
            }
            ToolUpdateResult::EditTextDrawable(id) => {
                self.enter_text_edit_mode(id, outer_sender.clone());
            }
            ToolUpdateResult::RaiseAndRedrawStop(id) => {
                self.renderer.reorder_to_top_coalesce(id);
                self.refresh_screen();
            }
            ToolUpdateResult::Unmodified | ToolUpdateResult::StopPropagation => (),
            ToolUpdateResult::Redraw | ToolUpdateResult::RedrawAndStopPropagation => {
                self.refresh_screen()
            }
        };

        // After every update, push the selected drawable's style to
        // the StyleToolbar so the size slider, color chip, etc. track
        // whatever shape the user currently has picked.
        self.sync_toolbar_to_selection(&outer_sender);
    }

    fn init(
        image: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let config = APP_CONFIG.read();
        let tools = ToolsManager::new();

        let im_context = gtk::IMMulticontext::new();

        // Seed `style.size` from the initial tool's saved per-tool
        // default so the very first drag-to-draw is at the user's
        // preferred size for that tool. Falls back to Style::default()
        // (Medium) when nothing has been saved yet.
        let initial_tool = config.initial_tool();
        let initial_size = crate::state::load_size_for_tool(initial_tool).unwrap_or_default();

        // Layer panel scaffold. Built up-front so the view! macro can
        // pin the Paned via `#[local_ref]`. Starts hidden (panel
        // content's `visible = false` collapses Paned's start slot so
        // the divider disappears too) and is toggled by the configured
        // shortcut + the toolbar button. Width comes from state.toml.
        let initial_panel_width = crate::state::load_layer_panel_width()
            .map(|w| w.clamp(LAYER_PANEL_MIN_WIDTH, LAYER_PANEL_MAX_WIDTH))
            .unwrap_or(LAYER_PANEL_DEFAULT_WIDTH);
        let layer_panel_content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .visible(false)
            .css_classes(["layer_panel"])
            .build();
        let layer_panel_paned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .resize_start_child(false)
            .resize_end_child(true)
            .shrink_start_child(false)
            .shrink_end_child(false)
            .position(initial_panel_width as i32)
            .start_child(&layer_panel_content)
            .build();

        let mut model = Self {
            renderer: FemtoVGArea::default(),
            active_tool: tools.get(&initial_tool),
            style: Style {
                color: crate::state::initial_color(),
                size: initial_size,
                ..Style::default()
            },
            tools,
            im_context,
            last_saved_filepath: RefCell::new(None),
            last_synced_selection: None,
            last_was_multi_selection: false,
            tool_before_crop: None,
            scroll_resize_accum: 0.0,
            last_tool_press: None,
            last_hover_image_pos: None,
            session_highlighter_opacity: None,
            session_brush_smooth: None,
            session_fill_per_tool: HashMap::new(),
            layer_panel_open: false,
            layer_panel_content,
            layer_panel_sender: None,
            layer_panel_shortcut: parse_shortcut(config.layer_panel_shortcut()),
            layer_panel_paned,
            layer_panel_width: initial_panel_width,
        };

        let pointer_tool = model.tools.get(&Tools::Pointer);
        // Seed the crop tool with the image dimensions + persisted
        // snap-to-edges preference BEFORE the renderer consumes `image`
        // — `CropTool::set_image_bounds` needs the raw pixel size to
        // know what edges to snap to.
        let image_bounds =
            crate::math::Vec2D::new(image.width() as f32, image.height() as f32);
        {
            let crop_tool = model.tools.get_crop_tool();
            let mut ct = crop_tool.borrow_mut();
            ct.set_image_bounds(image_bounds);
            ct.set_snap_to_edges(crate::state::load_snap_to_edges().unwrap_or(true));
        }
        // Re-hydrate per-tool variant preferences from persisted state.
        // Arrow geometry and blur algorithm auto-save on every change
        // (see the ToolbarEvent handlers above), so re-loading them
        // here means the next launch opens each tool on the variant
        // the user last picked.
        if let Some(style) = crate::state::load_arrow_style() {
            model
                .tools
                .get(&Tools::Arrow)
                .borrow_mut()
                .set_arrow_style(style);
        }
        if let Some(style) = crate::state::load_blur_style() {
            model
                .tools
                .get(&Tools::Blur)
                .borrow_mut()
                .set_blur_style(style);
        }
        if let Some(bg) = crate::state::load_text_background() {
            model
                .tools
                .get(&Tools::Text)
                .borrow_mut()
                .set_text_background(bg);
        }
        if let Some(style) = crate::state::load_highlighter_style() {
            model
                .tools
                .get(&Tools::Highlighter)
                .borrow_mut()
                .set_highlighter_style(style);
        }
        // Cache the input sender for panel row/footer builders so they
        // don't have to receive `ComponentSender` through every call.
        model.layer_panel_sender = Some(sender.input_sender().clone());

        // Single panel-level drop target. Owns the drop indicator
        // state so adjacent rows never simultaneously show "below" /
        // "above" lines — the previous per-row design would briefly
        // double-render the indicator when the cursor crossed the
        // boundary between two rows because each row's enter/leave
        // pair raced independently. Now one walk through the rows
        // computes the target + above/below from the cursor's y in
        // panel-local coords.
        {
            let drop_target = gtk::DropTarget::new(
                u64::static_type(),
                gtk::gdk::DragAction::MOVE,
            );
            let content = model.layer_panel_content.clone();
            let input_sender = sender.input_sender().clone();
            // `connect_enter` is also wired (even though `motion`
            // would fire on its own) so the indicator is in the
            // right place on the very first frame of the hover,
            // before the first synthetic motion event arrives.
            {
                let content = content.clone();
                drop_target.connect_enter(move |_dt, _x, y| {
                    clear_drop_indicators(&content);
                    if let Some((row, above)) = drop_target_row(&content, y) {
                        row.add_css_class(if above {
                            "drop_above"
                        } else {
                            "drop_below"
                        });
                    }
                    gtk::gdk::DragAction::MOVE
                });
            }
            {
                let content = content.clone();
                drop_target.connect_motion(move |_dt, _x, y| {
                    clear_drop_indicators(&content);
                    if let Some((row, above)) = drop_target_row(&content, y) {
                        row.add_css_class(if above {
                            "drop_above"
                        } else {
                            "drop_below"
                        });
                    }
                    gtk::gdk::DragAction::MOVE
                });
            }
            {
                let content = content.clone();
                drop_target.connect_leave(move |_dt| {
                    clear_drop_indicators(&content);
                });
            }
            {
                let content = content.clone();
                drop_target.connect_drop(move |_dt, value, _x, y| {
                    clear_drop_indicators(&content);
                    let Ok(src_u64) = value.get::<u64>() else {
                        return false;
                    };
                    let Some((row, above_target)) = drop_target_row(&content, y)
                    else {
                        return false;
                    };
                    let Some(target_u64) = row
                        .widget_name()
                        .as_str()
                        .strip_prefix("layer-row-")
                        .and_then(|s| s.parse::<u64>().ok())
                    else {
                        return false;
                    };
                    input_sender
                        .send(SketchBoardInput::PanelDropOnto {
                            src: DrawableId(src_u64),
                            target: DrawableId(target_u64),
                            above_target,
                        })
                        .ok();
                    true
                });
            }
            content.add_controller(drop_target);
        }

        // Subscribe to the Paned's position changes so user drags of
        // its separator update our cached width AND persist to state.
        // GtkPaned doesn't expose a "drag-end" signal, so we save on
        // every change — the writes are cheap (~few/second during a
        // drag) and survival across crashes outweighs micro-overhead.
        // Clamp on the way in so an out-of-range Paned position (e.g.
        // a giant window briefly setting position=600) doesn't get
        // persisted past `LAYER_PANEL_MAX_WIDTH`.
        {
            let paned = model.layer_panel_paned.clone();
            let sender = sender.input_sender().clone();
            paned.clone().connect_position_notify(move |p| {
                let raw = p.position() as f32;
                let clamped = raw.clamp(LAYER_PANEL_MIN_WIDTH, LAYER_PANEL_MAX_WIDTH);
                if (clamped - raw).abs() > 0.5 {
                    paned.set_position(clamped as i32);
                    return;
                }
                sender
                    .send(SketchBoardInput::LayerPanelWidthChanged(clamped))
                    .ok();
            });
        }

        let area = &mut model.renderer;
        area.init(
            sender.input_sender().clone(),
            model.tools.get_crop_tool(),
            model.active_tool.clone(),
            pointer_tool,
            image,
        );
        // `#[local_ref]` binding for the view! macro. Immutable borrow
        // of a field different from `area` so Rust's field-level borrow
        // splitting keeps both refs live through `view_output!()`.
        let layer_panel_paned_ref = &model.layer_panel_paned;
        // Push the initial spotlight darkness so the renderer agrees
        // with the toolbar slider on the very first frame (otherwise
        // an existing-spotlight image rendered before the user has
        // touched the slider would use the renderer's hard-coded
        // default rather than the persisted slider value).
        area.set_spotlight_darkness(model.style.spotlight_darkness);

        // Shared state for the trackpad-pinch gesture. `begin` resets
        // it to 1.0 (the gesture-start scale); `scale-changed` reads
        // the previous value to compute the per-frame multiplicative
        // delta before storing the new absolute scale. Lives outside
        // the model because both callbacks need cheap concurrent
        // access and a `Cell<f32>` is plenty.
        let pinch_last = std::rc::Rc::new(std::cell::Cell::new(1.0_f32));

        let widgets = view_output!();

        model.im_context.set_client_widget(Some(&model.renderer));
        model.im_context.set_use_preedit(true);

        if let Ok(module) = std::env::var("GTK_IM_MODULE")
            && (module.eq_ignore_ascii_case("fcitx") || module.eq_ignore_ascii_case("fcitx5"))
        {
            model.im_context.set_context_id(Some("fcitx"));
        }

        {
            let sender = sender.input_sender().clone();
            model.im_context.connect_commit(move |_cx, txt| {
                sender.emit(SketchBoardInput::new_commit_event(TextEventMsg::Commit(
                    txt.to_string(),
                )));
            });
        }

        {
            let sender = sender.input_sender().clone();
            model.im_context.connect_preedit_changed(move |cx| {
                let (text, attrs, cursor) = cx.preedit_string();
                let cursor_chars = if cursor >= 0 {
                    Some(cursor as usize)
                } else {
                    None
                };
                let spans = spans_from_pango_attrs(text.as_str(), Some(attrs));
                sender.emit(SketchBoardInput::new_commit_event(TextEventMsg::Preedit {
                    text: text.to_string(),
                    cursor_chars,
                    spans,
                }));
            });
        }

        {
            let sender = sender.input_sender().clone();
            model.im_context.connect_preedit_end(move |_cx| {
                sender.emit(SketchBoardInput::new_commit_event(TextEventMsg::PreeditEnd));
            });
        }

        let focus_controller = gtk::EventControllerFocus::new();
        {
            let im_context = model.im_context.clone();
            focus_controller.connect_enter(move |_| {
                im_context.focus_in();
            });
        }
        {
            let im_context = model.im_context.clone();
            focus_controller.connect_leave(move |_| {
                im_context.focus_out();
            });
        }
        model.renderer.add_controller(focus_controller);

        let widget_ref: gtk::Widget = model.renderer.clone().upcast();
        model
            .active_tool
            .borrow_mut()
            .set_im_context(Some(crate::tools::InputContext {
                im_context: model.im_context.clone(),
                widget: widget_ref,
            }));

        // Inject the drawable store into both the active tool and the pointer
        // tool. The pointer tool also handles implicit selection while another
        // tool is active, so it always needs a live renderer handle.
        let store: Rc<dyn DrawableStore> = Rc::new(model.renderer.clone());
        model.active_tool.borrow_mut().set_drawable_store(store.clone());
        model
            .tools
            .get(&Tools::Pointer)
            .borrow_mut()
            .set_drawable_store(store);

        // Fire the initial tool's `Activated` hook (and the
        // matching renderer/sender plumbing) so tools that seed
        // state on first entry — Crop's "create a full-image
        // seed rectangle with handles" being the canonical
        // example — actually run that seed code on launch. Without
        // this, `--initial-tool crop` boots without any visible
        // crop frame until the user exits the tool and re-enters.
        model.renderer.set_active_tool(model.active_tool.clone());
        model
            .active_tool
            .borrow_mut()
            .set_sender(sender.input_sender().clone());
        model
            .active_tool
            .borrow_mut()
            .handle_event(ToolEvent::StyleChanged(model.style));
        let _ = model
            .active_tool
            .borrow_mut()
            .handle_event(ToolEvent::Activated);

        ComponentParts { model, widgets }
    }
}

impl KeyEventMsg {
    pub fn new(key: Key, code: u32, modifier: ModifierType) -> Self {
        Self {
            key,
            code,
            modifier,
        }
    }

    /// Matches one of providen keys. The modifier is not considered.
    /// And the key has more priority over keycode.
    fn is_one_of(&self, key: Key, code: KeyMappingId) -> bool {
        // INFO: on linux the keycode from gtk4 is evdev keycode, so need to match by him if need
        // to use layout-independent shortcuts. And notice that there is subtraction by 8, it's
        // because of x11 compatibility in which the keycodes are in range [8,255]. So need shift
        // them to get correct evdev keycode.
        let keymap = KeyMap::from(code);
        self.key == key || self.code as u16 - 8 == keymap.evdev
    }
}

#[cfg(test)]
mod tests {
    use super::SketchBoard;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock before Unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!("satty-{name}-{nanos}"));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn save_as_initial_dir_uses_remembered_existing_directory() {
        let temp = TempDir::new("remembered-dir");
        let remembered_dir = temp.path().join("remembered");
        let fallback_dir = temp.path().join("fallback");
        fs::create_dir_all(&remembered_dir).expect("create remembered dir");
        fs::create_dir_all(&fallback_dir).expect("create fallback dir");

        let state_file = temp.path().join("state").join("save_as_last_dir");
        fs::create_dir_all(state_file.parent().expect("state parent")).expect("create state dir");
        fs::write(&state_file, remembered_dir.to_string_lossy().as_bytes())
            .expect("write state file");

        let initial_dir = SketchBoard::save_as_initial_dir(
            Some(&state_file),
            Some(&fallback_dir.join("screenshot.png")),
        );

        assert_eq!(initial_dir, Some(remembered_dir));
    }

    #[test]
    fn save_as_initial_dir_falls_back_when_remembered_directory_is_invalid() {
        let temp = TempDir::new("invalid-remembered-dir");
        let fallback_dir = temp.path().join("fallback");
        fs::create_dir_all(&fallback_dir).expect("create fallback dir");

        let state_file = temp.path().join("save_as_last_dir");
        fs::write(
            &state_file,
            temp.path().join("missing").to_string_lossy().as_bytes(),
        )
        .expect("write invalid state file");

        let initial_dir = SketchBoard::save_as_initial_dir(
            Some(&state_file),
            Some(&fallback_dir.join("screenshot.png")),
        );

        assert_eq!(initial_dir, Some(fallback_dir));
    }

    #[test]
    fn save_as_initial_dir_handles_missing_state_and_output_path() {
        let initial_dir = SketchBoard::save_as_initial_dir(None, None);

        assert_eq!(initial_dir, None);
    }

    #[test]
    fn remember_save_as_dir_creates_state_file() {
        let temp = TempDir::new("remember-save-as-dir");
        let saved_dir = temp.path().join("saved");
        fs::create_dir_all(&saved_dir).expect("create saved dir");
        let state_dir = temp.path().join("state");
        fs::create_dir_all(&state_dir).expect("create state dir");
        let state_file = state_dir.join("save_as_last_dir");

        SketchBoard::write_save_as_last_dir(&state_file, &saved_dir.join("image.png"));

        let remembered_dir = fs::read_to_string(state_file).expect("read state file");
        assert_eq!(remembered_dir, saved_dir.to_string_lossy());
    }

    #[test]
    fn write_save_as_last_dir_ignores_unwritable_state_path() {
        let temp = TempDir::new("unwritable-state-path");
        let saved_dir = temp.path().join("saved");
        fs::create_dir_all(&saved_dir).expect("create saved dir");

        SketchBoard::write_save_as_last_dir(temp.path(), &saved_dir.join("image.png"));
    }
}
