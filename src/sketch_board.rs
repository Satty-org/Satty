use anyhow::anyhow;

use femtovg::imgref::Img;
use femtovg::rgb::{ComponentBytes, RGBA};
use keycode::{KeyMap, KeyMappingId};
use relm4::gtk::gdk_pixbuf::Pixbuf;
use relm4::gtk::gdk_pixbuf::glib::Bytes;
use std::cell::RefCell;
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
    Output(SketchBoardOutput),
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
    /// `None` means selection has been cleared (multi-select or
    /// nothing selected) — the toolbar then keeps its last value.
    SelectionStyleChanged(Option<Style>),
    /// Sketch board changed the active tool's size programmatically
    /// (e.g. Shift+wheel over the canvas with no selection). The
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
    /// Plain wheel/trackpad scroll — used to PAN the canvas. The
    /// scroll delta is packed into `MouseEventMsg.pos` (`pos.x = dx`,
    /// `pos.y = dy`).
    PanScroll,
    /// Modified scroll (Super held) — used to ZOOM. Delta in
    /// `MouseEventMsg.pos.y`.
    Scroll,
    PointerPos,
    Release,
    //Motion(Vec2D),
}

#[derive(Debug, Clone, Copy)]
pub struct MouseEventMsg {
    pub type_: MouseEventType,
    pub button: MouseButton,
    pub modifier: ModifierType,
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

    pub fn new_scroll_event(delta_y: f64) -> SketchBoardInput {
        SketchBoardInput::InputEvent(InputEvent::Mouse(MouseEventMsg {
            type_: MouseEventType::Scroll,
            button: MouseButton::Middle,
            n_pressed: 0,
            modifier: ModifierType::empty(),
            pos: Vec2D::new(0.0, delta_y as f32),
            release: false,
        }))
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

                MouseEventType::Scroll => {
                    // Treat scroll delta as a *continuous* zoom
                    // exponent rather than a discrete step. A notched
                    // mouse wheel reports |dy| = 1.0 per click, so
                    // `factor^(-dy)` reduces to the old behavior
                    // (factor / 1·factor per click). A trackpad
                    // emits many smooth events with |dy| ≪ 1.0 per
                    // event, so the previous "any nonzero dy = one
                    // full zoom step" rule made trackpad zoom feel
                    // ~10× too aggressive. With the exponent, a full
                    // trackpad swipe accumulates to roughly one
                    // mouse-wheel-click of zoom — same end-state, no
                    // sprint.
                    if me.pos.y != 0.0 {
                        let factor = APP_CONFIG.read().zoom_factor();
                        let multiplier = factor.powf(-me.pos.y);
                        renderer.set_zoom_scale(multiplier);
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
    last_synced_selection: Option<(DrawableId, crate::style::Size, f32)>,
    /// The tool that was active just before the user switched into
    /// Crop. Captured in `handle_toolbar_event` and used by the Esc
    /// path in `CropTool` to return the user to where they were
    /// rather than dropping them on Pointer. `None` means we haven't
    /// recorded anything yet (initial app state) — the fallback is
    /// Pointer in that case.
    tool_before_crop: Option<Tools>,
    /// Accumulator for the scroll-resize gesture (selection-wheel and
    /// Shift+wheel). A notched mouse wheel reports |dy| = 1.0 per
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
}

/// Max gap (ms) between two presses of the same tool-shortcut key
/// for the second press to register as a "cycle" instead of a
/// re-selection. Tuned tight so an inadvertent double-press still
/// reads as the user intentionally drumming.
const TOOL_CYCLE_MS: u64 = 500;

impl SketchBoard {
    fn refresh_screen(&mut self) {
        self.renderer.queue_render();
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
        // Clear pointer's selection state so the post-delete frame
        // doesn't render handles around now-deleted IDs.
        pointer_tool.borrow_mut().set_selected_drawables(Vec::new());
        if selected.len() == 1 {
            ToolUpdateResult::DeleteDrawable(selected[0])
        } else {
            ToolUpdateResult::DeleteDrawables(selected)
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
                if dx < 0.0
                    && b.pos.x + dx < 0.0
                    && b.pos.x + b.size.x - dx <= img_w
                {
                    dx = -dx;
                } else if dx > 0.0
                    && b.pos.x + b.size.x + dx > img_w
                    && b.pos.x - dx >= 0.0
                {
                    dx = -dx;
                }
                if dy > 0.0
                    && b.pos.y + b.size.y + dy > img_h
                    && b.pos.y - dy >= 0.0
                {
                    dy = -dy;
                } else if dy < 0.0
                    && b.pos.y + dy < 0.0
                    && b.pos.y + b.size.y - dy <= img_h
                {
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

    /// Advance the active tool's style variant by one. Triggered by a
    /// Re-shape every currently-selected Arrow drawable to the given
    /// style. Wired into `ArrowStyleSelected` (popover-click and
    /// double-tap cycle paths both flow through it) so the picker
    /// retroactively edits the canvas instead of only affecting
    /// future strokes. Mirrors the existing TextBackground path.
    fn apply_arrow_style_to_selection(
        &mut self,
        style: crate::tools::ArrowStyle,
    ) -> ToolUpdateResult {
        let selected_ids = self
            .tools
            .get(&Tools::Pointer)
            .borrow()
            .selected_drawables();
        let mut updates: Vec<(DrawableId, Box<dyn Drawable>)> = Vec::new();
        for id in selected_ids {
            if let Some(mut d) = self.renderer.clone_drawable(id)
                && d.arrow_style().is_some()
            {
                d.set_arrow_style_on_drawable(style);
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

    /// Same as `apply_arrow_style_to_selection` for Blur drawables.
    fn apply_blur_style_to_selection(
        &mut self,
        style: crate::tools::BlurStyle,
    ) -> ToolUpdateResult {
        let selected_ids = self
            .tools
            .get(&Tools::Pointer)
            .borrow()
            .selected_drawables();
        let mut updates: Vec<(DrawableId, Box<dyn Drawable>)> = Vec::new();
        for id in selected_ids {
            if let Some(mut d) = self.renderer.clone_drawable(id)
                && d.blur_style().is_some()
            {
                d.set_blur_style_on_drawable(style);
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

    /// Re-run the brush smoothing pipeline on every currently-selected
    /// brush annotation at the given level. Mirrors
    /// `apply_arrow_style_to_selection` — gates on the drawable's
    /// `smooth_level()` so the caller can fall through to the
    /// "treat as default" branch when nothing brush is selected.
    fn apply_brush_smooth_to_selection(&mut self, level: usize) -> ToolUpdateResult {
        let selected_ids = self
            .tools
            .get(&Tools::Pointer)
            .borrow()
            .selected_drawables();
        let mut updates: Vec<(DrawableId, Box<dyn Drawable>)> = Vec::new();
        for id in selected_ids {
            if let Some(mut d) = self.renderer.clone_drawable(id)
                && d.smooth_level().is_some()
            {
                d.set_smooth_level(level);
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

    /// Read the current variant for the tool's cycle. When a single
    /// drawable of the matching type is selected, prefer its style
    /// over the global default — so cycling operates on the thing
    /// the user has on screen, not the stale tool default. Falls
    /// back to the persisted default when nothing relevant is
    /// selected.
    fn cycle_seed_arrow(&self) -> crate::tools::ArrowStyle {
        let selected = self
            .tools
            .get(&Tools::Pointer)
            .borrow()
            .selected_drawables();
        if selected.len() == 1
            && let Some(s) = self
                .renderer
                .clone_drawable(selected[0])
                .and_then(|d| d.arrow_style())
        {
            return s;
        }
        crate::state::load_arrow_style().unwrap_or_default()
    }
    fn cycle_seed_blur(&self) -> crate::tools::BlurStyle {
        let selected = self
            .tools
            .get(&Tools::Pointer)
            .borrow()
            .selected_drawables();
        if selected.len() == 1
            && let Some(s) = self
                .renderer
                .clone_drawable(selected[0])
                .and_then(|d| d.blur_style())
        {
            return s;
        }
        crate::state::load_blur_style().unwrap_or_default()
    }
    fn cycle_seed_text(&self) -> crate::tools::TextBackground {
        let selected = self
            .tools
            .get(&Tools::Pointer)
            .borrow()
            .selected_drawables();
        if selected.len() == 1
            && let Some(s) = self
                .renderer
                .clone_drawable(selected[0])
                .and_then(|d| d.text_background())
        {
            return s;
        }
        crate::state::load_text_background().unwrap_or_default()
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
        use crate::tools::{ArrowStyle, BlurStyle, HighlighterStyle, TextBackground};
        match tool {
            Tools::Arrow => {
                // Seed off the selected arrow (if any) so cycling
                // operates on what the user is actually editing.
                // Falls back to the persisted default when nothing
                // matching is selected.
                let next = match self.cycle_seed_arrow() {
                    ArrowStyle::Standard => ArrowStyle::Fancy,
                    ArrowStyle::Fancy => ArrowStyle::Curved,
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

    fn handle_toolbar_event(
        &mut self,
        toolbar_event: ToolbarEvent,
        sender: ComponentSender<Self>,
    ) -> ToolUpdateResult {
        match toolbar_event {
            ToolbarEvent::ToolSelected(tool) => {
                // Capture the prior non-Crop tool right before we
                // switch — the Esc handler in `CropTool` uses this to
                // restore the user to the tool they had before
                // entering Crop, rather than dropping them on Pointer.
                let current_tool = self.active_tool_type();
                if tool == Tools::Crop && current_tool != Tools::Crop {
                    self.tool_before_crop = Some(current_tool);
                }
                // Re-entering Spotlight or Highlighter snaps the
                // slider back to the saved default (or the system
                // detent if no save). In-session edits during a
                // single tool stretch persist across multiple new
                // shapes; switching away wipes them so the next
                // entry starts from a known baseline.
                if tool != current_tool {
                    match tool {
                        Tools::Spotlight => {
                            let saved =
                                crate::state::load_spotlight_darkness().unwrap_or(0.50);
                            self.style.spotlight_darkness = saved;
                            self.renderer.set_spotlight_darkness(saved);
                            sender
                                .output_sender()
                                .emit(SketchBoardOutput::SpotlightDarknessReset(saved));
                        }
                        Tools::Highlighter => {
                            let saved = crate::state::load_highlighter_opacity()
                                .unwrap_or(0.40);
                            self.style.highlighter_opacity = saved;
                            sender
                                .output_sender()
                                .emit(SketchBoardOutput::HighlighterOpacityReset(saved));
                        }
                        Tools::Brush => {
                            // Same snapback semantics as the other
                            // tool-specific sliders: re-entering Brush
                            // pulls the saved default off state.toml
                            // (falling back to the config / built-in
                            // 2) and pushes it into APP_CONFIG so the
                            // next stroke uses it.
                            //
                            // BUT: if the user got here because they
                            // clicked an existing brush annotation
                            // (the auto-tool-switch in
                            // `sync_toolbar_to_selection`), use THAT
                            // annotation's stored level instead — so
                            // the slider lands on the value the
                            // selected stroke was drawn with rather
                            // than overwriting it with the saved
                            // default a frame later. Subsequent slider
                            // tweaks re-smooth the selected stroke;
                            // re-entering Brush without a selection
                            // (or selecting nothing) falls back to
                            // the saved default normally.
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
                                crate::state::load_brush_post_smooth_iterations()
                                    .unwrap_or_else(|| {
                                        APP_CONFIG.read().brush_post_smooth_iterations()
                                    })
                            });
                            // Only update APP_CONFIG when this is a
                            // genuine snapback (no selection driving
                            // the value) — otherwise the user's slider
                            // tweaks for the selected annotation would
                            // bleed into the default for the *next*
                            // new stroke.
                            if selected_level.is_none() {
                                APP_CONFIG
                                    .write()
                                    .set_brush_post_smooth_iterations(saved);
                            }
                            sender
                                .output_sender()
                                .emit(SketchBoardOutput::BrushPostSmoothReset(saved));
                        }
                        Tools::Rectangle | Tools::Ellipse => {
                            // Same snapback for per-tool fill. Saved
                            // default wins if the user has explicitly
                            // pinned one for THIS shape tool;
                            // otherwise leave style.fill alone so an
                            // in-session toggle survives switching
                            // between Rectangle and Ellipse.
                            if let Some(saved) =
                                crate::state::load_fill_for_tool(tool)
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
                    // TextTool emits ModifyDrawable on tool-switch when
                    // finalizing a re-edit; replace the existing
                    // drawable in-place.
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

                match activate_result {
                    ToolUpdateResult::Unmodified => deactivate_result,
                    _ => activate_result,
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
                // shape's fill state before flipping it. Without this,
                // if the user clicks a shape whose fill state disagrees
                // with `self.style.fill` (which may carry a stale
                // value from an earlier toggle), the first press just
                // brings the global into sync with the shape and the
                // visible state doesn't change — the user has to press
                // again to actually flip. Reading the selection's own
                // fill makes a single press always produce a visible
                // flip on the selected shape.
                let selected = self
                    .tools
                    .get(&Tools::Pointer)
                    .borrow()
                    .selected_drawables();
                if let Some(fill) = selected.iter().find_map(|id| {
                    self.renderer
                        .clone_drawable(*id)
                        .and_then(|d| d.style())
                        .map(|s| s.fill)
                }) {
                    self.style.fill = fill;
                }
                self.style.fill = !self.style.fill;
                // Toast announces the new state so a keyboard toggle
                // (`F`) reads as feedback, not a silent change.
                let label = if self.style.fill { "Fill shape" } else { "No fill" };
                sender
                    .output_sender()
                    .emit(SketchBoardOutput::ShowCycleToast(label.to_string()));
                self.dispatch_style_change()
            }
            ToolbarEvent::AnnotationSizeChanged(value) => {
                self.style.annotation_size_factor = value;
                self.dispatch_style_change()
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
            ToolbarEvent::FocusCanvas => {
                self.renderer.grab_focus();
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
                }
                selection_result
            }
            ToolbarEvent::SaveBrushPostSmoothAsDefault => {
                crate::state::save_brush_post_smooth_iterations(
                    APP_CONFIG.read().brush_post_smooth_iterations(),
                );
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
                    self.tools.get_crop_tool().borrow_mut().revert_to_seed();
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
                if let Some((new_w, new_h)) = self.renderer.rotate_image_ccw() {
                    let crop_tool = self.tools.get_crop_tool();
                    let mut ct = crop_tool.borrow_mut();
                    // Update bounds the snap-to-edges + seed paths
                    // read from, then reseed so the crop rect lands
                    // fresh on the rotated image (the old rect's
                    // (pos, size) refer to coordinates that no
                    // longer exist in the new orientation).
                    ct.set_image_bounds(crate::math::Vec2D::new(new_w, new_h));
                    ct.revert_to_seed();
                    drop(ct);
                    // Drop any prior user zoom / drag offset so the
                    // rotated image re-engages auto-fit against the
                    // canvas. Without this, a 90 ° turn from landscape
                    // to portrait leaves the image partially clipped
                    // by the now-too-narrow canvas — same fit logic
                    // ResizeImage already applies for the same reason.
                    self.renderer.reset_size(0.0);
                    sender
                        .output_sender()
                        .emit(SketchBoardOutput::ImageDimensionsChanged {
                            width: new_w as i32,
                            height: new_h as i32,
                        });
                    // Ask the window to fit the rotated content (up to
                    // 90 % of the monitor — `window_size_for_content`
                    // applies that cap). If the rotated image still
                    // fits at 1:1 within the cap, the on-screen zoom
                    // stays the same; only when it can't does auto-fit
                    // shrink it. `revert_to_seed` already emits this,
                    // but the crop-tool sender path doesn't seem to
                    // make it back here, so emit directly on the
                    // output sender we already have in hand.
                    sender
                        .output_sender()
                        .emit(SketchBoardOutput::ContentSizeChanged {
                            width: new_w,
                            height: new_h,
                        });
                }
                ToolUpdateResult::Redraw
            }
            ToolbarEvent::ResizeImage { width, height } => {
                if let Some((new_w, new_h)) = self.renderer.resize_image(width, height) {
                    let crop_tool = self.tools.get_crop_tool();
                    let mut ct = crop_tool.borrow_mut();
                    ct.set_image_bounds(crate::math::Vec2D::new(new_w, new_h));
                    ct.revert_to_seed();
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
                    let is_cycle = match self.last_tool_press {
                        Some((prev_ch, prev_t))
                            if prev_ch == ch
                                && now.duration_since(prev_t).as_millis()
                                    <= TOOL_CYCLE_MS as u128
                                && self.active_tool_type() == tool =>
                        {
                            true
                        }
                        _ => false,
                    };
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
                            .emit(SketchBoardOutput::ColorSwitchShortcut(index_digit as u64));
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
    /// `SelectionStyleChanged` with the new drawable's style. Skips
    /// re-emit for multi-select or empty selection so the toolbar
    /// keeps its last value in those cases.
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
            .map(|(id, s)| (*id, s.size, s.annotation_size_factor));
        if new_key == self.last_synced_selection {
            return;
        }
        self.last_synced_selection = new_key;
        sender
            .output_sender()
            .emit(SketchBoardOutput::SelectionStyleChanged(
                new_style.map(|(_, s)| s),
            ));
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
                sender.input(SketchBoardInput::ToolbarEvent(
                    ToolbarEvent::ToolSelected(target),
                ));
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

        // 2. Otherwise, any drawable under the pointer → grab.
        if cursor.is_none()
            && self
                .renderer
                .hit_test(image_pos, crate::tools::HIT_TOLERANCE)
                .is_some()
        {
            cursor = Some("grab");
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
        gtk::Box {
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
                        // Single inversion site for the canvas. Flips both
                        // axes so zoom (Super+wheel), pan (plain wheel),
                        // and the scroll-resize gestures (Shift / selection
                        // + wheel) all reverse together — keeps the
                        // preference's polarity consistent across every
                        // downstream consumer.
                        let (dx, dy) = if APP_CONFIG.read().invert_scrolling() {
                            (-dx, -dy)
                        } else {
                            (dx, dy)
                        };
                        if modifier.contains(gtk::gdk::ModifierType::SUPER_MASK) {
                            // Super + wheel → zoom. Returning Stop here
                            // is our best-effort attempt to override
                            // Hyprland's workspace-switch binding while
                            // the cursor is inside Satty; Hyprland may
                            // still grab the event at the compositor
                            // level (its `bind = SUPER, mouse_*` rules
                            // fire before GTK sees the event). If
                            // workspace-switching wins, the user can
                            // configure a `windowrulev2 = ...` to
                            // exempt `class:com.gabm.satty` from the
                            // Super+scroll binding.
                            sender.input(SketchBoardInput::new_scroll_event(dy));
                        } else {
                            // Plain wheel / trackpad pan → move the
                            // canvas. GTK reports the delta already
                            // sign-corrected for the OS's natural-
                            // scrolling preference (natural-on inverts
                            // dy at the compositor layer), so we just
                            // pass the deltas straight through to the
                            // panner. The PanScroll handler scales
                            // them into pixels (or hijacks for
                            // size-resize on Shift, per the modifier
                            // we forward below).
                            //
                            // We DON'T do the Shift+vertical→horizontal
                            // remap here anymore — Shift now signals
                            // "resize on scroll" in the input handler,
                            // and remapping dy→dx would steal the
                            // delta. The handler does its own
                            // horizontal-pan fallback for plain Shift
                            // on a one-axis wheel.
                            sender.input(SketchBoardInput::new_pan_scroll_event(
                                dx, dy, modifier,
                            ));
                        }
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
                        // Mouse-side Super gestures (Super+scroll =
                        // zoom) are handled separately in the scroll
                        // controller and are unaffected.
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
            }
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
                        | ToolUpdateResult::DeleteDrawable(_)
                        | ToolUpdateResult::DeleteDrawables(_)
                        | ToolUpdateResult::ModifyDrawable(_, _)
                        | ToolUpdateResult::ModifyDrawables(_)
                        | ToolUpdateResult::ModifyDrawableCoalesce(_, _)
                        | ToolUpdateResult::ModifyDrawablesCoalesce(_)
                        | ToolUpdateResult::Commit(_) => active_tool_result,
                        _ => {
                            if ke.is_one_of(Key::z, KeyMappingId::UsZ)
                                && ke.modifier == ModifierType::CONTROL_MASK
                            {
                                self.handle_undo()
                            } else if ke.is_one_of(Key::y, KeyMappingId::UsY)
                                && ke.modifier == ModifierType::CONTROL_MASK
                            {
                                self.handle_redo()
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
                        let shift_held = me.modifier.contains(ModifierType::SHIFT_MASK);
                        if !selected.is_empty() {
                            // Selection + wheel → resize the selected
                            // drawable(s). Modifier-free; ignores Shift.
                            self.scroll_resize_selection(&selected, me.pos.y, &outer_sender);
                            true
                        } else if shift_held {
                            // No selection + Shift + wheel → bump the
                            // active tool's size for the next stroke.
                            self.scroll_resize_tool_size(me.pos.y, &outer_sender);
                            true
                        } else {
                            // Clear residual accumulation when neither
                            // resize path is active — keeps a later
                            // resize gesture from inheriting stale
                            // delta from a pan.
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
                        let r = self
                            .tools
                            .get(&Tools::Pointer)
                            .borrow_mut()
                            .handle_event(ToolEvent::Input(ie.clone()));
                        match r {
                            ToolUpdateResult::StopPropagation
                            | ToolUpdateResult::RedrawAndStopPropagation
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
                self.renderer.grab_focus();
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::SyncFillToToolbar => {
                sender
                    .output_sender()
                    .emit(SketchBoardOutput::FillShapesChanged(self.style.fill));
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
            tool_before_crop: None,
            scroll_resize_accum: 0.0,
            last_tool_press: None,
            last_hover_image_pos: None,
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
        let area = &mut model.renderer;
        area.init(
            sender.input_sender().clone(),
            model.tools.get_crop_tool(),
            model.active_tool.clone(),
            pointer_tool,
            image,
        );
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
