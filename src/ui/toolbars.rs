use std::{borrow::Cow, cell::RefCell, collections::HashMap, rc::Rc, time::Duration};

use crate::{
    configuration::APP_CONFIG,
    style::{Color, Size},
    tools::{ArrowStyle, BlurStyle, Tools},
};

use gtk::ToggleButton;
use relm4::gtk::gdk_pixbuf::{
    Pixbuf,
    gio::SimpleAction,
    glib::{Variant, VariantTy},
};
use relm4::{
    actions::{ActionablePlus, RelmAction, RelmActionGroup},
    gtk::{Align, gdk::RGBA, prelude::*},
    prelude::*,
};

/// Install a tooltip that re-shows reliably on every hover.
///
/// Why: GTK4's built-in tooltip system keeps a window-level "tooltip
/// recently shown / dismissed" state that only clears when the pointer
/// leaves the toplevel window. We bypass it with a per-widget
/// `gtk::Popover` driven by motion enter/leave.
///
/// Why a global tracker: GTK4's `EventControllerMotion::leave` can drop
/// when the pointer moves quickly between adjacent siblings, leaving the
/// previous widget's tooltip stuck open. We track the currently-shown
/// tooltip in a thread-local `RefCell` and dismiss it whenever a new
/// tooltip's `enter` fires — so even if `leave` never arrives, the
/// stale tooltip is forced down by the next hover.
pub trait RobustTooltipExt {
    /// Tooltip pops downward (good for top-toolbar buttons).
    fn install_tooltip(&self, text: &str);
    /// Tooltip pops upward (good for bottom-toolbar buttons so it stays
    /// inside the window).
    fn install_tooltip_above(&self, text: &str);
}

impl<T: IsA<gtk::Widget> + Clone> RobustTooltipExt for T {
    fn install_tooltip(&self, text: &str) {
        attach_tooltip(self, text, gtk::PositionType::Bottom);
    }
    fn install_tooltip_above(&self, text: &str) {
        attach_tooltip(self, text, gtk::PositionType::Top);
    }
}

/// Hover delay before any of our custom tooltips appear. Tuned to feel
/// snappy without flashing tooltips at every passing pointer movement.
const TOOLTIP_DELAY: Duration = Duration::from_millis(750);

thread_local! {
    /// The currently-shown tooltip popover, if any. Lets `show_tooltip`
    /// dismiss the previous one even when its `leave` event was dropped.
    static ACTIVE_TOOLTIP: RefCell<Option<gtk::Popover>> = const { RefCell::new(None) };
}

fn show_tooltip(popover: &gtk::Popover) {
    ACTIVE_TOOLTIP.with(|active| {
        let mut active = active.borrow_mut();
        if let Some(prev) = active.as_ref()
            && prev != popover
        {
            prev.popdown();
        }
        *active = Some(popover.clone());
    });
    popover.popup();
}

fn hide_tooltip(popover: &gtk::Popover) {
    popover.popdown();
    ACTIVE_TOOLTIP.with(|active| {
        let mut active = active.borrow_mut();
        if active.as_ref() == Some(popover) {
            *active = None;
        }
    });
}

/// Like the trait `install_tooltip{,_above}` methods but returns the
/// inner `gtk::Label` so callers can update the text later — used for
/// buttons whose tooltip describes a live state (e.g. the Fill toggle).
/// The label is part of the popover's child tree; updating its text via
/// `set_label` reflows the next time the popover shows.
fn install_dynamic_tooltip<W: IsA<gtk::Widget> + Clone>(
    widget: &W,
    initial: &str,
    position: gtk::PositionType,
) -> gtk::Label {
    attach_tooltip(widget, initial, position)
}

fn attach_tooltip<W: IsA<gtk::Widget> + Clone>(
    widget: &W,
    text: &str,
    position: gtk::PositionType,
) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .margin_start(8)
        .margin_end(8)
        .margin_top(4)
        .margin_bottom(4)
        .build();
    let popover = gtk::Popover::builder()
        .child(&label)
        .has_arrow(false)
        .autohide(false)
        .position(position)
        .build();
    popover.add_css_class("custom-tooltip");
    popover.set_can_focus(false);
    popover.set_can_target(false);

    // Push the popover a few pixels away from the widget edge so the
    // text isn't crammed against the toolbar.
    let gap = 8;
    let y_offset = match position {
        gtk::PositionType::Bottom => gap,
        gtk::PositionType::Top => -gap,
        _ => 0,
    };
    popover.set_offset(0, y_offset);
    popover.set_parent(widget);

    // `pending_show` holds the SourceId of a timer that will pop the
    // tooltip up after `TOOLTIP_DELAY`. Re-entering cancels and
    // re-arms; leaving (or destroying the widget) cancels outright.
    let pending_show: Rc<RefCell<Option<gtk::glib::SourceId>>> = Rc::new(RefCell::new(None));

    let motion = gtk::EventControllerMotion::new();
    {
        let popover = popover.clone();
        let pending_show = pending_show.clone();
        motion.connect_enter(move |_, _, _| {
            if let Some(id) = pending_show.borrow_mut().take() {
                id.remove();
            }
            let popover_for_timer = popover.clone();
            let pending_inner = pending_show.clone();
            let id = gtk::glib::timeout_add_local_once(TOOLTIP_DELAY, move || {
                pending_inner.borrow_mut().take();
                show_tooltip(&popover_for_timer);
            });
            *pending_show.borrow_mut() = Some(id);
        });
    }
    {
        let popover = popover.clone();
        let pending_show = pending_show.clone();
        motion.connect_leave(move |_| {
            if let Some(id) = pending_show.borrow_mut().take() {
                id.remove();
            }
            hide_tooltip(&popover);
        });
    }
    widget.add_controller(motion);

    // GtkPopover::set_parent attaches the popover as a child of the
    // widget; we have to unparent it explicitly before the parent is
    // finalized or GTK warns on shutdown.
    widget.connect_destroy(move |_| {
        if let Some(id) = pending_show.borrow_mut().take() {
            id.remove();
        }
        ACTIVE_TOOLTIP.with(|active| {
            let mut active = active.borrow_mut();
            if active.as_ref() == Some(&popover) {
                *active = None;
            }
        });
        popover.unparent();
    });
    label
}

pub struct ToolsToolbar {
    visible: bool,
    active_button: Option<ToggleButton>,
    tool_buttons: HashMap<Tools, ToggleButton>,
    tool_action: SimpleAction,
    /// Mirrors `tool_action`'s state in plain-Tools form. Driven by
    /// `SwitchSelectedTool` so the view! `#[watch]` rules can swap
    /// the top toolbar between its normal contents and the
    /// Crop-mode contents (aspect ratio / W×H / bg / rotate-flip /
    /// image size / Cancel-Crop). Initial value is `Pointer` —
    /// reset to the actual starting tool right before view_output!.
    current_tool: Tools,
    /// Last crop (width, height) pushed up from `CropTool`'s
    /// dimensions emit. Mirrored locally so the toolbar can both
    /// refresh the W/H entries (when they're not focused) and
    /// recompute swap-button output on click without a round-trip.
    crop_width: i32,
    crop_height: i32,
    /// Handles to the W/H text inputs so the `CropDimensionsChanged`
    /// handler can `has_focus`-check before calling `set_text` —
    /// `#[watch]`-driven `set_text` would otherwise clobber a
    /// half-typed value every drag tick.
    crop_width_entry: Option<gtk::Entry>,
    crop_height_entry: Option<gtk::Entry>,
    /// Current background image dimensions (in image-space pixels).
    /// Drives the "Image size: W × H px" MenuButton label and
    /// pre-fills the resize popover's W/H entries when it opens.
    /// Pushed up via `ImageDimensionsChanged` from main.rs at
    /// startup and after every rotate / resize.
    image_width: i32,
    image_height: i32,
    /// Handles to the resize popover's W/H entries so the open
    /// handler can pre-fill them with the current image dims
    /// (the popover opens already populated so
    /// the user only types the field they want to change).
    resize_width_entry: Option<gtk::Entry>,
    resize_height_entry: Option<gtk::Entry>,
    /// Currently-selected crop background (matte) preset.
    /// Mirrored locally so the swatch on the bg-color MenuButton
    /// can refresh via `#[watch]` whenever the user picks a new
    /// preset from the popover.
    crop_bg_color: crate::tools::CropBgColor,
    /// The "Custom Color…" row's swatch inside the bg-color popover.
    /// Stashed so the `CropBgColorSelected(Custom(...))` handler can
    /// refresh the row to reflect the user's chosen color — without
    /// this the row always shows the mid-gray placeholder from the
    /// initial build, which gives the dropdown no visual indication
    /// of what "Custom" currently means.
    crop_bg_custom_swatch: Option<gtk::Image>,
    /// Last picked `Custom(r,g,b)` value (mid-gray default before any
    /// pick). Shared with the row's click closure so opening the
    /// chooser dialog re-seeds it with the user's prior choice
    /// instead of GTK's white default. Updated in lockstep with the
    /// swatch in `CropBgColorSelected`.
    crop_bg_custom_rgb: Option<std::rc::Rc<std::cell::Cell<(f32, f32, f32)>>>,
    /// Resize-popover state shared between handler updates and
    /// the popover's imperative connect_* closures. `Rc<Cell>` so
    /// the closures (each owns a clone) can read the live values
    /// without taking `&mut self`. Updated by
    /// `ImageDimensionsChanged` / `SetDisplayScale`.
    resize_orig_dims: Option<std::rc::Rc<std::cell::Cell<(i32, i32)>>>,
    resize_display_scale: Option<std::rc::Rc<std::cell::Cell<i32>>>,
    resize_aspect_locked: Option<std::rc::Rc<std::cell::Cell<bool>>>,
    resize_units: Option<std::rc::Rc<std::cell::Cell<ResizeUnits>>>,
    /// Display device-pixel-ratio (matches main.rs's
    /// `display_scale_divisor`). All user-facing pixel values
    /// (crop W/H entries, "Image size: W × H px" label, resize
    /// popover entries) divide raw image pixels by this to show
    /// LOGICAL pixels — what the user sees on screen — and
    /// multiply typed values back to image pixels before they
    /// flow out as ToolbarEvents. Defaults to 1; main.rs pushes
    /// the real value at startup via `SetDisplayScale`.
    display_scale: i32,
    /// Currently-selected color, mirrored on the unified color-picker
    /// MenuButton's swatch. Updated whenever a palette/custom color is
    /// chosen, so the swatch reflects what subsequent annotations will use.
    current_color: Color,
    current_color_pixbuf: Pixbuf,
    /// Last-picked color from the ColorChooserDialog. Used as the
    /// dialog's seed value on subsequent opens and as the fallback for
    /// stale `CustomSaved` indices; *not* surfaced as a separate slot
    /// in the popover anymore (replaced by `custom_colors`).
    custom_color: Color,
    /// Persisted "saved custom colors" — sparse slot list. `Some(color)`
    /// renders as a filled swatch addressable via
    /// `ColorButtons::CustomSaved(i)`; `None` renders as a dashed
    /// placeholder. Empty mid-list slots are intentional (created by
    /// dragging a swatch away from its slot or by deletion) and survive
    /// across launches; trailing `None`s are trimmed by `state.rs` on
    /// save.
    custom_colors: Vec<Option<Color>>,
    color_action: SimpleAction,
    /// The color-picker `MenuButton` the popover hangs off. Stored so
    /// the eyedropper-recovery path can re-open the popover *through*
    /// the MenuButton — keeping its `active` state in sync — instead
    /// of calling `popover.popup()` directly.
    color_button: Option<gtk::MenuButton>,
    /// Reference to the popover so `update` can rebuild the right
    /// column when a saved color is appended.
    color_popover: Option<gtk::Popover>,
    /// The popover's actual child is a `gtk::Stack` containing one
    /// or more grid pages — `refresh_color_popover` adds a fresh
    /// grid as a new page and flips the visible child to it, which
    /// crossfades from the previous grid over `STACK_FADE_MS`. Stored
    /// here so the refresh path doesn't have to walk the popover's
    /// child tree on every update.
    color_popover_stack: Option<gtk::Stack>,
    /// Monotonic counter so each fresh grid page gets a unique name
    /// inside the stack. The names themselves are throwaway — only
    /// uniqueness matters.
    color_popover_page_id: u64,
    /// True iff the inline color picker panel (revealed by the arrow /
    /// wheel button) is currently open. Drives the arrow icon and the
    /// revealer's `reveal_child`.
    picker_expanded: bool,
    /// `gtk::Revealer` wrapping the inline picker panel. Uses
    /// `SlideRight` transition because that's the only one that
    /// keeps the colorplane's gradient cache valid across
    /// collapse/expand toggles — Crossfade/None both collapse the
    /// child to 0×0 and the gradient never re-paints. SlideRight
    /// makes width = 0 when collapsed (so the popover narrows
    /// horizontally) at the cost of preserving the child's
    /// vertical extent (so the collapsed popover stays tall).
    picker_revealer: Option<gtk::Revealer>,
    /// The embedded `ColorChooserWidget` inside the inline picker
    /// panel. `AddCurrentPickerToCustoms` reads its `rgba` so the
    /// "+ Add to My Colors" button knows what to persist.
    picker_chooser: Option<gtk::ColorChooserWidget>,
    /// Handle to the caret icon inside the merged bottom-row "expand
    /// picker" button so the toggle handler can flip the caret
    /// direction (pan-end ↔ pan-start) without rebuilding the button.
    /// The merged button itself spans both swatch columns and carries
    /// the paint-bucket icon next to this caret.
    picker_caret_icon: Option<gtk::Image>,
    /// Currently-targeted empty saved-custom slot. Set by clicking a
    /// dashed empty placeholder in the popover; cleared on a successful
    /// `SaveCustomColor`, on selecting a filled swatch, and on popover
    /// close. When `Some(slot)` and the user clicks "+ Add to custom
    /// colors", the new color lands at `slot` instead of the first
    /// empty slot.
    selected_empty_slot: Option<usize>,
    /// Color currently being dragged within the saved-custom column,
    /// captured at drag-begin. While set, `custom_colors[origin_slot]`
    /// is `None` (the slot is logically empty while in transit). On
    /// drop, the dragged color is inserted at `dragging_preview_slot`
    /// with subsequent slots shifted down by one (preserving any
    /// existing mid-list `None`s); the original slot stays `None`,
    /// so a drag effectively *moves* the color and leaves a gap
    /// behind. `None` between drags.
    dragging_color: Option<Color>,
    /// Snapshot of `custom_colors` taken at drag-begin so a cancelled
    /// drag (drop outside the popover, Esc, etc.) can fully restore
    /// the pre-drag list. The live `custom_colors` is mutated during
    /// the drag (origin slot blanked, preview slot moves), so the
    /// snapshot is the only way back to the original layout.
    pre_drag_snapshot: Option<Vec<Option<Color>>>,
    /// While a drag is in flight, the slot index where the ghost
    /// placeholder is currently drawn — i.e. the slot the dragged
    /// color will land in if the user drops right now. Updated each
    /// time the pointer enters a new slot's drop area; rendered by
    /// `build_color_popover_grid` as a brighter outlined placeholder
    /// so the user sees where other swatches will shift to make room.
    dragging_preview_slot: Option<usize>,
    /// Active responsive layout. Driven by `SetLayout` from
    /// main.rs's window-resize listener. See `TopBarLayout` for
    /// what each variant does to the three clusters.
    layout: TopBarLayout,
    /// The Box wrapping the right cluster (color picker +
    /// separator + settings/copy/save/save-as). Captured in init
    /// so `SetLayout` can imperatively re-parent it between
    /// `normal_end_host` (Normal) and `top_wrap_row`'s end slot
    /// (Wrap).
    right_cluster: Option<gtk::Box>,
    /// The `set_end_widget` outer Box. Hosts `right_cluster` in
    /// Normal layout.
    normal_end_host: Option<gtk::Box>,
    /// Second row sitting below the main CenterBox in the
    /// toolbar's vertical wrapper. In Wrap layout it hosts
    /// `left_cluster` followed by `right_cluster` as one centered
    /// group; hidden otherwise. A centered horizontal Box (not a
    /// CenterBox) so the two clusters sit together under the
    /// centered tool row rather than splitting flush to the edges.
    top_wrap_row: Option<gtk::Box>,
    /// The Box wrapping the left cluster (1:1 / fit / reset /
    /// undo / redo). Captured in init so `SetLayout` can move it
    /// between its Normal home (`start_widget_box`) and its
    /// Wrap home (`top_wrap_row`'s start slot).
    left_cluster: Option<gtk::Box>,
    /// The CenterBox's start slot Box. Hosts `left_cluster` in
    /// Normal layout; the cluster is unparented from here on
    /// transition into Wrap.
    start_widget_box: Option<gtk::Box>,
}

impl ToolsToolbar {
    /// Regenerate the popover's grid with the current model state and
    /// crossfade to it via the embedded `gtk::Stack`. Called after
    /// saved-customs change (save / reorder / delete / live drag) so
    /// the next paint reflects the new list with a smooth fade rather
    /// than a snap. Takes `&mut self` so the monotonic page-id
    /// counter can advance.
    ///
    /// Old grid pages stay attached to the stack while a drag is in
    /// flight — the drag's source widget lives inside one of those
    /// pages, and removing the page would unparent it and cancel the
    /// drag. After the drag ends, `clean_up_old_popover_pages`
    /// reaps everything but the current visible child.
    fn refresh_color_popover(&mut self, sender: &ComponentSender<ToolsToolbar>) {
        let Some(stack) = self.color_popover_stack.clone() else {
            return;
        };
        let Some(popover) = self.color_popover.clone() else {
            return;
        };
        // Prune non-visible pages synchronously before adding the new
        // one (outside an active drag — see the note above re: keeping
        // the drag source widget parented). This caps the stack at
        // 2 children (the previously-visible page + the new page) so
        // the homogeneous+crossfade sizing can't be inflated by stale
        // pages from prior refreshes whose scheduled cleanups haven't
        // fired yet.
        if self.dragging_color.is_none() {
            clean_up_old_popover_pages(&stack);
        }
        let grid = build_color_popover_grid(self, sender, &popover);
        let name = format!("page-{}", self.color_popover_page_id);
        self.color_popover_page_id = self.color_popover_page_id.wrapping_add(1);
        stack.add_named(&grid, Some(&name));
        stack.set_visible_child(&grid);
        // Outside an active drag, prune old pages once the fade has
        // completed. During a drag, leave the previous grids attached
        // so the drag source widget stays parented.
        if self.dragging_color.is_none() {
            let stack_for_cleanup = stack.clone();
            gtk::glib::timeout_add_local_once(
                std::time::Duration::from_millis(STACK_FADE_MS as u64 + 50),
                move || {
                    clean_up_old_popover_pages(&stack_for_cleanup);
                },
            );
        }
    }

    /// Re-resolve which swatch in the popover should show as
    /// "checked" given the current `current_color`. Used after
    /// reorder/delete shuffles or removes saved-custom indices.
    ///
    /// Preserves the user's explicit pick — if the current action
    /// state still points to a swatch holding `current_color`, we
    /// leave it alone. This matters when palette + customs share a
    /// color (e.g. user clicks a custom swatch whose color also
    /// appears in the default palette, then toggles "hide default
    /// palette" off): a naive re-resolve would jump the selection
    /// over to the palette swatch and the user's intentional choice
    /// would be lost. We only re-resolve when the existing state has
    /// become invalid (color at that position changed or the slot was
    /// emptied), and even then we still prefer the palette as the
    /// fallback since palette colors are stable across sessions.
    fn sync_color_action(&self) {
        let palette = APP_CONFIG.read().color_palette().palette().to_vec();
        let current_state: Option<ColorButtons> = self
            .color_action
            .state()
            .and_then(|v| ColorButtons::from_variant(&v));
        let still_valid = current_state.is_some_and(|btn| match btn {
            ColorButtons::Palette(i) => {
                palette.get(i as usize).copied() == Some(self.current_color)
            }
            ColorButtons::CustomSaved(i) => {
                self.custom_colors.get(i as usize).copied().flatten() == Some(self.current_color)
            }
            ColorButtons::Custom => false,
        });
        if still_valid {
            return;
        }
        let button = palette
            .iter()
            .position(|c| *c == self.current_color)
            .map(|i| ColorButtons::Palette(i as u64))
            .or_else(|| {
                self.custom_colors
                    .iter()
                    .position(|slot| matches!(slot, Some(c) if *c == self.current_color))
                    .map(|i| ColorButtons::CustomSaved(i as u64))
            })
            .unwrap_or(ColorButtons::Custom);
        self.color_action.change_state(&button.to_variant());
    }

    fn map_button_to_color(&self, button: ColorButtons) -> Color {
        let config = APP_CONFIG.read();
        match button {
            ColorButtons::Palette(n) => config.color_palette().palette()[n as usize],
            ColorButtons::Custom => self.custom_color,
            ColorButtons::CustomSaved(n) => {
                // Out-of-range or now-empty indices shouldn't be reachable
                // from the UI (empty slots render as dashed placeholders
                // that don't dispatch this action), but if a stale action
                // target ever fires after a refresh, fall back to the
                // legacy custom color rather than panic.
                self.custom_colors
                    .get(n as usize)
                    .copied()
                    .flatten()
                    .unwrap_or(self.custom_color)
            }
        }
    }
}

/// Number of saved-custom slots per popover column. Matches the
/// palette column's 10 swatches so saved customs visually line up
/// with palette colors row-for-row. Once the user saves more than
/// this many, a second column appears — matches typical "fill
/// then wrap" behavior. The bottom row (row 10) of each column is
/// reserved: the left column for the color-wheel button, the right
/// column(s) for the expand-arrow (last column only).
const SLOTS_PER_COLUMN: usize = 10;

/// Duration of the crossfade between successive popover-grid layouts
/// while reorder drag-and-drop is in flight. Short enough that a fast
/// hover still feels responsive (the new layout commits within ~1.5
/// frames at 60 fps after `STACK_FADE_MS`) but long enough that the
/// shift reads as motion rather than a snap.
const STACK_FADE_MS: u32 = 120;

/// Inline color-picker panel sizing. Width is whatever fits the
/// chooser's natural row layout (eyedropper + preview + hex entry);
/// height pinned so the chooser doesn't grow taller than the
/// swatches column it sits next to. The panel itself carries no
/// outer padding — the popover's CSS `padding: 14px` is the single
/// source of outer breathing room.
const PICKER_CHOOSER_WIDTH: i32 = 330;
const PICKER_CHOOSER_HEIGHT: i32 = 250;
/// Side-by-side sat/val + hue layout — `colorplane` width chosen so
/// the pair fills `PICKER_CHOOSER_WIDTH` exactly (`COLORPLANE_WIDTH` +
/// hue_width(20) + internal spacing(~6) ≈ chooser_width). With this
/// match, the sat/val square's right edge lines up with the
/// chooser's right edge, so the visible gap from sat/val to popover
/// edge is just the popover's CSS padding — no chooser-internal
/// dead space.
///
/// Saturation/value square — shrunk from the chooser's default 200+
/// natural so the gradient doesn't dominate. Hue scale on its left
/// is sized to match this height in `shrink_color_chooser_internals`.
const PICKER_COLORPLANE_WIDTH: i32 = 150;
const PICKER_COLORPLANE_HEIGHT: i32 = 100;

/// Handles returned by `build_color_popover` so the caller (`init`)
/// can stash everything it needs on the model — the popover itself,
/// the swatch-grid stack (for crossfade rebuilds), the inline picker's
/// revealer + chooser (for live color updates), and the bottom merged
/// button's caret icon (so the toggle handler can flip the caret's
/// direction without rebuilding the button).
struct ColorPopoverHandles {
    popover: gtk::Popover,
    swatch_stack: gtk::Stack,
    picker_revealer: gtk::Revealer,
    picker_chooser: gtk::ColorChooserWidget,
    caret_icon: gtk::Image,
}

/// Build the popover that hangs off the unified color-picker MenuButton.
/// Layout:
///
/// ```text
///   ┌── popover ───────────────────────────┬── revealer ──┐
///   │ ┌── swatch_stack ─┐                   │ inline       │
///   │ │ swatches grid   │                   │ picker       │
///   │ └─────────────────┘                   │ panel        │
///   │ ┌── controls box ─┐                   │ (chooser +   │
///   │ │ [wheel]   [⇄]   │                   │ + Add to     │
///   │ └─────────────────┘                   │ My Colors)   │
///   └──────────────────────────────────────┴──────────────┘
/// ```
///
/// The swatches grid is wrapped in a `Stack` so reorder drag-and-drop
/// can crossfade between layouts. The controls and inline picker live
/// outside the stack — they keep their state (and the chooser its
/// hue/saturation/value cursor) across saved-customs rebuilds.
fn build_color_popover(
    model: &ToolsToolbar,
    sender: &ComponentSender<ToolsToolbar>,
) -> ColorPopoverHandles {
    let popover = gtk::Popover::new();
    popover.add_css_class("color-picker-popover");
    popover.set_position(gtk::PositionType::Bottom);
    popover.set_has_arrow(true);

    // Outer: horizontal box. Left = swatches+controls column. Right
    // = stack holding the chooser. No `spacing` — the gutter
    // between the columns is on `left.margin_end` instead, because
    // `Box.spacing` only applies between *visible* children: a
    // popover refresh that briefly toggles `picker_revealer`'s
    // visibility (or a collapse → re-expand round trip) drops the
    // gap and the saved-customs `:checked` rings get re-clipped.
    let outer = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(0)
        .hexpand(false)
        .vexpand(false)
        // Center within the popover's contents node so any
        // intrinsic min-width from GTK's theme defaults distributes
        // evenly on both sides — the user perceives the same gap
        // on the left and right of the swatches column.
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    outer.add_css_class("color-picker-content");
    outer.set_size_request(0, 0);
    outer.set_overflow(gtk::Overflow::Visible);

    // Left column. Vertical: swatch_stack + controls + add-revealer.
    // Uniform spacing between sections so the column reads as a tidy
    // stack instead of three siblings with mismatched margins.
    let left = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        // 7 px matches the grid's `row_spacing`, so the gap between
        // the swatches grid and the controls row reads as another
        // "row gap" of the same palette grid.
        .spacing(7)
        .hexpand(false)
        .vexpand(false)
        .build();
    left.add_css_class("color-picker-left");
    left.set_size_request(0, 0);
    // Allow the saved-customs `:checked` outline (which sits OUTSIDE
    // the swatch widget's bounds) to paint outside LEFT col without
    // being clipped at the box's right edge. GTK4 widgets default to
    // `Overflow::Hidden`, which clips descendants to the widget's
    // allocation; switching to `Visible` lets the ring paint into
    // the popover's content area beyond LEFT col. We do this on
    // every ancestor between the swatch and the popover so the
    // chain of clip regions doesn't trim the ring at any level
    // (especially important after `refresh_color_popover` rebuilds
    // the grid — the new grid's clip is freshly tight around its
    // content, and without overflow-visible the ring gets re-cut).
    left.set_overflow(gtk::Overflow::Visible);

    let swatch_stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(STACK_FADE_MS)
        .hhomogeneous(true)
        .vhomogeneous(true)
        .build();
    swatch_stack.add_css_class("swatches-area");
    swatch_stack.set_overflow(gtk::Overflow::Visible);
    let grid = build_color_popover_grid(model, sender, &popover);
    swatch_stack.add_named(&grid, Some("page-0"));
    swatch_stack.set_visible_child(&grid);
    left.append(&swatch_stack);

    // Bottom controls: one merged button that stretches the full
    // width of the swatch grid above it. The paint-bucket icon sits
    // flush-left so it lands centered under the first (palette)
    // column; the caret sits flush-right so it lands centered under
    // the rightmost saved-customs column — whichever column count
    // the grid currently shows. One button instead of two reads as
    // one action ("open the mixer"); the caret still flips
    // pan-end ↔ pan-start on expand/collapse as a direction cue.
    let controls = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .hexpand(true)
        .halign(gtk::Align::Fill)
        .build();
    controls.add_css_class("color-picker-controls");

    let merged_inner = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .hexpand(true)
        .halign(gtk::Align::Fill)
        .build();
    let paint_icon = gtk::Image::builder()
        .icon_name("color-regular")
        .width_request(SWATCH_DISPLAY_SIZE)
        .halign(gtk::Align::Start)
        .build();
    // Hexpand spacer between the two icons absorbs all the slack so
    // the icons stay pinned to the left/right edges of the button
    // (= the centers of the leftmost/rightmost grid columns). Doing
    // this with a dedicated spacer Box is more reliable than relying
    // on each icon's `halign` to "drift apart" inside a parent Box —
    // GTK's box-layout distributes extra width to hexpand children,
    // and only the spacer is hexpand here.
    let spacer = gtk::Box::builder().hexpand(true).build();
    let caret_icon = gtk::Image::builder()
        .icon_name(if model.picker_expanded {
            "pan-start-symbolic"
        } else {
            "pan-end-symbolic"
        })
        .width_request(SWATCH_DISPLAY_SIZE)
        .halign(gtk::Align::End)
        .build();
    merged_inner.append(&paint_icon);
    merged_inner.append(&spacer);
    merged_inner.append(&caret_icon);

    let merged_btn = gtk::Button::builder()
        .focusable(false)
        .focus_on_click(false)
        .hexpand(true)
        .vexpand(false)
        .height_request(SWATCH_DISPLAY_SIZE)
        .halign(gtk::Align::Fill)
        .valign(gtk::Align::Center)
        .child(&merged_inner)
        .build();
    merged_btn.add_css_class("flat");
    merged_btn.add_css_class("picker-expand-button");
    attach_floating_swatch_tooltip(&merged_btn, "Open color picker");
    let sender_for_merged = sender.clone();
    merged_btn.connect_clicked(move |_| {
        sender_for_merged.input(ToolsToolbarInput::TogglePickerExpansion);
    });

    controls.append(&merged_btn);
    left.append(&controls);

    // Right side: stack holding the inline picker panel. The
    // chooser inside is built once and kept alive — its in-progress
    // state (saturation/value cursor, hex entry text) needs to
    // survive across saved-customs rebuilds. The panel itself also
    // owns the "+ Add to My Colors" button now (used to live in the
    // left column with a separate revealer + a pair of vexpand
    // spacers around the controls to keep them centered when the
    // chooser pushed the left column taller). Putting the Add
    // button inside the same panel means the controls on the left
    // can sit at their natural position (right below the swatch
    // grid) regardless of whether the chooser is shown — no more
    // controls drifting up/down on toggle.
    let (picker_revealer, picker_chooser) = build_inline_picker_panel(model, sender);

    // When the user triggers GTK's screen eyedropper, pop the
    // picker down for the duration of the pick — it otherwise
    // floats on top of the very screen content the user is trying
    // to sample. The popover comes back automatically once the
    // picked color arrives (the `InlinePickerColorChanged` handler
    // re-opens it through the MenuButton).
    if let Some(eyedropper) = find_eyedropper_button(&picker_chooser) {
        let popover_for_eyedropper = popover.clone();
        eyedropper.connect_clicked(move |_| {
            popover_for_eyedropper.popdown();
        });
    }

    // 4-px transparent gutters on BOTH sides of LEFT col. The right
    // gutter sits between LEFT col and `picker_revealer` so the
    // rightmost saved-customs column's `:checked` box-shadow (which
    // extends 2 logical px past the swatch widget's right edge) has
    // room to render against the popover's bg without being painted
    // over by `picker_revealer`. The left gutter mirrors it so the
    // popover's visible side padding stays symmetric (20 px CSS
    // padding + 4 px gutter = 24 px on each side). Always-on — the
    // cost of a reliable ring that doesn't depend on visibility-
    // conditional layout that GTK can drop on a refresh.
    let outline_gutter_left = gtk::Box::builder().width_request(4).build();
    let outline_gutter_right = gtk::Box::builder().width_request(4).build();
    outer.append(&outline_gutter_left);
    outer.append(&left);
    outer.append(&outline_gutter_right);
    outer.append(&picker_revealer);

    popover.set_child(Some(&outer));

    // The "rebuild on open" refresh is wired up on the parent
    // MenuButton's `notify::active` in `init`, not on the popover's
    // `connect_show` — that handler fires AFTER GTK has already mapped
    // the popover surface and asked Wayland to position it, so a
    // refresh from there mutates the stack mid-fade and the Wayland
    // surface gets reconfigured to a different size/position one
    // frame later (the "glitch render" flash). Running the refresh
    // from `notify::active` lets the rebuild settle before the
    // surface maps so the popover's first measurement is final.

    // Keyboard delete on the popover: Backspace / Delete drops the
    // currently-selected saved-custom color (if any). Bubble phase so
    // the hex / RGB entries inside the chooser get first crack at the
    // keystroke — those consume Backspace at target phase, so this
    // handler only fires when focus is OUTSIDE an entry (i.e., the
    // user just clicked a swatch). Stays open after deletion so the
    // user can keep editing the palette.
    let key_controller = gtk::EventControllerKey::builder()
        .propagation_phase(gtk::PropagationPhase::Bubble)
        .build();
    let sender_for_key = sender.clone();
    key_controller.connect_key_pressed(move |_c, keyval, _kc, _mods| {
        use relm4::gtk::gdk::Key;
        if matches!(keyval, Key::BackSpace | Key::Delete | Key::KP_Delete) {
            sender_for_key.input(ToolsToolbarInput::DeleteCurrentSavedColor);
            relm4::gtk::glib::Propagation::Stop
        } else {
            relm4::gtk::glib::Propagation::Proceed
        }
    });
    popover.add_controller(key_controller);

    // Separate controller on the Capture phase so we see Escape
    // *before* GTK's default popover-close handler — the
    // `EscapePressed` handler can then cancel a drag (keeping the
    // popover open) or pop down the popover when no drag is
    // in flight. Without Capture, GTK's autohide closes the popover
    // first and our drag-cancel path never runs.
    let esc_controller = gtk::EventControllerKey::builder()
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    let sender_for_esc = sender.clone();
    esc_controller.connect_key_pressed(move |_c, keyval, _kc, _mods| {
        use relm4::gtk::gdk::Key;
        if matches!(keyval, Key::Escape) {
            sender_for_esc.input(ToolsToolbarInput::EscapePressed);
            relm4::gtk::glib::Propagation::Stop
        } else {
            relm4::gtk::glib::Propagation::Proceed
        }
    });
    popover.add_controller(esc_controller);

    ColorPopoverHandles {
        popover,
        swatch_stack,
        picker_revealer,
        picker_chooser,
        caret_icon,
    }
}

/// Walk the chooser's widget tree and resize the leaf nodes that
/// drive its overall height. The default `colorplane` requests a
/// 200+ pixel natural size, and the adjacent vertical `colorscale`
/// (hue) and horizontal `colorscale` (alpha) inherit `vexpand=TRUE`
/// from `GtkColorEditor`, so CSS `min-*` can't shrink them — we have
/// to call `set_size_request` + `set_vexpand(false)` on each.
///
/// Idempotent: every internal `colorplane` is shrunk to a fixed
/// width/height; the hue scale gets the same height (so it lines up
/// alongside the plane); the alpha scale gets a horizontal slim
/// dimension. Anything else in the tree is left untouched.
fn shrink_color_chooser_internals(chooser: &gtk::ColorChooserWidget) {
    let mut stack: Vec<gtk::Widget> = Vec::new();
    let root: gtk::Widget = chooser.clone().upcast();
    let mut child = root.first_child();
    while let Some(c) = child {
        stack.push(c.clone());
        child = c.next_sibling();
    }
    while let Some(w) = stack.pop() {
        let name = w.css_name();
        match name.as_str() {
            "colorplane" => {
                // Keep the colorplane's default `hexpand` so it
                // grows to fill whatever horizontal space the
                // chooser allocates — that way the sat/val
                // gradient reaches the chooser's right edge with
                // no dead band.
                w.set_size_request(PICKER_COLORPLANE_WIDTH, PICKER_COLORPLANE_HEIGHT);
                w.set_vexpand(false);
            }
            "colorscale" => {
                if w.has_css_class("opacity") {
                    // Alpha — horizontal slim slider. `hexpand`
                    // stays at default so the alpha bar stretches
                    // across the chooser width like the sat/val
                    // gradient above.
                    w.set_size_request(-1, 14);
                    w.set_vexpand(false);
                } else {
                    // Hue — vertical, height matches the plane.
                    w.set_size_request(20, PICKER_COLORPLANE_HEIGHT);
                    w.set_vexpand(false);
                }
            }
            _ => {
                // Every other descendant — top-row preview block,
                // hex entry, internal Boxes — defaults to
                // `hexpand: true`, which makes the chooser balloon
                // wider than the sat/val + hue stack needs.
                // Forcing `hexpand: false` keeps the chooser's
                // natural width to just the colorplane + hue
                // column, so the sat/val ends flush with the
                // chooser's right edge.
                w.set_hexpand(false);
            }
        }
        let mut grand = w.first_child();
        while let Some(g) = grand {
            stack.push(g.clone());
            grand = g.next_sibling();
        }
    }
}

/// Find GTK's screen-eyedropper button inside the color chooser's
/// editor — the `GtkButton` carrying the `color-select-symbolic`
/// icon (tooltip "Pick a color from the screen"). The chooser is a
/// stock `GtkColorChooserWidget`, so this leans on a GTK-internal
/// widget: if a future GTK reworks the editor and the button can't
/// be found, callers degrade gracefully (the popover simply won't
/// auto-close while the eyedropper runs).
fn find_eyedropper_button(chooser: &gtk::ColorChooserWidget) -> Option<gtk::Button> {
    let mut stack: Vec<gtk::Widget> = vec![chooser.clone().upcast()];
    while let Some(w) = stack.pop() {
        if let Some(btn) = w.downcast_ref::<gtk::Button>()
            && btn.icon_name().as_deref() == Some("color-select-symbolic")
        {
            return Some(btn.clone());
        }
        let mut c = w.first_child();
        while let Some(child) = c {
            stack.push(child.clone());
            c = child.next_sibling();
        }
    }
    None
}

/// Build the inline color-picker panel — chooser + "+ Add" button
/// inside a panel Box, wrapped in a Revealer that uses SlideRight
/// transition. SlideRight is the only Revealer transition that
/// actually scales the child's *horizontal* allocation along with
/// the animation — Crossfade and None leave the child allocated at
/// its full natural width (just hidden via alpha or unmap), which
/// blows the collapsed popover up to chooser-width. The remaining
/// few px of slack from SlideRight at rest are masked by also
/// calling `set_visible(false)` on the revealer once the conceal
/// animation finishes (see the visibility toggle below).
///
/// The Add button sits BELOW the chooser (separated by a vexpand
/// spacer) so it aligns with the controls row at the bottom of the
/// swatches column on the left — wherever the popover's outer
/// height settles, the spacer absorbs the slack and the Add button
/// hugs the bottom of the panel.
///
/// Returns the revealer + the chooser. The Add button doesn't need
/// to be returned because its click handler is wired up inside this
/// function and there's no callsite-level state to thread through.
fn build_inline_picker_panel(
    model: &ToolsToolbar,
    sender: &ComponentSender<ToolsToolbar>,
) -> (gtk::Revealer, gtk::ColorChooserWidget) {
    let revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideRight)
        .transition_duration(220)
        .reveal_child(model.picker_expanded)
        .build();
    revealer.add_css_class("inline-picker-revealer");
    revealer.set_overflow(gtk::Overflow::Visible);
    // Hide the revealer entirely while collapsed so it consumes no
    // horizontal space at rest. Even with SlideRight a few logical
    // px of slack would otherwise survive on the right edge of the
    // collapsed popover. The TogglePickerExpansion handler flips
    // visibility to `true` before starting the reveal animation;
    // here we flip it back to `false` once the conceal animation
    // has fully run (so the animation plays before we hide).
    revealer.set_visible(model.picker_expanded);
    revealer.connect_child_revealed_notify(|rev| {
        if !rev.is_child_revealed() {
            rev.set_visible(false);
        }
    });

    // Panel wraps the chooser + Add button vertically. The 14 px
    // left/right margins match the swatches' column_spacing — the
    // chooser reads as "one column over" from the saved-customs
    // column. `valign: Fill` lets the panel stretch to the outer
    // Box's height, so the internal vexpand spacer between chooser
    // and Add button can push the Add button to the bottom of the
    // panel (aligning it with the bottom of the swatches column on
    // the left).
    let panel = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        // 11 px matches the visible gap above the alpha slider
        // (chooser-internal row spacing), so the gap above the Add
        // button mirrors the gap above the slider on the other
        // side. Old value was 6 which read as cramped.
        .spacing(11)
        .margin_start(14)
        .margin_end(14)
        .margin_top(0)
        .margin_bottom(0)
        .valign(gtk::Align::Fill)
        .build();
    panel.add_css_class("inline-picker-panel");
    panel.set_overflow(gtk::Overflow::Visible);

    let chooser = gtk::ColorChooserWidget::new();
    chooser.set_use_alpha(true);
    chooser.set_rgba(&RGBA::from(model.current_color));
    // Skip the palette grid built into ColorChooserWidget — the
    // popover's left column already serves that role. The editor
    // (saturation/value, hue, alpha, hex/RGB) is the new value-add.
    chooser.set_show_editor(true);
    chooser.set_hexpand(false);
    chooser.set_vexpand(false);
    chooser.set_halign(gtk::Align::Fill);
    chooser.set_valign(gtk::Align::Start);
    // The GtkColorChooserWidget's internal layout requests a tall
    // natural size (saturation/value square + vertical hue scale +
    // alpha row + hex entry stack). CSS `min-*` only sets a floor,
    // so to clamp the TOTAL height we both (a) pin an explicit
    // size_request on the chooser as a whole and (b) walk the tree
    // and resize the colorplane / colorscale leaf nodes directly,
    // since those carry vexpand=TRUE and would otherwise expand
    // back to fill the allocated space.
    chooser.set_size_request(PICKER_CHOOSER_WIDTH, PICKER_CHOOSER_HEIGHT);
    shrink_color_chooser_internals(&chooser);

    // Broadcast color changes live. The chooser fires `notify::rgba`
    // on every cursor movement — forward as `InlinePickerColorChanged`
    // so the active drawing color tracks what the user is mixing.
    let sender_for_chooser = sender.clone();
    chooser.connect_rgba_notify(move |c| {
        let color = Color::from_gdk(c.rgba());
        sender_for_chooser.input(ToolsToolbarInput::InlinePickerColorChanged(color));
    });

    panel.append(&chooser);

    // "+ Add to custom colors" — wrapped in an HBox with a leading
    // transparent spacer that pushes the button right to align with
    // the alpha slider's track. The slider doesn't sit flush with
    // the chooser's left edge — the chooser's internal Grid puts a
    // hue-scale column (20 logical wide) plus a column gap to the
    // left of the slider's column, AND there's apparently another
    // ~36 logical of offset between the panel widget and the
    // chooser's content (likely GTK theme padding on the chooser
    // node). The empirical net offset from the panel's left to the
    // slider's left is 76 logical px (`ALPHA_LEFT_OFFSET`); we
    // mirror it with a spacer Box. The button width (`254`) keeps
    // the button effective span at `PICKER_CHOOSER_WIDTH` so the
    // panel doesn't grow.
    //
    // 33 px tall mirrors the chooser's top color-preview chip so
    // the panel reads as a balanced chooser → preview/slider →
    // matching-height footer.
    //
    // No dynamic background — the swatches above already act as the
    // "current color" cue, and a footer button that recolors itself
    // every cursor move was visually noisy.
    const ALPHA_LEFT_OFFSET: i32 = 76;
    let button_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .build();
    let left_spacer = gtk::Box::builder().width_request(ALPHA_LEFT_OFFSET).build();
    button_row.append(&left_spacer);

    let add_btn = gtk::Button::with_label("+ Add to custom colors");
    add_btn.add_css_class("add-color-btn");
    add_btn.set_focusable(false);
    add_btn.set_focus_on_click(false);
    add_btn.set_hexpand(false);
    // GTK4's GtkButton has a ~11 px implicit horizontal inset between
    // its widget allocation and the visible button background (theme
    // border + internal padding that we can't fully suppress via
    // `.add-color-btn`). Request 301 to land a visible width of 290.
    add_btn.set_width_request(301);
    add_btn.set_height_request(33);
    let chooser_for_add = chooser.clone();
    let sender_for_add = sender.clone();
    add_btn.connect_clicked(move |_| {
        let color = Color::from_gdk(chooser_for_add.rgba());
        sender_for_add.input(ToolsToolbarInput::SaveCustomColor(color));
    });
    button_row.append(&add_btn);

    panel.append(&button_row);

    revealer.set_child(Some(&panel));
    (revealer, chooser)
}

thread_local! {
    /// One shared tooltip popover used for every swatch in the picker.
    /// Parented lazily to the top-level window (NOT to a widget inside
    /// the picker popover) so it lives in its own Wayland surface,
    /// outside the picker — sidestepping the deadlocks we hit with
    /// per-swatch install_tooltip popovers inside the picker.
    static FLOATING_SWATCH_TIP: RefCell<Option<(gtk::Popover, gtk::Label)>> =
        const { RefCell::new(None) };
}

fn ensure_floating_swatch_tip(near: &gtk::Widget) -> (gtk::Popover, gtk::Label) {
    FLOATING_SWATCH_TIP.with(|cell| {
        if let Some(pair) = cell.borrow().as_ref() {
            return pair.clone();
        }
        // Walk up to the top-level window and parent the shared
        // popover there. Any descendant widget shares the same root.
        let window = near
            .root()
            .expect("swatch widget should be parented before hover");
        let label = gtk::Label::builder()
            .margin_start(8)
            .margin_end(8)
            .margin_top(4)
            .margin_bottom(4)
            .build();
        let popover = gtk::Popover::builder()
            .child(&label)
            .has_arrow(false)
            .autohide(false)
            .position(gtk::PositionType::Top)
            .build();
        popover.add_css_class("custom-tooltip");
        popover.set_can_focus(false);
        popover.set_can_target(false);
        popover.set_offset(0, -6);
        popover.set_parent(&window);
        *cell.borrow_mut() = Some((popover.clone(), label.clone()));
        (popover, label)
    })
}

/// Attach a custom floating tooltip to a swatch inside the picker
/// Attach a secondary-button GestureClick to `target` that pops up a
/// small "Save as default" popover at the click point. The popover is
/// rebuilt per-press (so each instance is independent) and unparented
/// on close. `on_save` runs when the popover's button is clicked —
/// typically it emits a `ToolbarEvent` or `StyleToolbarInput` to drive
/// the actual persistence path.
///
/// `set_propagation_phase(Capture)` is intentional: bubble-phase
/// gestures lose secondary-button presses on `gtk::Button` because
/// the button's internal click controller absorbs them; capture
/// phase fires first and reliably picks up the press.
fn attach_save_default_popover<F>(target: &impl IsA<gtk::Widget>, on_save: F)
where
    F: Fn() + 'static + Clone,
{
    use relm4::gtk::gdk;
    let target_widget = target.clone().upcast::<gtk::Widget>();
    let right_click = gtk::GestureClick::new();
    right_click.set_button(gdk::BUTTON_SECONDARY);
    right_click.set_propagation_phase(gtk::PropagationPhase::Capture);
    right_click.connect_pressed(move |g, _n, x, y| {
        // Claim the gesture sequence so descendant widgets with their
        // own internal gestures (notably `gtk::Scale`, which grabs
        // every press through its slider handle) can't take over and
        // suppress this popover. Without the claim, right-click on a
        // GtkScale fires `connect_pressed` here briefly but the
        // popover never appears because the scale's internal gesture
        // group cancels ours mid-sequence.
        g.set_state(gtk::EventSequenceState::Claimed);
        let menu = gtk::Popover::builder()
            .has_arrow(false)
            .autohide(true)
            .build();
        menu.add_css_class("save-default-menu");
        let save = gtk::Button::with_label("Save as default");
        save.add_css_class("flat");
        save.set_focusable(false);
        save.set_focus_on_click(false);
        let menu_for_click = menu.clone();
        let on_save = on_save.clone();
        save.connect_clicked(move |_| {
            on_save();
            menu_for_click.popdown();
        });
        menu.set_child(Some(&save));
        menu.set_parent(&target_widget);
        menu.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        menu.connect_closed(|m| m.unparent());
        menu.popup();
    });
    target.add_controller(right_click);
}

/// popover. Uses ONE shared popover parented to the top-level window,
/// repositioned via `set_pointing_to` with the swatch's bounds in
/// window coordinates. Because the tooltip popover lives outside the
/// picker, the popover-in-popover deadlock doesn't apply.
fn attach_floating_swatch_tooltip(target: &impl IsA<gtk::Widget>, text: &str) {
    let target_widget = target.clone().upcast::<gtk::Widget>();
    let motion = gtk::EventControllerMotion::new();
    let text = text.to_string();
    let target_enter = target_widget.clone();

    // Delay the show by `TOOLTIP_DELAY` — re-arm on every enter,
    // cancel on leave. Keeps quick passes over the swatches from
    // flashing a tooltip the user never asked to see.
    let pending_show: Rc<RefCell<Option<gtk::glib::SourceId>>> = Rc::new(RefCell::new(None));

    {
        let pending_show = pending_show.clone();
        motion.connect_enter(move |_, _, _| {
            if let Some(id) = pending_show.borrow_mut().take() {
                id.remove();
            }
            let target = target_enter.clone();
            let text = text.clone();
            let pending_inner = pending_show.clone();
            let id = gtk::glib::timeout_add_local_once(TOOLTIP_DELAY, move || {
                pending_inner.borrow_mut().take();
                let Some(window) = target.root() else {
                    return;
                };
                let (popover, label) = ensure_floating_swatch_tip(&target);
                label.set_label(&text);
                if let Some(bounds) = target.compute_bounds(&window) {
                    let rect = gtk::gdk::Rectangle::new(
                        bounds.x() as i32,
                        bounds.y() as i32,
                        bounds.width() as i32,
                        bounds.height() as i32,
                    );
                    popover.set_pointing_to(Some(&rect));
                }
                popover.popup();
            });
            *pending_show.borrow_mut() = Some(id);
        });
    }
    {
        let pending_show = pending_show.clone();
        motion.connect_leave(move |_| {
            if let Some(id) = pending_show.borrow_mut().take() {
                id.remove();
            }
            if let Some(pair) = FLOATING_SWATCH_TIP.with(|c| c.borrow().clone()) {
                pair.0.popdown();
            }
        });
    }
    target_widget.add_controller(motion);
}

/// Build the grid that lives inside the picker popover. Separated from
/// `build_color_popover` so the contents can be regenerated when the
/// user appends a new saved custom color — see
/// `ToolsToolbar::rebuild_color_popover_grid`.
fn build_color_popover_grid(
    model: &ToolsToolbar,
    sender: &ComponentSender<ToolsToolbar>,
    popover: &gtk::Popover,
) -> gtk::Grid {
    // Color picker grid carries NO outer margins — the popover's
    // content padding (CSS `.color-picker-popover contents`,
    // 20 px) is the single source of truth for "distance from
    // popover edge to content," and the parent `left` Box's spacing
    // handles the gap between the grid and the controls row below.
    // `column_spacing` is 14 px to match the controls row below
    // (wheel + arrow buttons line up under their swatch columns).
    // `row_spacing` is half that, 7 px — the vertical gap doesn't
    // need to match the horizontal one, and a tighter row pitch keeps
    // the popover from running tall with 10 palette swatches stacked.
    let grid = gtk::Grid::builder()
        .row_spacing(7)
        .column_spacing(14)
        .build();
    // Don't clip swatches at the grid's bounds — the `:checked`
    // outline paints OUTSIDE the swatch widget, and the rightmost
    // saved-customs column sits flush against the grid's right edge.
    // See the matching `set_overflow(Visible)` on `left` /
    // `swatch_stack` in `build_color_popover`.
    grid.set_overflow(gtk::Overflow::Visible);

    // Per-swatch tooltips are attached via `attach_floating_swatch_tooltip`
    // below. See its docstring for why we use a custom shared popover
    // parented to the top-level window rather than GTK's tooltip system.

    // The user can hide the default palette via Preferences, in which
    // case the saved-customs grid slides over to start at column 0 and
    // *its* first column inherits the 1–9, 0 shortcut tooltips.
    let hide_palette = APP_CONFIG.read().hide_default_palette();

    // Left column: 10 palette swatches, one per row, with shortcut
    // keys 1..9, 0 mapped to indexes 0..9. Skipped entirely when
    // the user has hidden the default palette.
    if !hide_palette {
        for (i, &color) in APP_CONFIG
            .read()
            .color_palette()
            .palette()
            .iter()
            .enumerate()
            .take(10)
        {
            let btn = gtk::ToggleButton::builder()
                .focusable(false)
                .focus_on_click(false)
                .hexpand(false)
                .vexpand(false)
                // Pin the toggle button to the same SWATCH_DISPLAY_SIZE
                // bounds the dashed placeholders use. Without this the
                // button's natural size includes a few pixels of vertical
                // chrome that makes the `:checked` outline read as
                // asymmetric (thicker on the top/bottom than left/right).
                .width_request(SWATCH_DISPLAY_SIZE)
                .height_request(SWATCH_DISPLAY_SIZE)
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Center)
                .child(&create_icon(color))
                .build();
            btn.add_css_class("flat");
            btn.add_css_class("color-swatch");
            btn.set_action::<ColorAction>(ColorButtons::Palette(i as u64));
            // Dismiss the popover after the user picks a color —
            // matches the typical "swatch tap = commit + close"
            // expectation. The action fires the
            // `ColorButtonSelected` input alongside this click, so
            // by the time the popover is down the model has
            // already taken the pick.
            let popover_for_dismiss = popover.clone();
            btn.connect_clicked(move |_| {
                popover_for_dismiss.popdown();
            });
            let shortcut = if i < 9 {
                format!("{}", i + 1)
            } else {
                "0".to_string()
            };
            let name = color
                .name()
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("Color {}", i + 1));
            attach_floating_swatch_tooltip(&btn, &format!("{name} ({shortcut})"));
            grid.attach(&btn, 0, i as i32, 1, 1);
        }
    }

    // Right column(s): the sparse `custom_colors` slot list. Each
    // `Some(color)` renders as a filled swatch; each `None` as a
    // dashed placeholder. Trailing `None`s are trimmed eagerly
    // (every drag-state mutation, persistence write) so the user
    // sees a compact view — only the trailing "drop-to-grow" empty
    // slot shown during drag, no other padding.
    //
    // During a drag, the dragged color has already been pulled out
    // of its origin slot by `BeginCustomDrag` (replaced with `None`,
    // then trailing-trimmed if the origin sat at the end). The ghost
    // preview is rendered at `dragging_preview_slot`. Subsequent
    // slots only shift down by one if the preview slot is on a
    // *filled* swatch (the user is dropping ONTO an existing
    // color, which needs to make room). If the preview is over an
    // empty placeholder or past the end of the list, nothing
    // shifts — the ghost just appears at the position.
    let saved = &model.custom_colors;
    let dragging = model.dragging_color.is_some();
    let preview = model.dragging_preview_slot;
    // Shift only when the preview is sitting on a filled swatch.
    // Dropping on an empty slot just fills it; dropping past the
    // list extends the list — neither moves any other swatches.
    let target_filled = preview
        .and_then(|p| saved.get(p).copied().flatten())
        .is_some();
    let shift_active = dragging && target_filled;
    // If the shift can land on an existing `None` in the same
    // column past the target, the shift stops there — items past
    // that `None` don't move, and the `None` itself is consumed by
    // the shifted-in tail. Without an in-column gap, the shift
    // runs all the way to the column's end and the list grows.
    let absorb_slot = if shift_active {
        preview.and_then(|p| find_same_column_gap(saved, p))
    } else {
        None
    };
    // Total visible slots: show the currently-used columns rounded
    // up to a full column, plus an extra empty "next column" when:
    //
    //   - The chooser is expanded — drag can drop past the end of
    //     the list and grow it (growing requires the chooser
    //     anyway since new colors come from the inline picker).
    //
    //   - The default palette is hidden — the saved-customs are
    //     the picker's primary content, so we keep a balanced
    //     2-column layout going: 1 used + 1 empty spillover when
    //     the list is sparse, N used + 1 empty once a swatch is
    //     present in the previous spillover column. A "3rd column"
    //     never appears until the 2nd column gets its first
    //     swatch, etc.
    //
    // Building the baseline the same way for idle and drag prevents
    // the popover from resizing the moment a drag starts. During
    // drag the preview / shift extras may push the count further.
    let used_cols = saved.len().div_ceil(SLOTS_PER_COLUMN);
    let extra_col = if model.picker_expanded || hide_palette {
        1
    } else {
        0
    };
    let min_cols = if hide_palette { 2 } else { 1 };
    let base = (used_cols + extra_col).max(min_cols) * SLOTS_PER_COLUMN;
    let total_slots = if dragging {
        let shift_extra = if shift_active { 1 } else { 0 };
        let min_for_list = saved.len() + 1 + shift_extra;
        let min_for_preview = preview.map(|p| p + 1 + shift_extra).unwrap_or(0);
        base.max(min_for_list).max(min_for_preview)
    } else {
        base
    };
    for visual_slot in 0..total_slots {
        let col_idx = visual_slot / SLOTS_PER_COLUMN;
        let row_idx = visual_slot % SLOTS_PER_COLUMN;
        // Saved-customs sit in grid column 1 normally (column 0 is the
        // default palette). When the user hides the default palette,
        // saved-customs slide over to start at grid column 0.
        let grid_col = if hide_palette {
            col_idx as i32
        } else {
            (1 + col_idx) as i32
        };

        let (widget, tooltip) = if dragging && Some(visual_slot) == preview {
            (build_ghost_placeholder(), None)
        } else {
            // When the shift is active, slots strictly past the
            // preview look up the previous index in `saved` —
            // they've moved down one row to make room for the
            // ghost. But the shift stops at `absorb_slot` (a
            // mid-column `None` that absorbs the tail), so slots
            // past it map 1:1 to `saved` unchanged. When no
            // shift is active, every visual slot maps 1:1.
            let shifted = shift_active
                && matches!(preview, Some(p) if visual_slot > p)
                && match absorb_slot {
                    Some(absorb) => visual_slot <= absorb,
                    None => true,
                };
            let saved_idx = if shifted {
                visual_slot - 1
            } else {
                visual_slot
            };
            match saved.get(saved_idx).copied().flatten() {
                Some(color) => {
                    let selected = color == model.current_color;
                    let w = build_saved_custom_swatch(color, saved_idx, selected, sender, popover);
                    // First-column swatches inherit the 1–9, 0 shortcut
                    // tooltip when the default palette is hidden — that
                    // column is the picker's primary set, and main.rs's
                    // `ColorSwitchShortcut` handler routes the number
                    // keys to those slots in that mode.
                    let t = if hide_palette && saved_idx < SLOTS_PER_COLUMN {
                        let shortcut = if saved_idx == 9 {
                            "0".to_string()
                        } else {
                            (saved_idx + 1).to_string()
                        };
                        let name = color
                            .name()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| format!("Color {}", saved_idx + 1));
                        format!("{name} ({shortcut})")
                    } else {
                        match color.name() {
                            Some(name) => format!("{name} (saved {})", saved_idx + 1),
                            None => format!("Saved color {}", saved_idx + 1),
                        }
                    };
                    (w, Some(t))
                }
                None => {
                    let is_selected = model.selected_empty_slot == Some(visual_slot);
                    (
                        build_dashed_placeholder(visual_slot, is_selected, sender),
                        None,
                    )
                }
            }
        };
        if let Some(t) = tooltip {
            attach_floating_swatch_tooltip(&widget, &t);
        }
        attach_reorder_drop_target(&widget, visual_slot, sender);
        grid.attach(&widget, grid_col, row_idx as i32, 1, 1);
    }

    // The color-wheel + expand-arrow live in the controls box BELOW
    // the swatches grid (built once in `build_color_popover`). They're
    // not attached here so they keep their state across rebuilds.
    let _ = popover;

    grid
}

pub struct StyleToolbar {
    visible: bool,
    /// Tracks the currently-active tool so tool-specific controls (e.g. the
    /// arrow-style dropdown) can show/hide reactively.
    current_tool: Tools,
    /// Currently-selected size step, mirrored locally so the size
    /// slider's value can stay in sync via `#[watch]`. Replaces the
    /// 6-button radio bank's `RelmAction` state.
    current_size: Size,
    /// Spotlight overlay darkness (0.10–0.90). Persisted across launches
    /// via state.rs; restored here on init.
    spotlight_darkness: f32,
    /// Highlighter stroke opacity (0.10–1.00). Persisted likewise.
    highlighter_opacity: f32,
    /// Brush post-stroke smoothing iterations (Chaikin passes,
    /// 0–4). Snaps back to the saved default on re-entering the
    /// brush tool; right-click → "Save as default" persists.
    brush_post_smooth_iterations: usize,
    /// True while a multi-selection has a mixed `size` across its
    /// drawables — disables the size slider so the user can't
    /// accidentally collapse the group to a single value. Driven by
    /// `SyncMultiAgreement` (and reset on single-select / empty).
    size_slider_disabled: bool,
    /// True while there is an active selection (single or multi).
    /// Read by the `ToolChanged` handler so its "snap slider to the
    /// new tool's saved default" auto-emit only fires when there is
    /// no selection — otherwise the auto-emit cascades through
    /// `dispatch_style_change` and rewrites the just-selected
    /// drawable's size back to the tool default. Set from
    /// `SyncFromSelection` / `SyncMultiAgreement` (true) and
    /// `SyncToToolDefault` (false).
    has_selection: bool,
    /// Tooltip label stashed from `install_dynamic_tooltip` so the
    /// SelectionStyleChanged / SyncMultiAgreement / SyncToToolDefault
    /// handlers can flip its text between the with-selection and
    /// without-selection variants. The two variants point the user
    /// at different keystrokes (plain wheel vs Ctrl+wheel).
    size_tooltip_label: Option<gtk::Label>,
    /// Per-tool in-session size memory used when the user has the
    /// `sticky_session_defaults` preference enabled. Records the last
    /// size the slider showed for each tool during this session;
    /// `ToolChanged` / `SyncToToolDefault` consult this map (falling
    /// back to `state::load_size_for_tool` if the tool hasn't been
    /// touched yet) instead of always re-loading the saved default.
    /// Lives only in memory — a fresh launch starts empty, which is
    /// exactly the "reset on each load" semantics the preference
    /// promises.
    session_size_per_tool: HashMap<Tools, Size>,
    /// True while a multi-selection of brush strokes has a mixed
    /// `smooth_level` — disables the smoothness slider so a stray
    /// drag can't collapse them to a single value. Stays false (and
    /// the slider remains usable) when a multi-select agrees on its
    /// level. When the selection contains any non-brush drawable
    /// the smoothness slider hides entirely — that's
    /// `brush_smooth_slider_show_for_multi` below, not this flag.
    brush_smooth_slider_disabled: bool,
    /// True while a multi-selection is made entirely of brush strokes
    /// (regardless of whether their levels agree). Forces the
    /// smoothness slider visible even when `current_tool != Brush`
    /// — typical Pointer-tool multi-edit flow. Reset to false on
    /// single-select / empty-select so the slider falls back to its
    /// usual "visible iff current_tool == Brush" rule.
    brush_smooth_slider_show_for_multi: bool,
    /// Clone of the size slider widget — held in the model so the
    /// handlers below can imperatively refresh its mark labels
    /// (bolding the letter that matches the current tool's saved
    /// default). The marks are static after construction; relm4's
    /// `#[watch]` only updates declarative properties, so we do
    /// `clear_marks` + `add_mark` calls by hand on tool change /
    /// SaveSizeAsDefault.
    size_slider: Option<gtk::Scale>,
    /// Clone of the brush smoothness slider — held so the handlers
    /// below can imperatively re-position its single tick mark so the
    /// mark always points at the current saved default
    /// (`state::brush_post_smooth_iterations`). Without this the mark
    /// is stuck at the position it was given at construction time,
    /// even after the user persists a new default.
    brush_smooth_slider: Option<gtk::Scale>,
    /// True iff a crop region currently exists (in either edit or
    /// committed state). Drives the "Revert to Original" button's
    /// visibility — pushed via `CropPresenceChanged` from sketch_board.
    has_crop: bool,
    /// Current fill state — true means "fill shapes", false means
    /// "outline only". Mirrored locally so the Fill Shape button's
    /// icon and tooltip can update via `#[watch]`.
    fill_shapes: bool,
    /// Inner `Label` of the Fill button's custom-tooltip popover,
    /// captured in init() after `install_dynamic_tooltip` so the
    /// `ToggleFill` handler can refresh the wording every time the
    /// state flips (filled ↔ outline). Built lazily so a Fill button
    /// that never appears doesn't pay the popover cost.
    fill_tooltip_label: Option<gtk::Label>,
    /// Currently-selected blur algorithm. Mirrored locally so the
    /// MenuButton's leading icon can refresh via `#[watch]`. Sourced
    /// from `state.toml` on init and updated from the popover after.
    blur_style: BlurStyle,
    /// Popover hanging off the blur-style MenuButton — stashed so each
    /// row's click handler can `popdown()` after dispatch.
    blur_style_popover: Option<gtk::Popover>,
    /// Currently-selected arrow geometry. Same role as `blur_style`
    /// for the arrow MenuButton's leading icon.
    arrow_style: ArrowStyle,
    arrow_style_popover: Option<gtk::Popover>,
    /// DrawingArea rendering the live preview on the arrow MenuButton.
    /// Stashed so `SetArrowStyle` can `queue_draw()` after flipping the
    /// preview cell.
    arrow_preview_area: Option<gtk::DrawingArea>,
    /// Shared cell driving the MenuButton preview — its draw_func reads
    /// this. Mutated in `SetArrowStyle` to switch which variant the chip
    /// shows.
    arrow_preview_cell: Option<Rc<std::cell::Cell<ArrowStyle>>>,
    /// Inner Label of the arrow MenuButton's custom-tooltip popover so
    /// `SetArrowStyle` can refresh the wording to name the active style.
    arrow_style_tooltip_label: Option<gtk::Label>,
    /// Inner Label of the blur MenuButton's tooltip — same role as the
    /// arrow one. Updated on `SetBlurStyle`.
    blur_style_tooltip_label: Option<gtk::Label>,
    /// Text-background DropDown widget, stashed so `SetTextBackground`
    /// can flip its `selected` index when sketch_board cycles the
    /// variant via the double-tap shortcut (the view! macro only sets
    /// the initial value in `init`).
    text_background_dropdown: Option<gtk::DropDown>,
    /// Flag set by the `SetTextBackground { emit_upstream: false }`
    /// handler around its programmatic `set_selected` call so the
    /// dropdown's `connect_selected_notify` skips its upstream emit.
    /// Without this, syncing the dropdown to a freshly-selected
    /// drawable's background would re-fire `TextBackgroundSelected`
    /// and toast + re-apply pointlessly. `Rc<Cell<bool>>` because the notify
    /// closure captures it independently from the model.
    text_background_silent: std::rc::Rc<std::cell::Cell<bool>>,
    /// Currently-selected highlighter style — drives the
    /// highlighter MenuButton's tooltip wording and is the seed for
    /// the dropdown's "active" indicator.
    highlighter_style: crate::tools::HighlighterStyle,
    highlighter_style_popover: Option<gtk::Popover>,
    /// Inner Label of the highlighter MenuButton's tooltip — same
    /// role as the arrow / blur ones. Updated on `SetHighlighterStyle`.
    highlighter_style_tooltip_label: Option<gtk::Label>,
}

/// Icon name shown on the blur-style MenuButton and on each popover
/// row. Single source of truth so the chip and the menu can't drift.
fn blur_style_icon(s: BlurStyle) -> &'static str {
    match s {
        BlurStyle::Pixelate => "tetris-app-regular",
        BlurStyle::SecureBlur => "shield-lock-regular",
        BlurStyle::Gaussian => "drop-regular",
        BlurStyle::BlackOut => "weather-moon-regular",
    }
}

fn blur_style_label(s: BlurStyle) -> &'static str {
    match s {
        BlurStyle::Pixelate => "Pixelate",
        BlurStyle::SecureBlur => "Blur (secure)",
        BlurStyle::Gaussian => "Blur (smooth)",
        BlurStyle::BlackOut => "Black Out",
    }
}

/// Icon name shown on the highlighter-style MenuButton chip and the
/// popover rows. Both icons live in the bundled set (see
/// `icons.toml`).
fn highlighter_style_icon(s: crate::tools::HighlighterStyle) -> &'static str {
    use crate::tools::HighlighterStyle::*;
    match s {
        // Text-locked = "smart" highlighter that snaps to text rows.
        // The i-beam glyph (vertical stem with serifs top and bottom)
        // mirrors the thick i-beam cursor the tool puts on screen
        // when this mode is active, so the chip and the cursor speak
        // the same visual language.
        TextLocked => "text-regular",
        // Normal = freeform highlighter. The marker/highlight glyph
        // is the canonical highlighter affordance.
        Normal => "highlight-regular",
    }
}

fn highlighter_style_label(s: crate::tools::HighlighterStyle) -> &'static str {
    s.display_name()
}

fn arrow_style_label(s: ArrowStyle) -> &'static str {
    match s {
        ArrowStyle::Standard => "Standard",
        ArrowStyle::Pointy => "Pointy",
        ArrowStyle::Curved => "Curved",
        ArrowStyle::Double => "Double",
    }
}

/// Dimensions for the arrow-style preview. The chip in the bottom-
/// right cluster and the popover rows share a single longer width
/// — the short stubby chip was hard to distinguish between Standard
/// and Pointy at a glance, and the cluster has plenty of room to fit
/// the longer rendering. Height stays low so neither the chip nor
/// the rows tower over the rest of the toolbar.
const ARROW_PREVIEW_W: i32 = 60;
const ARROW_PREVIEW_H: i32 = 16;
const ARROW_ROW_PREVIEW_W: i32 = ARROW_PREVIEW_W;
const ARROW_ROW_PREVIEW_H: i32 = ARROW_PREVIEW_H;

/// Paint a small arrow preview into `ctx` using cairo, matching the shape
/// language of the actual ArrowStyle renderings in `tools::arrow`. The
/// constants here are tuned for legibility at preview size rather than
/// pulled from the per-size calibration tables — at ~30 × 16 px those
/// tables would either underflow (thin shafts disappear) or overflow.
pub fn draw_arrow_preview_cairo(
    ctx: &relm4::gtk::cairo::Context,
    style: ArrowStyle,
    width: f64,
    height: f64,
    rgba: (f64, f64, f64, f64),
) {
    use relm4::gtk::cairo;

    let pad_x = 1.5;
    let pad_y = 1.0;
    let start_x = pad_x;
    let end_x = width - pad_x;
    let mid_y = height * 0.5;
    let length = (end_x - start_x).max(1.0);
    let usable_h = (height - 2.0 * pad_y).max(1.0);

    ctx.save().ok();
    ctx.set_source_rgba(rgba.0, rgba.1, rgba.2, rgba.3);
    ctx.translate(start_x, mid_y);

    // Head / tip geometry is intentionally tied to HEIGHT (or fixed
    // ratios off it) rather than overall length, so widening the
    // preview (chip 30 px → row 60 px) extends the BODY of the arrow
    // without enlarging the head. Earlier sizing used `length * X`
    // which stretched the head linearly with the preview width and
    // made the wider previews look like a different glyph rather
    // than the same arrow with more shaft visible.
    match style {
        ArrowStyle::Standard => {
            // Solid filled head + tapered body, rounded outline overlay.
            // Head is roughly square (length ≈ height of usable area).
            let head_length = (usable_h * 0.85).min(length - 2.0);
            let head_half_h = head_length * 0.50;
            let stroke = (usable_h * 0.16).max(1.4);
            // body_max half-width before stroke widening; the rounded
            // stroke adds `stroke/2` on each side, so target visible
            // half-width is body_max_half + stroke/2.
            let body_max_half = (usable_h * 0.16).max(0.6);
            let head_outer_x = length - head_length;
            let head_inner_x = head_outer_x + head_length * 0.05;

            ctx.set_line_join(cairo::LineJoin::Round);
            ctx.set_line_cap(cairo::LineCap::Round);
            ctx.set_line_width(stroke);
            ctx.move_to(0.0, 0.0);
            ctx.line_to(head_inner_x, body_max_half);
            ctx.line_to(head_outer_x, head_half_h);
            ctx.line_to(length, 0.0);
            ctx.line_to(head_outer_x, -head_half_h);
            ctx.line_to(head_inner_x, -body_max_half);
            ctx.close_path();
            ctx.fill_preserve().ok();
            ctx.stroke().ok();
        }
        ArrowStyle::Pointy => {
            // Wider body, swept-back wing ears, flat-back tail. No stroke.
            // Head is a touch wider than tall; body fills the rest.
            let head_length = (usable_h * 1.05).min(length - 2.0);
            let head_half_h = head_length * 0.46;
            let body_max_half = (usable_h * 0.22).max(0.8);
            let back_half = body_max_half * 0.35;
            let wing_back_ratio = 0.22_f64;
            let wing_height_ratio = 0.22_f64;
            let head_outer_x = length - head_length;
            let wing_x = head_outer_x - head_length * wing_back_ratio;
            let wing_half_h = head_half_h * (1.0 + wing_height_ratio);

            ctx.set_line_join(cairo::LineJoin::Miter);
            ctx.move_to(0.0, back_half);
            ctx.line_to(head_outer_x, body_max_half);
            ctx.line_to(wing_x, wing_half_h);
            ctx.line_to(length, 0.0);
            ctx.line_to(wing_x, -wing_half_h);
            ctx.line_to(head_outer_x, -body_max_half);
            ctx.line_to(0.0, -back_half);
            ctx.close_path();
            ctx.fill().ok();
        }
        ArrowStyle::Curved | ArrowStyle::Double => {
            // Quadratic bezier shaft + open V tip(s). V-arm length
            // is tied to height so wider previews don't grow taller
            // tips. The curve amount is also clamped against height
            // so the bow stays inside the preview vertically.
            let shaft_width = (usable_h * 0.14).max(1.2);
            let head_side = (usable_h * 0.70).max(4.0);
            let half_angle = 45.0_f64.to_radians();
            // Arc upward — control point above the chord midpoint.
            // Scale with length so longer previews bow more, but
            // cap so we don't overflow the preview's vertical bounds.
            let curve_amount = (length * 0.18).min(usable_h * 0.55);
            let qx = length * 0.5;
            let qy = -curve_amount;

            ctx.set_line_width(shaft_width);
            ctx.set_line_cap(cairo::LineCap::Round);
            ctx.set_line_join(cairo::LineJoin::Round);

            // Quadratic → cubic conversion for cairo's curve_to.
            let c1x = 2.0 / 3.0 * qx;
            let c1y = 2.0 / 3.0 * qy;
            let c2x = length + 2.0 / 3.0 * (qx - length);
            let c2y = 2.0 / 3.0 * qy;
            ctx.move_to(0.0, 0.0);
            ctx.curve_to(c1x, c1y, c2x, c2y, length, 0.0);
            ctx.stroke().ok();

            let draw_v = |tip_x: f64, tip_y: f64, dx: f64, dy: f64| {
                let len = (dx * dx + dy * dy).sqrt();
                if len < 1e-6 {
                    return;
                }
                let angle = dy.atan2(dx);
                ctx.save().ok();
                ctx.translate(tip_x, tip_y);
                ctx.rotate(angle);
                let bx = -head_side * half_angle.cos();
                let by = head_side * half_angle.sin();
                ctx.move_to(bx, -by);
                ctx.line_to(0.0, 0.0);
                ctx.line_to(bx, by);
                ctx.stroke().ok();
                ctx.restore().ok();
            };

            // Tangent at end of quadratic is `end - control`.
            draw_v(length, 0.0, length - qx, -qy);
            if matches!(style, ArrowStyle::Double) {
                // Tangent at start is `start - control`.
                draw_v(0.0, 0.0, -qx, -qy);
            }
        }
    }

    ctx.restore().ok();
}

/// Build a DrawingArea that renders the given arrow style. The returned
/// `Rc<Cell<ArrowStyle>>` lets the caller change which variant is drawn
/// later — update the cell, then call `queue_draw()` on the returned area.
///
/// `width` / `height` are content-area dimensions; pass the chip
/// constants (`ARROW_PREVIEW_W` / `_H`) for the MenuButton's leading
/// preview, or the `ARROW_ROW_PREVIEW_*` pair for the wider popover-row
/// variant where the user wants to see more of the stroke.
fn make_arrow_preview(
    initial: ArrowStyle,
    width: i32,
    height: i32,
) -> (gtk::DrawingArea, Rc<std::cell::Cell<ArrowStyle>>) {
    let area = gtk::DrawingArea::new();
    area.set_content_width(width);
    area.set_content_height(height);
    area.set_valign(gtk::Align::Center);
    area.set_halign(gtk::Align::Center);
    let cell = Rc::new(std::cell::Cell::new(initial));
    let cell_for_draw = cell.clone();
    area.set_draw_func(move |area, ctx, w, h| {
        let style = cell_for_draw.get();
        // Mirror the way `gtk::Image` icons inherit the theme's
        // foreground color. `widget.color()` is GTK 4.10+ (we're on
        // 4.6 here), so we fall back to looking the named theme color
        // up through the (still-supported) style context.
        #[allow(deprecated)]
        let fg = area
            .style_context()
            .lookup_color("theme_fg_color")
            .unwrap_or_else(|| gtk::gdk::RGBA::new(0.9, 0.9, 0.9, 1.0));
        draw_arrow_preview_cairo(
            ctx,
            style,
            w as f64,
            h as f64,
            (
                fg.red() as f64,
                fg.green() as f64,
                fg.blue() as f64,
                fg.alpha() as f64,
            ),
        );
    });
    (area, cell)
}

/// Tooltip text for the arrow-style MenuButton — just the active
/// variant's name (the icon + the "Arrow" toolbar tool already make
/// the context clear, so the preamble was visual noise).
fn arrow_tooltip_text(s: ArrowStyle) -> String {
    arrow_style_label(s).to_string()
}

/// Tooltip text for the blur-style MenuButton — same shape as
/// `arrow_tooltip_text`, just the active algorithm's name.
fn blur_tooltip_text(s: BlurStyle) -> String {
    blur_style_label(s).to_string()
}

fn highlighter_tooltip_text(s: crate::tools::HighlighterStyle) -> String {
    highlighter_style_label(s).to_string()
}

/// Build a popover full of icon+label rows for an enum-style picker,
/// attach it to `menu`, and wire each row to dispatch the matching
/// `StyleToolbarInput`. Shared by the arrow and blur menus — they
/// differ only in the variant list and the icon/label/input mapping
/// functions, so factoring it out keeps the two pickers structurally
/// identical (they were drifting in the previous DropDown version).
fn build_style_popover<S, FW>(
    menu: &gtk::MenuButton,
    sender: &ComponentSender<StyleToolbar>,
    variants: &[S],
    widget_for: FW,
    label_for: fn(S) -> &'static str,
    to_input: fn(S) -> StyleToolbarInput,
) -> gtk::Popover
where
    S: Copy + 'static,
    FW: Fn(S) -> gtk::Widget,
{
    let popover = gtk::Popover::new();
    popover.add_css_class("compact-control-popover");
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    for &style in variants {
        let row = gtk::Button::new();
        row.add_css_class("flat");
        row.set_focus_on_click(false);
        let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let icon = widget_for(style);
        let label = gtk::Label::new(Some(label_for(style)));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        row_box.append(&icon);
        row_box.append(&label);
        row.set_child(Some(&row_box));
        let s = sender.clone();
        let popover_clone = popover.clone();
        row.connect_clicked(move |_| {
            s.input(to_input(style));
            popover_clone.popdown();
        });
        list.append(&row);
    }
    popover.set_child(Some(&list));
    menu.set_popover(Some(&popover));
    // Same MenuButton-with-focus_on_click(false) toggle workaround
    // as the color picker: a capture-phase click on the chip pops
    // the popover down if it's currently open, instead of letting
    // the autohide-then-re-popup chain blink it back open.
    install_menu_toggle_dismiss(menu, &popover);
    popover
}

/// Make a second click on `menu` dismiss `popover` cleanly when
/// the MenuButton has `focus_on_click: false`. Without this, the
/// autohide closes the popover but the same click re-opens it via
/// the menu-button gesture. The capture-phase click intercepts the
/// second press before either of those gestures runs and claims
/// the sequence if the popover is currently shown.
fn install_menu_toggle_dismiss(menu: &gtk::MenuButton, popover: &gtk::Popover) {
    let popover_for_click = popover.clone();
    let click = gtk::GestureClick::new();
    click.set_button(gtk::gdk::BUTTON_PRIMARY);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    click.connect_pressed(move |g, _, _, _| {
        if popover_for_click.is_visible() {
            popover_for_click.popdown();
            g.set_state(gtk::EventSequenceState::Claimed);
        }
    });
    menu.add_controller(click);
}

/// Tooltip wording for the Fill button — describes the *current* state
/// and what a click will do. Shared between init() (first install) and
/// the `ToggleFill` handler (refresh on toggle).
fn fill_tooltip_text(fill_shapes: bool) -> &'static str {
    if fill_shapes {
        "Currently filling shapes — click to switch to outline only (F)"
    } else {
        "Currently outlining shapes — click to switch to filled (F)"
    }
}

/// Size-slider tooltip text. The wheel-resize gesture's modifier
/// depends on whether anything is selected: with a selection, plain
/// wheel resizes it; without one, Ctrl+wheel changes the next-stroke
/// size. Reflecting that in the tooltip surfaces the right keystroke
/// for the user's current state.
fn size_tooltip_text(has_selection: bool) -> &'static str {
    if has_selection {
        "Annotation size — scroll on canvas to adjust"
    } else {
        "Annotation size — Ctrl+scroll on canvas to adjust"
    }
}

/// Map a `Size` to the size slider's integer position (0..=5). The
/// helper sits next to its inverse so the two stay in sync.
fn size_to_slider_value(size: Size) -> f64 {
    match size {
        Size::XSmall => 0.0,
        Size::Small => 1.0,
        Size::Medium => 2.0,
        Size::Large => 3.0,
        Size::XLarge => 4.0,
        Size::XXLarge => 5.0,
    }
}

fn slider_value_to_size(v: f64) -> Size {
    match v.round() as i32 {
        0 => Size::XSmall,
        1 => Size::Small,
        2 => Size::Medium,
        3 => Size::Large,
        4 => Size::XLarge,
        _ => Size::XXLarge,
    }
}

/// Display label for the right-side "tool-specific cluster" — empty
/// when the active tool has no dedicated control to show.
fn tool_cluster_label(tool: Tools) -> &'static str {
    match tool {
        Tools::Arrow => "Style",
        Tools::Blur => "Blur",
        Tools::Text => "Background",
        Tools::Spotlight => "Darkness",
        Tools::Highlighter => "Opacity",
        Tools::Brush => "Smoothing",
        Tools::Rectangle | Tools::Ellipse => "Fill Shape",
        _ => "",
    }
}

/// Width of the Spotlight darkness / Highlighter opacity sliders
/// inside the cluster. Narrower than they used to be — wide enough
/// to drag precisely, slim enough that they don't dominate the
/// cluster slot.
const CLUSTER_SLIDER_WIDTH: i32 = 100;

#[derive(Debug, Copy, Clone)]
pub enum ToolbarEvent {
    ToolSelected(Tools),
    ColorSelected(Color),
    SizeSelected(Size),
    ArrowStyleSelected(ArrowStyle),
    BlurStyleSelected(BlurStyle),
    HighlighterStyleSelected(crate::tools::HighlighterStyle),
    /// Crop tool's "Snap to edges" checkbox toggled. sketch_board
    /// forwards the value to `CropTool` and persists it to state.
    SnapToEdgesChanged(bool),
    Redo,
    Undo,
    SaveFile,
    CopyClipboard,
    ToggleFill,
    Reset,
    SaveFileAs,
    /// User clicked the gear button (or pressed Ctrl+,). Opens the
    /// preferences dialog where shortcut keys can be edited.
    OpenPreferences,
    /// A toolbar popover (e.g. the unified color picker) has closed; the
    /// canvas should grab keyboard focus back so single-key shortcuts
    /// (z, r, b, …) keep working without the user having to click first.
    FocusCanvas,
    /// User clicked the layers button (or pressed F7). Toggles the
    /// layer panel on the left edge of the canvas.
    ToggleLayerPanel,
    /// Spotlight overlay darkness (0.10–0.90) — global, applies to all
    /// committed and in-progress spotlights. Sketch_board pushes the
    /// value into the renderer for the next frame.
    SpotlightDarknessChanged(f32),
    /// User picked "Save as default" from the darkness slider's
    /// right-click menu — write the live value to state.toml.
    SaveSpotlightDarknessAsDefault,
    /// Highlighter stroke opacity (0.10–1.00) — applies only to
    /// future strokes; existing strokes keep their captured value.
    HighlighterOpacityChanged(f32),
    /// User picked "Save as default" from the opacity slider's
    /// right-click menu — write the live value to state.toml.
    SaveHighlighterOpacityAsDefault,
    /// Brush post-stroke smoothing iterations (0–4 Chaikin passes).
    /// Applies on the very next stroke; in-flight stroke isn't
    /// re-smoothed.
    BrushPostSmoothChanged(usize),
    /// User picked "Save as default" from the brush smoothing
    /// slider's right-click menu — write the carried value (read
    /// from the slider widget at click time) to state.toml and
    /// promote it to APP_CONFIG. The value is carried explicitly
    /// because the slider's position can diverge from APP_CONFIG
    /// when the user adjusts smoothness with a brush stroke
    /// selected (those edits don't update APP_CONFIG by design,
    /// so reading from APP_CONFIG would persist a stale value).
    SaveBrushPostSmoothAsDefault(usize),
    /// User picked "Save as default" from the fill button's
    /// right-click menu — persist the live fill state as the saved
    /// default for the current tool (Rectangle / Ellipse).
    SaveFillAsDefault,
    /// User clicked "Revert to Original" — drop the committed crop
    /// entirely so the canvas shows the full original image again.
    RevertCrop,
    /// User clicked "Cancel" on the crop-mode top toolbar — same
    /// behavior as Esc inside the Crop tool (drop uncommitted edit,
    /// restore the prior committed crop if any, exit Crop).
    CancelCrop,
    /// User clicked "Crop" on the crop-mode top toolbar — same
    /// behavior as Enter inside the Crop tool (apply the in-progress
    /// edit and exit Crop).
    ApplyCrop,
    /// User picked an aspect-ratio constraint from the crop-mode
    /// top toolbar's dropdown. Sketch_board forwards to
    /// `CropTool::set_aspect_ratio`, which both snaps the existing
    /// rect to the new ratio and enforces it on subsequent drags.
    CropAspectRatioChanged(crate::tools::AspectRatio),
    /// User entered explicit (width, height) values from the
    /// crop-mode W/H text inputs (or pressed the ↔ swap button).
    /// Sketch_board recenters the crop rect on the image at the
    /// requested dimensions via `CropTool::set_dimensions`.
    CropDimensionsSet {
        width: i32,
        height: i32,
    },
    /// User picked a background-color preset for the matte shown
    /// outside the crop region. Sketch_board forwards to
    /// `CropTool::set_bg_color`.
    CropBgColorChanged(crate::tools::CropBgColor),
    /// User clicked the flip-horizontal button in the crop-mode top
    /// toolbar. Mirrors the background image around its vertical
    /// axis; existing drawables stay at their image-space positions
    /// (documented limitation in `FemtoVGArea::flip_image_horizontal`).
    FlipHorizontal,
    /// User clicked the rotate button in the crop-mode top toolbar.
    /// Rotates the background image 90° counter-clockwise; the new
    /// image-bounds (width/height swapped) flow back to update the
    /// window size and reseed the crop rect.
    RotateImage,
    /// User confirmed "Resize" in the image-size popover. Resamples
    /// the background image to the target pixel dimensions; the new
    /// `(width, height)` flow back through `ContentSizeChanged` to
    /// resize the window and reseed the crop rect.
    ResizeImage {
        width: i32,
        height: i32,
    },
    /// User picked a different background style for new text
    /// drawables (Plain or Rounded). Sketch_board pushes through to
    /// the Text tool's `set_text_background`.
    TextBackgroundSelected(crate::tools::TextBackground),
}

/// Two responsive layouts the top toolbar can take, driven by a
/// width threshold + hysteresis from main.rs's window-resize
/// listener — never user-toggleable.
///
/// Below the wrap threshold the left + right clusters drop to a
/// second row (left flush-left, right flush-right) so every
/// control stays reachable even on tiny windows; previously the
/// transition went through an intermediate "hide just the left
/// cluster" state but that lost icons the user might still want.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum TopBarLayout {
    /// Wide windows. All three clusters horizontal, left/center/
    /// right at their CenterBox slots.
    Normal,
    /// Narrow windows. Center cluster keeps the top row to itself;
    /// left and right clusters drop to a second row (left
    /// flush-left, right flush-right). Trades vertical canvas
    /// height for keeping every control reachable.
    Wrap,
}

#[derive(Debug, Copy, Clone)]
pub enum ToolsToolbarInput {
    SetVisibility(bool),
    ToggleVisibility,
    SwitchSelectedTool(Tools),
    ColorButtonSelected(ColorButtons),
    /// The inline picker's chooser emitted a new RGBA — broadcast it
    /// as the current drawing color so the picked color is "live"
    /// (applies applying as the user drags).
    InlinePickerColorChanged(Color),
    /// Toggle the inline picker panel's revealer. Flips
    /// `picker_expanded`, animates the revealer, and updates the
    /// arrow button's icon (pan-end ↔ pan-start).
    TogglePickerExpansion,
    /// Append the given color to the user's persisted saved-custom
    /// palette, then refresh the popover so the new swatch shows up
    /// next to its dashed placeholder neighbors. Fired by the inline
    /// picker's "+ Add to My Colors" button.
    SaveCustomColor(Color),
    /// Drop the saved-custom color at the given index. Fired by the
    /// per-swatch right-click → "Delete" menu.
    DeleteCustomColor(usize),
    /// Drop the saved-custom color whose value matches the currently-
    /// selected color, if any. Fired by Backspace / Delete on the
    /// popover when focus isn't sitting in an entry — keyboard
    /// equivalent of the right-click → Delete menu, leaving the
    /// popover open so the user can keep editing their palette.
    DeleteCurrentSavedColor,
    /// A drag of a saved-custom swatch is starting at `slot`. The
    /// handler stashes both the color (so the live-reorder path can
    /// keep tracking it as the list mutates) and a snapshot of the
    /// pre-drag order so a cancel can revert.
    BeginCustomDrag(usize),
    /// While a drag is in flight, the pointer entered the drop area
    /// for `target_slot`. The handler relocates the dragged color to
    /// that slot in real time so the user sees a live preview instead
    /// of having to release to see the final order.
    LiveReorderCustomColor {
        target: usize,
    },
    /// Drag finished. `success = true` if the drop landed on a valid
    /// target (we persist the latest order); `false` means cancel
    /// (drop outside the popover, Esc, etc.) and the handler restores
    /// the pre-drag snapshot.
    EndCustomDrag {
        success: bool,
    },
    /// Escape was pressed while the popover had keyboard focus. If a
    /// drag is in flight, cancel the drag (restore the pre-drag list,
    /// keep the popover open). Otherwise close the popover.
    EscapePressed,
    /// Rebuild the color picker's swatch grid. Fired when the popover
    /// is shown so preference changes (palette visibility, etc.) take
    /// effect on the next open without needing a list mutation first.
    RefreshColorPopover,
    /// Crop tool emitted a new (width, height) for its current rect
    /// (drag tick, ratio snap, or explicit set). The handler updates
    /// `crop_width` / `crop_height` and refreshes the W/H entries
    /// unless they currently have focus (don't clobber typed input).
    CropDimensionsChanged {
        width: i32,
        height: i32,
    },
    /// User pressed Enter in the W entry (or `None` if the typed
    /// text didn't parse — we ignore it).
    CropWidthEntered(Option<i32>),
    CropHeightEntered(Option<i32>),
    /// User clicked the ↔ swap button between the W/H entries.
    /// Swaps the current dimensions and emits a fresh
    /// `CropDimensionsSet` so the crop rect resizes accordingly.
    CropDimensionsSwap,
    /// Background image dimensions changed (startup, rotate, or
    /// resize). The handler updates `image_width` / `image_height`
    /// so the MenuButton label refreshes via `#[watch]`, and
    /// pre-fills the resize popover's entries so it opens already
    /// populated next time.
    ImageDimensionsChanged {
        width: i32,
        height: i32,
    },
    /// User picked a crop-mode background-color preset from the
    /// swatch popover. Mirrors the choice into `crop_bg_color`
    /// (so the MenuButton's swatch image refreshes) and re-emits
    /// `ToolbarEvent::CropBgColorChanged` for the rest of the app.
    CropBgColorSelected(crate::tools::CropBgColor),
    /// Push the display DPR divisor from main.rs at startup so
    /// all user-facing pixel values (W/H entries, "Image size"
    /// label, resize-popover entries) render in LOGICAL pixels
    /// instead of raw image pixels.
    SetDisplayScale(i32),
    /// User clicked a dashed-empty placeholder in the saved-custom
    /// grid. Stash that slot index as `selected_empty_slot` so the
    /// next `SaveCustomColor` inserts at that visual position instead
    /// of appending. Clicking the same slot again clears the marker.
    SelectEmptySlot(usize),
    /// Clear `selected_empty_slot` without rebuilding the grid. Fired
    /// on popover close and on `ColorButtonSelected` so a stale empty
    /// slot doesn't survive across popover sessions.
    ClearEmptySlotSelection,
    /// Apply a responsive layout. Fired by main.rs from the
    /// window-resize listener whenever the width crosses a
    /// breakpoint. The handler is the single place that knows how
    /// to re-parent the right cluster between its three hosts;
    /// idempotent when the requested layout matches the current.
    SetLayout(TopBarLayout),
}

#[derive(Debug, Copy, Clone)]
pub enum StyleToolbarInput {
    SetVisibility(bool),
    ToggleVisibility,
    /// Right-click → "Save as default" on the size slider. Writes
    /// the current size as the saved default for the currently-active
    /// tool. Future tool-switches into that tool (and the next
    /// launch) start at this size.
    SaveSizeAsDefault,
    /// The renderer's selection went empty — pop the slider back to
    /// the active tool's saved default. Mirror image of
    /// `SyncFromSelection`, which loads the selected object's size.
    SyncToToolDefault,
    /// Sketch board changed the active tool's size externally
    /// (Shift+wheel over canvas) — mirror it into `current_size`
    /// without re-emitting `SizeSelected` (sketch_board already
    /// pushed the new size to the active tool).
    SetCurrentSize(crate::style::Size),
    /// The active drawing tool changed; tool-specific controls re-evaluate
    /// their visibility.
    ToolChanged(Tools),
    /// Crop is present (edit OR committed) — show/hide the
    /// "Revert to Original" button accordingly.
    CropPresenceChanged(bool),
    /// Size slider changed — update the model mirror and broadcast
    /// `SizeSelected` so sketch_board picks up the new size.
    SizeChanged(Size),
    /// Selection in sketch_board changed — push the selected
    /// drawable's style here so the size slider (and other style
    /// widgets) reflect the picked shape instead of the last value
    /// the user typed. Does NOT re-broadcast — applying the value
    /// back to the selection would loop forever.
    SyncFromSelection(crate::style::Style),
    /// Multi-select per-property agreement report. For each property,
    /// `Some(v)` says "all selected drawables share `v`" → reflect it
    /// on the matching slider and let the user group-edit. `None`
    /// means "they disagree" → disable that slider so a stray drag
    /// can't collapse the mixed set onto one value.
    SyncMultiAgreement {
        size: Option<crate::style::Size>,
        smooth: crate::sketch_board::SmoothLevelMulti,
    },
    /// Fill-shape button clicked. Mirrors `ToolbarEvent::ToggleFill`
    /// upstream and flips the local `fill_shapes` flag so the icon +
    /// tooltip in the right cluster update reactively.
    ToggleFill,
    /// Set the local `fill_shapes` mirror to the given value without
    /// emitting outbound `ToggleFill`. Used when the `F` keyboard
    /// shortcut toggles fill from outside the toolbar so the
    /// button icon + tooltip stay in sync.
    SetFillShapes(bool),
    /// Blur-algorithm popover picked a style (or sketch_board cycled
    /// to one). When `emit_upstream` is true the handler also
    /// forwards `BlurStyleSelected` upstream so sketch_board updates
    /// the active BlurTool + persists; when false (selection-sync
    /// path) only the local mirror + MenuButton icon + tooltip
    /// refresh, no toast, no re-apply.
    SetBlurStyle {
        style: BlurStyle,
        emit_upstream: bool,
    },
    /// Same shape as `SetBlurStyle` for the arrow-geometry picker.
    SetArrowStyle {
        style: ArrowStyle,
        emit_upstream: bool,
    },
    /// Same shape for the highlighter style (TextLocked / Normal).
    /// Handler updates local mirror + MenuButton preview and re-emits
    /// `HighlighterStyleSelected` upstream so sketch_board persists +
    /// toasts. Fired by both the popover-click path and the
    /// double-tap-cycle's round-trip from sketch_board.
    SetHighlighterStyle(crate::tools::HighlighterStyle),
    /// Sync the text-background DropDown to a variant. When
    /// `emit_upstream` is true (cycle path) the dropdown's natural
    /// `connect_selected_notify` re-emits `TextBackgroundSelected`
    /// upstream — sketch_board then re-applies the same value
    /// idempotently. When false (selection-sync path) the silent
    /// flag suppresses the dropdown's notify so the just-clicked
    /// drawable isn't redundantly re-styled or toasted.
    SetTextBackground {
        bg: crate::tools::TextBackground,
        emit_upstream: bool,
    },
    /// Snap the Spotlight / Highlighter slider widgets to the given
    /// values without re-emitting upstream. Fired by sketch_board
    /// when the user re-enters those tools so the slider always
    /// reflects the saved default (or the user's overridden default)
    /// instead of the previous session's drag.
    SetSpotlightDarkness(f32),
    SetHighlighterOpacity(f32),
    SetBrushPostSmooth(usize),
    /// Re-read the persisted brush-smoothness default and re-position
    /// the slider's tick mark to match. Fired right after the user's
    /// right-click → "Save as default" so the visible mark jumps to
    /// confirm the save. Idempotent — safe to fire when nothing has
    /// changed (the mark just re-renders at the same position).
    RefreshBrushSmoothMarks,
}

/// Source pixbuf size for swatch icons. Rendered down via
/// `gtk::Image::set_pixel_size` at the call site — we keep the source
/// large so the cairo-drawn rounded corners stay smooth on hi-dpi.
const SWATCH_PIXBUF_SIZE: i32 = 40;
/// Corner radius in pixbuf pixels. Tuned with `SWATCH_PIXBUF_SIZE` so
/// the displayed swatch matches `.color-slot-empty`'s 4px CSS radius
/// once scaled to its on-screen size (~20px).
const SWATCH_PIXBUF_RADIUS: f64 = 8.0;

/// Units used by the image-resize popover. Pixels = the literal
/// target dimensions; Percent = a multiplier on the current image
/// dimensions (100 means "no change"). Stored in an `Rc<Cell>` so
/// the popover's connect_* closures can read the live value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ResizeUnits {
    Pixels,
    Percent,
}

/// Map a crop-mode bg-color preset to the RGBA used to render its
/// swatch in the picker popover + MenuButton. Auto stays
/// semi-transparent black (the legacy dim color); Transparent is
/// handled separately via `crop_bg_swatch_pixbuf` (it gets a
/// Photoshop-style checkerboard chip instead of a solid color), so
/// the value returned here for that variant is unused; the named
/// presets are solid; Custom keeps the user's stored RGB at full
/// alpha.
fn crop_bg_preset_swatch(bg: crate::tools::CropBgColor) -> Color {
    use crate::tools::CropBgColor;
    match bg {
        CropBgColor::Auto => Color::new(0, 0, 0, 128),
        CropBgColor::Transparent => Color::new(0, 0, 0, 0),
        CropBgColor::White => Color::new(255, 255, 255, 255),
        CropBgColor::Gray => Color::new(128, 128, 128, 255),
        CropBgColor::Black => Color::new(0, 0, 0, 255),
        CropBgColor::Custom(r, g, b) => Color::new(
            (r * 255.0).clamp(0.0, 255.0) as u8,
            (g * 255.0).clamp(0.0, 255.0) as u8,
            (b * 255.0).clamp(0.0, 255.0) as u8,
            255,
        ),
    }
}

/// Pixbuf shown in the bg-color MenuButton + each popover row.
/// `Transparent` gets a checkerboard chip (otherwise the row's
/// swatch would render invisibly against the popover); everything
/// else delegates to the regular rounded-rect swatch.
fn crop_bg_swatch_pixbuf(bg: crate::tools::CropBgColor) -> Pixbuf {
    use crate::tools::CropBgColor;
    if matches!(bg, CropBgColor::Transparent) {
        create_transparent_swatch_pixbuf()
    } else {
        create_icon_pixbuf(crop_bg_preset_swatch(bg))
    }
}

/// Rounded-rect swatch with a Photoshop-style transparency
/// checkerboard fill, plus a 1 px outline so the tile reads as a
/// distinct chip against the popover background. Used for the
/// "Transparent" crop-bg preset row and MenuButton mirror so the
/// option actually has a visible chip (a fully-transparent
/// `create_icon_pixbuf` rendered an invisible blank).
fn create_transparent_swatch_pixbuf() -> Pixbuf {
    use relm4::gtk::cairo;
    use relm4::gtk::gdk;

    let size = SWATCH_PIXBUF_SIZE;
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, size, size)
        .expect("create transparent swatch cairo surface");
    let ctx = cairo::Context::new(&surface).expect("create transparent swatch cairo context");

    let w = size as f64;
    let h = size as f64;
    let r = SWATCH_PIXBUF_RADIUS;
    let pi = std::f64::consts::PI;
    ctx.new_sub_path();
    ctx.arc(w - r, r, r, -pi / 2.0, 0.0);
    ctx.arc(w - r, h - r, r, 0.0, pi / 2.0);
    ctx.arc(r, h - r, r, pi / 2.0, pi);
    ctx.arc(r, r, r, pi, 3.0 * pi / 2.0);
    ctx.close_path();
    // Clip subsequent draws to the rounded rect so the checkerboard
    // doesn't bleed past the corners.
    ctx.clip_preserve();

    // Light + dark gray, sized so the chip shows ~4 × 4 cells at the
    // default 40 px pixbuf — enough to read as "checker" without
    // turning into noise.
    let cell = (size as f64 / 5.0).max(2.0);
    let dark = 0.55_f64;
    let light = 0.85_f64;
    ctx.set_source_rgba(light, light, light, 1.0);
    ctx.paint().expect("fill light cells");
    ctx.set_source_rgba(dark, dark, dark, 1.0);
    let cells_x = (w / cell).ceil() as i32 + 1;
    let cells_y = (h / cell).ceil() as i32 + 1;
    for j in 0..cells_y {
        for i in 0..cells_x {
            if (i + j) % 2 == 0 {
                continue;
            }
            ctx.rectangle(i as f64 * cell, j as f64 * cell, cell, cell);
        }
    }
    ctx.fill().expect("fill dark cells");

    // Outline so the chip has a defined edge even against a
    // light-themed popover background.
    ctx.reset_clip();
    ctx.new_sub_path();
    ctx.arc(w - r, r, r, -pi / 2.0, 0.0);
    ctx.arc(w - r, h - r, r, 0.0, pi / 2.0);
    ctx.arc(r, h - r, r, pi / 2.0, pi);
    ctx.arc(r, r, r, pi, 3.0 * pi / 2.0);
    ctx.close_path();
    ctx.set_source_rgba(0.0, 0.0, 0.0, 0.35);
    ctx.set_line_width(1.0);
    ctx.stroke().expect("stroke transparent swatch outline");
    drop(ctx);

    gdk::pixbuf_get_from_surface(&surface, 0, 0, size, size)
        .expect("transparent swatch surface → pixbuf")
}

fn create_icon_pixbuf(color: Color) -> Pixbuf {
    // GTK4's CSS `border-radius` doesn't clip a `GtkImage`'s pixbuf —
    // it only rounds the widget's own background/border. So we bake the
    // rounded rectangle directly into the pixbuf via cairo: transparent
    // corners, solid color elsewhere. That way both the popover swatch
    // and the always-visible MenuButton swatch render as the same
    // rounded square shape as the dashed placeholder slots.
    use relm4::gtk::cairo;
    use relm4::gtk::gdk;

    let size = SWATCH_PIXBUF_SIZE;
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, size, size)
        .expect("create swatch cairo surface");
    let ctx = cairo::Context::new(&surface).expect("create swatch cairo context");

    let w = size as f64;
    let h = size as f64;
    let r = SWATCH_PIXBUF_RADIUS;
    let pi = std::f64::consts::PI;
    ctx.new_sub_path();
    ctx.arc(w - r, r, r, -pi / 2.0, 0.0);
    ctx.arc(w - r, h - r, r, 0.0, pi / 2.0);
    ctx.arc(r, h - r, r, pi / 2.0, pi);
    ctx.arc(r, r, r, pi, 3.0 * pi / 2.0);
    ctx.close_path();

    ctx.set_source_rgba(
        color.r as f64 / 255.0,
        color.g as f64 / 255.0,
        color.b as f64 / 255.0,
        color.a as f64 / 255.0,
    );
    ctx.fill().expect("fill swatch");
    drop(ctx);

    gdk::pixbuf_get_from_surface(&surface, 0, 0, size, size).expect("swatch surface → pixbuf")
}

/// Displayed size for popover swatches and placeholders. Sized down
/// so each cell reads as a distinct chip with breathing room
/// between rows (see `row_spacing` in `build_color_popover_grid`).
/// The Add-button footer is pinned to the bottom of the left column
/// via a `vexpand` spacer, so the column ends flush with the
/// chooser's alpha-slider bottom regardless of how tall the chooser
/// grows.
const SWATCH_DISPLAY_SIZE: i32 = 24;

fn create_icon(color: Color) -> gtk::Image {
    let img = gtk::Image::from_pixbuf(Some(&create_icon_pixbuf(color)));
    img.set_pixel_size(SWATCH_DISPLAY_SIZE);
    img
}

/// Build a filled saved-custom swatch button. Wires up: the gio
/// action that selects the color (left-click), a `DragSource` that
/// carries the source slot index for drag-and-drop reordering, and a
/// secondary-button `GestureClick` that shows a Delete popover.
fn build_saved_custom_swatch(
    color: Color,
    slot: usize,
    selected: bool,
    sender: &ComponentSender<ToolsToolbar>,
    popover: &gtk::Popover,
) -> gtk::Widget {
    use relm4::gtk::gdk;

    let btn = gtk::ToggleButton::builder()
        .focusable(false)
        .focus_on_click(false)
        .hexpand(false)
        .vexpand(false)
        // Match the palette-swatch sizing path: pin the toggle button
        // to SWATCH_DISPLAY_SIZE so the `:checked` outline (a 2 px
        // box-shadow around the button bounds) reads as symmetric.
        .width_request(SWATCH_DISPLAY_SIZE)
        .height_request(SWATCH_DISPLAY_SIZE)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .child(&create_icon(color))
        .build();
    btn.add_css_class("flat");
    btn.add_css_class("color-swatch");
    btn.set_action::<ColorAction>(ColorButtons::CustomSaved(slot as u64));
    // Dismiss the popover after the user picks a color — matches
    // the palette-swatch behavior above. The action fires
    // `ColorButtonSelected` alongside this click so the model has
    // already committed the pick by the time the popover is down.
    let popover_for_dismiss = popover.clone();
    btn.connect_clicked(move |_| {
        popover_for_dismiss.popdown();
    });
    // Tooltip is attached by the caller via
    // `attach_floating_swatch_tooltip` — one shared popover for all
    // swatches in the grid.

    // DragSource — the payload carries the source slot index for the
    // legacy `connect_drop`-based reorder path (still kept as a
    // fallback if `connect_enter` never fires for some reason). The
    // live-reorder pipeline below doesn't depend on it: `BeginCustomDrag`
    // captures the color, and `LiveReorderCustomColor` looks the color
    // up by value as the user drags so the dragged item rides through
    // each slot the pointer crosses.
    let drag = gtk::DragSource::new();
    drag.set_actions(gdk::DragAction::MOVE);
    let slot_for_prepare = slot;
    let color_for_icon = color;
    drag.connect_prepare(move |src, _x, _y| {
        // Replace GTK's default drag icon (a generic "document"
        // glyph) with a faithful copy of the swatch being dragged.
        // The pixbuf is the same one we render in the picker, so the
        // user sees the actual color floating under the cursor.
        // Hotspot is the center of the swatch so it sits centered on
        // the cursor.
        let pixbuf = create_icon_pixbuf(color_for_icon);
        let texture = gdk::Texture::for_pixbuf(&pixbuf);
        src.set_icon(
            Some(&texture),
            SWATCH_DISPLAY_SIZE / 2,
            SWATCH_DISPLAY_SIZE / 2,
        );
        let value = (slot_for_prepare as u32).to_value();
        Some(gdk::ContentProvider::for_value(&value))
    });
    let sender_for_begin = sender.clone();
    let slot_for_begin = slot;
    drag.connect_drag_begin(move |_src, _drag| {
        sender_for_begin.input(ToolsToolbarInput::BeginCustomDrag(slot_for_begin));
    });
    let sender_for_end = sender.clone();
    drag.connect_drag_end(move |_src, _drag, _delete_data| {
        // `delete_data` is true when the source acknowledged the move.
        // We use it as the success/cancel signal — anything else means
        // the drag was rejected (drop outside any target, Esc, etc.)
        // and the live preview should revert.
        sender_for_end.input(ToolsToolbarInput::EndCustomDrag {
            success: _delete_data,
        });
    });
    btn.add_controller(drag);

    // Secondary-button (right-click) gesture → ephemeral "Delete"
    // popover. The popover is parented to the swatch and unparented
    // on close so it doesn't leak when the popover grid is rebuilt.
    let right_click = gtk::GestureClick::new();
    right_click.set_button(gdk::BUTTON_SECONDARY);
    let btn_for_menu = btn.clone();
    let sender_for_menu = sender.clone();
    right_click.connect_pressed(move |_g, _n, x, y| {
        let menu = gtk::Popover::builder()
            .has_arrow(false)
            .autohide(true)
            .build();
        menu.add_css_class("custom-color-menu");
        let delete = gtk::Button::with_label("Delete");
        delete.add_css_class("flat");
        delete.set_focusable(false);
        delete.set_focus_on_click(false);
        let menu_for_click = menu.clone();
        let sender_for_click = sender_for_menu.clone();
        delete.connect_clicked(move |_| {
            sender_for_click.input(ToolsToolbarInput::DeleteCustomColor(slot));
            menu_for_click.popdown();
        });
        menu.set_child(Some(&delete));
        menu.set_parent(&btn_for_menu);
        // Anchor at the click point so the menu pops up near the
        // pointer rather than at the swatch's top-left corner.
        menu.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        // GTK4 popovers parented manually need to be unparented when
        // closed — otherwise the swatch retains a reference and the
        // popover stays attached after the right column is rebuilt.
        menu.connect_closed(|m| m.unparent());
        menu.popup();
    });
    btn.add_controller(right_click);

    // Delete affordances live elsewhere: right-click for the "Delete"
    // popover, Backspace / Delete on the popover for the currently-
    // selected swatch. Previously the selected swatch wore an "×"
    // overlay badge; dropped to keep the palette visually quiet.
    let _ = selected;
    btn.upcast::<gtk::Widget>()
}

/// Pop entire trailing empty columns off `custom_colors`. A
/// "trailing empty column" is the rightmost saved-customs column
/// when *every* slot in that column is `None` — those columns
/// carry no information and just add visual noise. Mid-column
/// `None`s (gaps the user left intentionally by dragging a swatch
/// away) are preserved, as are columns whose last few slots are
/// `None` but earlier ones are filled. Only ever called on user-
/// terminated actions (drop, delete) — never on drag start, so a
/// user can drag the last swatch in a column out and still drop
/// it further to the right.
fn trim_trailing_empty_columns(slots: &mut Vec<Option<Color>>) {
    loop {
        let len = slots.len();
        if len == 0 {
            break;
        }
        let last_col_start = ((len - 1) / SLOTS_PER_COLUMN) * SLOTS_PER_COLUMN;
        let all_empty = slots[last_col_start..].iter().all(Option::is_none);
        if all_empty {
            slots.truncate(last_col_start);
        } else {
            break;
        }
    }
}

/// When dropping ONTO a filled swatch in column `target_col`,
/// finds the first `None` slot strictly after `target` and still
/// within the same column. The shift-down can stop at that `None`
/// (the `None` is consumed by the shift, and slots past it stay
/// put), so dropping into a column with mid-column gaps doesn't
/// uselessly push the rest of the column further. Returns `None`
/// when no gap exists in the same column past the target — in
/// which case callers fall back to a list-growing standard insert.
fn find_same_column_gap(slots: &[Option<Color>], target: usize) -> Option<usize> {
    let target_col = target / SLOTS_PER_COLUMN;
    let col_end = ((target_col + 1) * SLOTS_PER_COLUMN).min(slots.len());
    (target + 1..col_end).find(|&i| slots[i].is_none())
}

/// Build the dashed empty-slot placeholder. Clickable: a primary-button
/// `GestureClick` fires `SelectEmptySlot(slot)` so the user can pre-
/// target a specific empty cell before clicking "+ Add to custom
/// colors". When `selected` is true, the placeholder wears the
/// `.color-slot-empty-selected` class for a brighter outline ring.
/// Drop-target wiring is added separately in
/// `attach_reorder_drop_target` so the same code path works for
/// filled swatches and empty placeholders.
fn build_dashed_placeholder(
    slot: usize,
    selected: bool,
    sender: &ComponentSender<ToolsToolbar>,
) -> gtk::Widget {
    use relm4::gtk::gdk;
    let placeholder = gtk::Box::builder()
        .width_request(SWATCH_DISPLAY_SIZE)
        .height_request(SWATCH_DISPLAY_SIZE)
        .hexpand(false)
        .vexpand(false)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    placeholder.add_css_class("color-slot-empty");
    if selected {
        placeholder.add_css_class("color-slot-empty-selected");
    }
    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_PRIMARY);
    let sender_for_click = sender.clone();
    click.connect_released(move |_g, _n, _x, _y| {
        sender_for_click.input(ToolsToolbarInput::SelectEmptySlot(slot));
    });
    placeholder.add_controller(click);
    placeholder.upcast::<gtk::Widget>()
}

/// Remove every page from the color-picker's `gtk::Stack` except the
/// currently-visible one. Scheduled after each `refresh_color_popover`
/// outside an active drag (the fade completes within ~`STACK_FADE_MS`)
/// and again after `EndCustomDrag` to drain anything accumulated mid-
/// drag. Safe to call at any time — the visible child is always kept.
fn clean_up_old_popover_pages(stack: &gtk::Stack) {
    let visible = stack.visible_child();
    let mut child = stack.first_child();
    while let Some(c) = child {
        let next = c.next_sibling();
        if visible.as_ref() != Some(&c) {
            stack.remove(&c);
        }
        child = next;
    }
}

/// Build the brighter, solid-outlined ghost slot used to preview
/// where a drag-in-flight swatch will land. Same geometry as the
/// dashed empty slot so it reads as a sibling cell rather than
/// reshuffling the grid layout; the visual treatment is owned by
/// the `.color-slot-ghost` CSS class.
fn build_ghost_placeholder() -> gtk::Widget {
    let placeholder = gtk::Box::builder()
        .width_request(SWATCH_DISPLAY_SIZE)
        .height_request(SWATCH_DISPLAY_SIZE)
        .hexpand(false)
        .vexpand(false)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    placeholder.add_css_class("color-slot-ghost");
    placeholder.upcast::<gtk::Widget>()
}

/// Attach a `DropTarget` to a slot widget that drives the live
/// drag-reorder pipeline. On pointer enter it fires
/// `LiveReorderCustomColor` so the ghost preview tracks the cursor;
/// on drop it fires `EndCustomDrag { success: true }` so the model
/// persists the final order. Accepting on filled and empty slots
/// alike lets the user drag a color into any position, including
/// past the end of the saved list.
fn attach_reorder_drop_target(
    widget: &gtk::Widget,
    target_slot: usize,
    sender: &ComponentSender<ToolsToolbar>,
) {
    use relm4::gtk::gdk;
    let drop_target = gtk::DropTarget::new(u32::static_type(), gdk::DragAction::MOVE);
    // Live-reorder: every time the drag pointer enters this slot's
    // bounds, ask the model to relocate the dragged color here so the
    // user sees the new order in real time instead of having to drop
    // to find out where the swatch will land.
    let sender_for_enter = sender.clone();
    drop_target.connect_enter(move |_dt, _x, _y| {
        sender_for_enter.input(ToolsToolbarInput::LiveReorderCustomColor {
            target: target_slot,
        });
        gdk::DragAction::MOVE
    });
    let sender_for_drop = sender.clone();
    drop_target.connect_drop(move |_dt, _value, _x, _y| {
        // By the time `connect_drop` fires the live-reorder path has
        // already mutated the list to the correct order — just emit
        // `EndCustomDrag { success: true }` so the model persists.
        // (We can't read the payload reliably here when the source
        // widget has been re-built mid-drag, so we don't depend on it.)
        sender_for_drop.input(ToolsToolbarInput::EndCustomDrag { success: true });
        true
    });
    widget.add_controller(drop_target);
}

#[relm4::component(pub)]
impl Component for ToolsToolbar {
    type Init = ();
    type Input = ToolsToolbarInput;
    type Output = ToolbarEvent;
    type CommandOutput = ();

    view! {
        // Vertical wrapper so the toolbar can grow a second row in
        // Wrap layout (left and right clusters move down to their
        // own row below the main CenterBox). In Normal the wrap
        // row stays hidden and this is visually identical to a
        // plain CenterBox.
        root = gtk::Box {
            set_orientation: gtk::Orientation::Vertical,
            set_valign: Align::Start,
            add_css_class: "toolbar",
            add_css_class: "toolbar-top",

            #[watch]
            set_visible: model.visible,

            // CenterBox mirrors the bottom row's layout so the toolbar's
            // three logical clusters (view+history on the left, drawing
            // tools in the middle, color+save on the right) sit at the
            // window's left/center/right edges instead of clustering in
            // the middle with empty space on each side. The cluster
            // pattern matches a typical editor toolbar.
            gtk::CenterBox {

            #[wrap(Some)]
            #[name(start_widget_box)]
            set_start_widget = &gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,

                // Hidden in Wrap layout (the left cluster moves
                // to the second row); visible in Normal where it
                // sits in the CenterBox's natural start slot.
                #[watch]
                set_visible: matches!(model.layout, TopBarLayout::Normal),

                // Normal start cluster — view + history ops. Hidden
                // when the Crop tool is active so the crop-mode top
                // toolbar can show its own start contents (just the
                // Crop indicator) without these competing for width.
                // In Wrap layout this Box is re-parented into
                // `top_wrap_row` (start slot) so the left buttons
                // stay reachable from the second row.
                #[name(left_cluster)]
                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 2,
                    #[watch]
                    set_visible: model.current_tool != Tools::Crop,

                    // 1:1 and Fit-to-window buttons used to sit here;
                    // both are reachable from the zoom control's
                    // popover (100% / Fit Canvas), so they were dropped
                    // to keep the left cluster about the same width as
                    // the right one — GtkCenterBox reserves symmetric
                    // side space, so balanced clusters are what let the
                    // tool row wrap right at its packed width instead
                    // of ~70 px early.
                    gtk::Button {
                        set_focusable: false,
                        set_hexpand: false,

                        set_icon_name: "recycling-bin",
                        install_tooltip: "Reset all annotations (Delete)",
                        connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::Reset);},
                    },
                    gtk::Separator {},
                    gtk::Button {
                        set_focusable: false,
                        set_hexpand: false,

                        set_icon_name: "arrow-undo-filled",
                        install_tooltip: "Undo (Ctrl-Z)",
                        connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::Undo);},
                    },
                    gtk::Button {
                        set_focusable: false,
                        set_hexpand: false,

                        set_icon_name: "arrow-redo-filled",
                        install_tooltip: "Redo (Ctrl-Y)",
                        connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::Redo);},
                    },
                    gtk::Separator {},
                    gtk::Button {
                        set_focusable: false,
                        set_hexpand: false,

                        set_icon_name: "layer-diagonal-regular",
                        install_tooltip: "Toggle layer panel (Ctrl-L)",
                        connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::ToggleLayerPanel);},
                    },
                },

                // Crop-mode start cluster — single "you are here"
                // indicator showing the Crop icon as a visual anchor.
                // Inert: it's just a marker; tool switching happens via
                // the bottom-row Cancel/Crop buttons or keyboard
                // shortcuts.
                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 2,
                    #[watch]
                    set_visible: model.current_tool == Tools::Crop,

                    gtk::Image {
                        set_icon_name: Some("crop-filled"),
                        set_pixel_size: 18,
                        set_margin_start: 4,
                        set_margin_end: 4,
                    },
                },
            },

            #[wrap(Some)]
            set_center_widget = &gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,

                // Normal center cluster — the 12 tool toggle buttons.
                // Hidden in Crop mode so the crop-options cluster
                // (next sibling below) takes over the center slot.
                // FlowBox (not a plain Box) so the 12 tool buttons can
                // wrap onto a second row when the window is too narrow
                // for all of them. That collapses the cluster's minimum
                // width to ~6 buttons, which is what lets the whole
                // window shrink past the single-row toolbar width —
                // a plain Box would pin the window minimum at the full
                // 12-button width. `min_children_per_line: 6` caps the
                // wrap at two rows (12 buttons ÷ 6 = 2); `max: 12` keeps
                // every tool on one row while there's room.
                #[name(normal_center_box)]
                gtk::FlowBox {
                set_orientation: gtk::Orientation::Horizontal,
                set_selection_mode: gtk::SelectionMode::None,
                set_homogeneous: true,
                set_min_children_per_line: 6,
                set_max_children_per_line: 12,
                set_row_spacing: 2,
                set_column_spacing: 2,
                set_can_focus: false,
                #[watch]
                set_visible: model.current_tool != Tools::Crop,

                #[name(pointer_button)]
                gtk::ToggleButton {
                    set_focusable: false,
                    set_hexpand: false,

                    set_icon_name: "cursor-regular",
                    // tooltip set programmatically
                    ActionablePlus::set_action::<ToolsAction>: Tools::Pointer,
                },
                #[name(crop_button)]
                gtk::ToggleButton {
                    set_focusable: false,
                    set_hexpand: false,

                    set_icon_name: "crop-filled",
                    // tooltip set programmatically
                    ActionablePlus::set_action::<ToolsAction>: Tools::Crop,
                },
                #[name(brush_button)]
                gtk::ToggleButton {
                    set_focusable: false,
                    set_hexpand: false,

                    set_icon_name: "pen-regular",
                    // tooltip set programmatically
                    ActionablePlus::set_action::<ToolsAction>: Tools::Brush,
                },
                #[name(line_button)]
                gtk::ToggleButton {
                    set_focusable: false,
                    set_hexpand: false,

                    set_icon_name: "minus-large",
                    // tooltip set programmatically
                    ActionablePlus::set_action::<ToolsAction>: Tools::Line,
                },
                #[name(arrow_button)]
                gtk::ToggleButton {
                    set_focusable: false,
                    set_hexpand: false,

                    set_icon_name: "arrow-up-right-filled",
                    // tooltip set programmatically
                    ActionablePlus::set_action::<ToolsAction>: Tools::Arrow,
                },
                #[name(rectangle_button)]
                gtk::ToggleButton {
                    set_focusable: false,
                    set_hexpand: false,

                    set_icon_name: "checkbox-unchecked-regular",
                    // tooltip set programmatically
                    ActionablePlus::set_action::<ToolsAction>: Tools::Rectangle,
                },
                #[name(ellipse_button)]
                gtk::ToggleButton {
                    set_focusable: false,
                    set_hexpand: false,

                    set_icon_name: "circle-regular",
                    // tooltip set programmatically
                    ActionablePlus::set_action::<ToolsAction>: Tools::Ellipse,
                },
                #[name(text_button)]
                gtk::ToggleButton {
                    set_focusable: false,
                    set_hexpand: false,

                    set_icon_name: "text-case-title-regular",
                    // tooltip set programmatically
                    ActionablePlus::set_action::<ToolsAction>: Tools::Text,
                },
                #[name(marker_button)]
                gtk::ToggleButton {
                    set_focusable: false,
                    set_hexpand: false,

                    set_icon_name: "number-circle-1-regular",
                    // tooltip set programmatically
                    ActionablePlus::set_action::<ToolsAction>: Tools::Marker,
                },
                #[name(blur_button)]
                gtk::ToggleButton {
                    set_focusable: false,
                    set_hexpand: false,

                    set_icon_name: "drop-regular",
                    // tooltip set programmatically
                    ActionablePlus::set_action::<ToolsAction>: Tools::Blur,
                },
                #[name(highlight_button)]
                gtk::ToggleButton {
                    set_focusable: false,
                    set_hexpand: false,

                    set_icon_name: "highlight-regular",
                    // tooltip set programmatically
                    ActionablePlus::set_action::<ToolsAction>: Tools::Highlighter,
                },
                #[name(spotlight_button)]
                gtk::ToggleButton {
                    set_focusable: false,
                    set_hexpand: false,

                    set_icon_name: "flashlight-regular",
                    // tooltip set programmatically
                    ActionablePlus::set_action::<ToolsAction>: Tools::Spotlight,
                },
                },

                // Crop-mode center cluster — aspect-ratio picker,
                // W/H inputs, background-color picker, rotate/flip,
                // image-size resize. Built up across subsequent
                // commits; this commit lands the aspect-ratio
                // dropdown.
                #[name(crop_center_box)]
                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 6,
                    #[watch]
                    set_visible: model.current_tool == Tools::Crop,

                    // Aspect-ratio picker. Built off
                    // `AspectRatio::ALL_LABELS` so adding a variant
                    // there auto-extends the menu. Selecting a
                    // non-Freeform option snaps the current crop to
                    // the new ratio and enforces it on subsequent
                    // drags (see `CropTool::set_aspect_ratio`).
                    #[name(crop_aspect_dropdown)]
                    gtk::DropDown {
                        set_focusable: false,
                        set_height_request: 34,
                        add_css_class: "compact-control",
                        install_tooltip: "Aspect ratio",
                        set_model: Some(&gtk::StringList::new(
                            crate::tools::AspectRatio::ALL_LABELS,
                        )),
                        set_selected: 0,
                        connect_selected_notify[sender] => move |dd| {
                            let ratio = crate::tools::AspectRatio::from_index(
                                dd.selected() as usize,
                            );
                            sender
                                .output_sender()
                                .emit(ToolbarEvent::CropAspectRatioChanged(ratio));
                            // Hand focus back to the canvas so single-
                            // key shortcuts (F = fill, etc.) keep
                            // working without a manual tab-back.
                            sender.output_sender().emit(ToolbarEvent::FocusCanvas);
                        },
                    },

                    // Direct-entry W and H inputs. Typing a value
                    // and pressing Enter (or moving focus) recenters
                    // the crop rect on the image at the typed
                    // dimensions, honoring the active aspect-ratio
                    // constraint. Drag updates flow back so the
                    // entries always show the current rect size
                    // (suspended while the entry has focus so we
                    // don't clobber half-typed input). `.crop-dim-entry`
                    // gives them tight 2-px horizontal padding so
                    // they don't dominate the toolbar's center
                    // cluster — the default compact-control padding
                    // makes the entries triple-wide for a 3-digit
                    // value.
                    #[name(crop_width_entry)]
                    gtk::Entry {
                        add_css_class: "compact-control",
                        add_css_class: "crop-dim-entry",
                        set_focusable: true,
                        set_hexpand: false,
                        set_height_request: 34,
                        set_width_request: 48,
                        set_width_chars: 3,
                        set_max_width_chars: 4,
                        set_max_length: 5,
                        set_input_purpose: gtk::InputPurpose::Digits,
                        install_tooltip: "Crop width (px)",
                        connect_activate[sender] => move |e| {
                            let v = e.text().trim().parse::<i32>().ok();
                            sender.input(ToolsToolbarInput::CropWidthEntered(v));
                        },
                    },
                    gtk::Button {
                        set_focusable: false,
                        set_hexpand: false,
                        set_height_request: 34,
                        add_css_class: "compact-control",
                        add_css_class: "flat",
                        set_icon_name: "arrow-swap-regular",
                        install_tooltip: "Swap width and height",
                        connect_clicked[sender] => move |_| {
                            sender.input(ToolsToolbarInput::CropDimensionsSwap);
                            sender.output_sender().emit(ToolbarEvent::FocusCanvas);
                        },
                    },
                    #[name(crop_height_entry)]
                    gtk::Entry {
                        add_css_class: "compact-control",
                        add_css_class: "crop-dim-entry",
                        set_focusable: true,
                        set_hexpand: false,
                        set_height_request: 34,
                        set_width_request: 48,
                        set_width_chars: 3,
                        set_max_width_chars: 4,
                        set_max_length: 5,
                        set_input_purpose: gtk::InputPurpose::Digits,
                        install_tooltip: "Crop height (px)",
                        connect_activate[sender] => move |e| {
                            let v = e.text().trim().parse::<i32>().ok();
                            sender.input(ToolsToolbarInput::CropHeightEntered(v));
                        },
                    },

                    // Background-color matte picker. Sets the color
                    // rendered OUTSIDE the crop rectangle while
                    // editing (Auto = the legacy semi-transparent
                    // black dim; Transparent removes the matte
                    // entirely; the named presets paint a solid
                    // frame in white / gray / black; Custom Color…
                    // is a placeholder for a follow-up picker
                    // dialog and currently maps to a mid-gray).
                    // Crop background-color picker — MenuButton
                    // showing the current preset's swatch, opening
                    // a popover of labeled swatches (built
                    // imperatively in init, mirrors the main
                    // color-picker UX). Selection updates the
                    // swatch via `#[watch]` on `crop_bg_color`.
                    #[name(crop_bg_color_menu_btn)]
                    gtk::MenuButton {
                        set_focusable: false,
                        set_focus_on_click: false,
                        set_hexpand: false,
                        set_height_request: 34,
                        add_css_class: "compact-control",
                        set_has_frame: true,
                        set_always_show_arrow: false,
                        install_tooltip: "Background color (outside crop)",

                        #[wrap(Some)]
                        set_child = &gtk::Image {
                            set_pixel_size: 18,
                            set_can_target: false,
                            #[watch]
                            set_from_pixbuf: Some(&crop_bg_swatch_pixbuf(model.crop_bg_color)),
                        },
                    },

                    gtk::Separator {
                        set_orientation: gtk::Orientation::Vertical,
                    },

                    // Rotate 90° CCW — width and height swap so the
                    // window re-fits around the rotated image. Same
                    // drawable-positions-stay limitation as flip.
                    gtk::Button {
                        set_focusable: false,
                        set_hexpand: false,
                        set_height_request: 34,
                        add_css_class: "compact-control",
                        add_css_class: "flat",
                        set_icon_name: "rotate-90-degrees-ccw",
                        install_tooltip: "Rotate 90° counter-clockwise",
                        connect_clicked[sender] => move |_| {
                            sender.output_sender().emit(ToolbarEvent::RotateImage);
                            sender.output_sender().emit(ToolbarEvent::FocusCanvas);
                        },
                    },

                    // Flip horizontal — mirrors the background image
                    // around its vertical center. Existing drawables
                    // keep their image-space positions (documented in
                    // FemtoVGArea::flip_image_horizontal).
                    gtk::Button {
                        set_focusable: false,
                        set_hexpand: false,
                        set_height_request: 34,
                        add_css_class: "compact-control",
                        add_css_class: "flat",
                        set_icon_name: "flip-horizontal-regular",
                        install_tooltip: "Flip horizontal",
                        connect_clicked[sender] => move |_| {
                            sender.output_sender().emit(ToolbarEvent::FlipHorizontal);
                            sender.output_sender().emit(ToolbarEvent::FocusCanvas);
                        },
                    },

                    // Hide the separator and the inline "Image
                    // size:" label once the bar has wrapped — the
                    // resize MenuButton still carries the
                    // dimensions ("1536 × 1728 px") so the
                    // affordance survives without the header label,
                    // and the saved width keeps Cancel/Crop on the
                    // same row at narrow widths.
                    gtk::Separator {
                        set_orientation: gtk::Orientation::Vertical,
                        #[watch]
                        set_visible: matches!(model.layout, TopBarLayout::Normal),
                    },

                    // "Image size: W × H px" MenuButton. The popover
                    // (built imperatively in init) lets the user
                    // type new pixel dimensions and resample. Label
                    // refreshes via #[watch] on `image_width` /
                    // `image_height`, which are pushed up by
                    // `ImageDimensionsChanged` after rotate / resize.
                    gtk::Label {
                        set_focusable: false,
                        set_hexpand: false,
                        set_label: "Image size:",
                        add_css_class: "dim-label",
                        #[watch]
                        set_visible: matches!(model.layout, TopBarLayout::Normal),
                    },
                    #[name(resize_menu_btn)]
                    gtk::MenuButton {
                        set_focusable: false,
                        set_hexpand: false,
                        set_height_request: 34,
                        add_css_class: "compact-control",
                        // CSS class gives the button a gray background
                        // even before hover, matching the resize
                        // MenuButton's "subtle but clickable" look in
                        // the standard pattern. The Adwaita default for a
                        // MenuButton in a toolbar context renders
                        // frameless until hover.
                        add_css_class: "image-size-menubtn",
                        // Frame on + always-show-arrow for the
                        // dropdown chevron — without these the
                        // MenuButton renders frameless inside a
                        // toolbar context and reads as a label
                        // rather than a clickable control.
                        set_has_frame: true,
                        set_always_show_arrow: true,
                        install_tooltip: "Resize image",
                        #[watch]
                        set_label: &format!(
                            "{} × {} px",
                            model.image_width / model.display_scale.max(1),
                            model.image_height / model.display_scale.max(1),
                        ),
                    },
                },

            },

            #[wrap(Some)]
            #[name(normal_end_host)]
            set_end_widget = &gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,

                // Normal end cluster — color picker + copy/save
                // actions. Hidden in Crop mode; the Cancel/Crop
                // buttons take over the right edge instead. The
                // OUTER `gtk::Box` named `right_cluster` is the
                // single re-parentable handle for Wrap layout:
                // `SetLayout(Wrap)` unparents it from
                // `normal_end_host` and hands it to the wrap row
                // below, then reverses on exit.
                #[name(right_cluster)]
                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 2,
                    #[watch]
                    set_visible: model.current_tool != Tools::Crop,

                    // Unified color picker — single MenuButton showing the current
                    // color; the popover (built in init) holds the palette and a
                    // custom-color picker, mirroring a standard compact picker.
                    // `focusable: false` blocks Tab navigation; `focus_on_click:
                    // false` blocks mouse-click focus too — both are needed or
                    // shortcuts stop working until the user tabs focus back to
                    // the canvas.
                    #[name(color_button)]
                    gtk::MenuButton {
                        set_focusable: false,
                        set_focus_on_click: false,
                        set_hexpand: false,
                        add_css_class: "color-picker-button",
                        add_css_class: "flat",
                        install_tooltip: "Color (1–0 picks a palette color)",
                        set_always_show_arrow: false,

                        #[wrap(Some)]
                        set_child = &gtk::Image {
                            set_pixel_size: 18,
                            set_can_target: false,
                            #[watch]
                            set_from_pixbuf: Some(&model.current_color_pixbuf),
                        },
                    },
                    gtk::Separator {},
                    gtk::Button {
                        set_focusable: false,
                        set_hexpand: false,

                        set_icon_name: "copy-regular",
                        install_tooltip: "Copy to clipboard (Ctrl+C)",
                        connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::CopyClipboard);},
                    },
                    gtk::Button {
                        set_focusable: false,
                        set_hexpand: false,

                        set_icon_name: "save-regular",
                        install_tooltip: "Save (Ctrl+S)",
                        connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::SaveFile);},

                        set_visible: APP_CONFIG.read().output_filename().is_some()
                    },
                    gtk::Button {
                        set_focusable: false,
                        set_hexpand: false,

                        set_icon_name: "save-multiple-regular",
                        install_tooltip: "Save as (Ctrl+Shift+S)",
                        connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::SaveFileAs);},
                    },
                    // Settings sits last, set off by a separator —
                    // mirrors the left cluster's trailing layers button.
                    gtk::Separator {},
                    gtk::Button {
                        set_focusable: false,
                        set_hexpand: false,

                        set_icon_name: "settings-regular",
                        install_tooltip: "Preferences (Ctrl+,)",
                        connect_clicked[sender] => move |_| {
                            sender.output_sender().emit(ToolbarEvent::OpenPreferences);
                        },
                    },
                },

                // Crop-mode end cluster — Cancel + Crop action
                // buttons. Cancel mirrors Esc (drop pending edit,
                // restore prior commit if any); Crop mirrors Enter
                // (apply the in-progress crop and exit the tool).
                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 6,
                    #[watch]
                    set_visible: model.current_tool == Tools::Crop,

                    gtk::Button {
                        set_focusable: false,
                        set_hexpand: false,
                        set_height_request: 34,
                        set_label: "Cancel",
                        add_css_class: "compact-control",
                        install_tooltip: "Cancel crop (Esc)",
                        connect_clicked[sender] => move |_| {
                            sender.output_sender().emit(ToolbarEvent::CancelCrop);
                            sender.output_sender().emit(ToolbarEvent::FocusCanvas);
                        },
                    },
                    gtk::Button {
                        set_focusable: false,
                        set_hexpand: false,
                        set_height_request: 34,
                        set_label: "Crop",
                        add_css_class: "compact-control",
                        add_css_class: "suggested-action",
                        install_tooltip: "Apply crop (Enter)",
                        connect_clicked[sender] => move |_| {
                            sender.output_sender().emit(ToolbarEvent::ApplyCrop);
                            sender.output_sender().emit(ToolbarEvent::FocusCanvas);
                        },
                    },
                },
            },
            },

            // Second-row container for Wrap layout. Empty in Normal
            // layout; when the `SetLayout(Wrap)` handler fires it
            // receives `left_cluster` then `right_cluster` appended
            // in order. A centered horizontal Box so the two clusters
            // sit together as one group under the centered tool row
            // — both rows read as centered. `set_visible` is watched
            // so the row only takes vertical space in Wrap.
            #[name(top_wrap_row)]
            gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,
                set_halign: gtk::Align::Center,
                set_spacing: 6,
                #[watch]
                set_visible: matches!(model.layout, TopBarLayout::Wrap),
            },
        },
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match message {
            ToolsToolbarInput::SetVisibility(visible) => self.visible = visible,
            ToolsToolbarInput::ToggleVisibility => {
                self.visible = !self.visible;
            }
            ToolsToolbarInput::SwitchSelectedTool(tool) => {
                // Change state of action, let GTK update the UI
                self.tool_action.change_state(&tool.to_variant());

                if let Some(selected_tool_button) = self.tool_buttons.get(&tool) {
                    self.active_button = Some(selected_tool_button.clone());
                }
                self.current_tool = tool;
            }
            ToolsToolbarInput::ColorButtonSelected(button) => {
                let color = self.map_button_to_color(button);
                self.color_action.change_state(&button.to_variant());
                self.current_color = color;
                self.current_color_pixbuf = create_icon_pixbuf(color);
                crate::state::save_last_color(color);
                // Picking a filled swatch consumes any pending empty-slot
                // intent — the user moved focus to an existing color.
                if self.selected_empty_slot.take().is_some() {
                    self.refresh_color_popover(&sender);
                }
                sender
                    .output_sender()
                    .emit(ToolbarEvent::ColorSelected(color));
            }
            ToolsToolbarInput::InlinePickerColorChanged(color) => {
                // The inline picker emitted a new RGBA — apply it as
                // the live drawing color so the picked value tracks
                // what the user is mixing. If it happens to match a
                // palette / saved-custom slot, mark that slot checked;
                // otherwise leave the action on `Custom`.
                self.custom_color = color;
                let matched_button = APP_CONFIG
                    .read()
                    .color_palette()
                    .palette()
                    .iter()
                    .position(|c| *c == color)
                    .map(|i| ColorButtons::Palette(i as u64))
                    .or_else(|| {
                        self.custom_colors
                            .iter()
                            .position(|slot| matches!(slot, Some(c) if *c == color))
                            .map(|i| ColorButtons::CustomSaved(i as u64))
                    })
                    .unwrap_or(ColorButtons::Custom);
                self.color_action.change_state(&matched_button.to_variant());
                self.current_color = color;
                self.current_color_pixbuf = create_icon_pixbuf(color);
                crate::state::save_last_color(color);
                sender
                    .output_sender()
                    .emit(ToolbarEvent::ColorSelected(color));
                // The eyedropper button's click handler popped the
                // picker down so it wouldn't cover the screen during
                // the pick (see the `find_eyedropper_button` hook in
                // `build_color_popover`). A color arriving here while
                // the popover is hidden is therefore an eyedropper
                // result — re-open the picker so the user lands back
                // on it with the freshly-picked color, ready to "Add
                // to custom colors" or keep editing. Re-opening through
                // the MenuButton (not `popover.popup()`) keeps its
                // `active` state in sync so the next click still
                // toggles correctly.
                let popover_hidden = self.color_popover.as_ref().is_some_and(|p| !p.is_visible());
                if popover_hidden && let Some(btn) = &self.color_button {
                    btn.popup();
                }
            }
            ToolsToolbarInput::TogglePickerExpansion => {
                self.picker_expanded = !self.picker_expanded;
                // SlideRight Revealer + panel wrapper — known-good
                // structure from HEAD that preserves the
                // colorplane's gradient across toggle cycles.
                if let Some(rev) = &self.picker_revealer {
                    if self.picker_expanded {
                        // Must be visible BEFORE starting the reveal
                        // animation; otherwise GTK skips it. The
                        // matching set_visible(false) on collapse
                        // happens in `connect_child_revealed_notify`
                        // after the conceal animation completes.
                        rev.set_visible(true);
                    }
                    rev.set_reveal_child(self.picker_expanded);
                }
                if let Some(icon) = &self.picker_caret_icon {
                    icon.set_icon_name(Some(if self.picker_expanded {
                        "pan-start-symbolic"
                    } else {
                        "pan-end-symbolic"
                    }));
                }
                // Re-seed the chooser to the current color each time
                // the panel opens so reopening doesn't strand the user
                // at a previously-edited hue.
                if self.picker_expanded
                    && let Some(chooser) = &self.picker_chooser
                {
                    chooser.set_rgba(&RGBA::from(self.current_color));
                }
                // Grid rendering depends on `picker_expanded` (the
                // "next column" empty placeholders only show when
                // the chooser is open, since growing the saved list
                // requires the chooser anyway). Refresh so the
                // extra column shows/hides with the toggle.
                self.refresh_color_popover(&sender);
            }
            ToolsToolbarInput::SaveCustomColor(color) => {
                // If the user pre-selected an empty slot, drop the new
                // color there (growing the list with `None` padding so
                // the visual position is preserved); otherwise fall back
                // to the original append behavior.
                if let Some(target) = self.selected_empty_slot.take() {
                    let mut slots = self.custom_colors.clone();
                    while slots.len() <= target {
                        slots.push(None);
                    }
                    slots[target] = Some(color);
                    crate::state::save_custom_colors(&slots);
                    self.custom_colors = slots;
                } else {
                    self.custom_colors = crate::state::append_custom_color(color);
                }
                self.refresh_color_popover(&sender);
            }
            ToolsToolbarInput::SelectEmptySlot(slot) => {
                // Toggle: click the same empty slot again to clear,
                // click a different one to move the target. Refreshes
                // the grid so the dashed cell renders its selected ring.
                if self.selected_empty_slot == Some(slot) {
                    self.selected_empty_slot = None;
                } else {
                    self.selected_empty_slot = Some(slot);
                }
                self.refresh_color_popover(&sender);
            }
            ToolsToolbarInput::ClearEmptySlotSelection => {
                // Popover closed — drop the pending target without a
                // refresh so we don't fight the closing animation.
                self.selected_empty_slot = None;
            }
            ToolsToolbarInput::DeleteCustomColor(index) => {
                // Delete blanks the slot, preserving the layout the
                // user has built up via drag-and-drop. Trailing
                // empties get trimmed eagerly afterward so the
                // column doesn't end on a hanging placeholder.
                if index >= self.custom_colors.len() {
                    return;
                }
                if self.custom_colors[index].is_none() {
                    return;
                }
                self.custom_colors[index] = None;
                trim_trailing_empty_columns(&mut self.custom_colors);
                crate::state::save_custom_colors(&self.custom_colors);
                self.sync_color_action();
                self.refresh_color_popover(&sender);
            }
            ToolsToolbarInput::DeleteCurrentSavedColor => {
                // Only saved-custom colors are deletable — palette
                // entries are config-driven and stay put. Match by
                // value against the first `Some` whose color equals
                // `current_color`; blank that slot, then trim.
                let target = self
                    .custom_colors
                    .iter()
                    .position(|slot| matches!(slot, Some(c) if *c == self.current_color));
                if let Some(idx) = target {
                    self.custom_colors[idx] = None;
                    trim_trailing_empty_columns(&mut self.custom_colors);
                    crate::state::save_custom_colors(&self.custom_colors);
                    self.sync_color_action();
                    self.refresh_color_popover(&sender);
                }
            }
            ToolsToolbarInput::BeginCustomDrag(slot) => {
                // Blank the origin slot so it renders as a gap during
                // the drag; the dragged color is held in
                // `dragging_color` until drop. On a successful drop
                // we re-insert at `dragging_preview_slot` with
                // shift-down (existing slots from there onward move
                // one position later), leaving the origin slot empty
                // — i.e. a drag effectively *moves* the color and
                // creates a gap behind it. The pre-drag snapshot is
                // the only way back if the user cancels.
                if slot >= self.custom_colors.len() {
                    return;
                }
                let Some(color) = self.custom_colors[slot] else {
                    // Trying to drag an empty slot — nothing to do.
                    return;
                };
                // Snapshot BEFORE blanking the origin — a cancelled
                // drag (drop outside any target) restores from this
                // snapshot, and the snapshot must include the dragged
                // color in its original slot for the restore to put it
                // back.
                self.pre_drag_snapshot = Some(self.custom_colors.clone());
                self.custom_colors[slot] = None;
                self.dragging_color = Some(color);
                // Ghost lands at the origin slot initially — the
                // user hasn't moved the pointer yet, and rendering
                // a ghost at the origin shows the swatch is "lifted
                // off" without immediately shifting anything else.
                // We DO NOT trim trailing empty columns here even
                // if the origin was the only filled slot in its
                // column: the user might be lifting that swatch
                // specifically to move it further to the right,
                // and removing the column would collapse the drop
                // targets they intended to use.
                self.dragging_preview_slot = Some(slot);
                // Mark the popover as dragging so the per-swatch hover
                // ring is suppressed — the `.color-slot-ghost`
                // placeholder is the only drop affordance the user
                // needs to see while the drag is in flight.
                if let Some(popover) = &self.color_popover {
                    popover.add_css_class("dragging");
                }
                self.sync_color_action();
                self.refresh_color_popover(&sender);
            }
            ToolsToolbarInput::LiveReorderCustomColor { target } => {
                // Move the ghost preview slot to wherever the pointer
                // is currently hovering. The actual color list is
                // *not* touched here — the origin slot is already
                // `None` from drag-begin, and the rendering layer
                // visualises the post-drop layout by shifting slots
                // at/after the ghost down by one.
                //
                // Target is NOT clamped to `custom_colors.len()` —
                // the user can drop past the current end of the list
                // (anywhere in the grid's "next row" empty slots) and
                // `EndCustomDrag` pads the list with `None`s up to
                // that slot before inserting. Without this, dropping
                // anywhere in the trailing empties always landed at
                // `len()`, so the user couldn't position past the
                // first empty slot of a new column.
                if self.dragging_color.is_none() {
                    return;
                }
                if self.dragging_preview_slot == Some(target) {
                    return;
                }
                self.dragging_preview_slot = Some(target);
                self.refresh_color_popover(&sender);
            }
            ToolsToolbarInput::EndCustomDrag { success } => {
                // Idempotent: connect_drop AND connect_drag_end both
                // route here, so a successful drop fires this twice.
                // Bail when there's nothing left to commit/revert.
                let Some(color) = self.dragging_color.take() else {
                    self.pre_drag_snapshot = None;
                    self.dragging_preview_slot = None;
                    return;
                };
                if success {
                    let target = self
                        .dragging_preview_slot
                        .unwrap_or(self.custom_colors.len());
                    // Pad with `None`s up to the target so the user
                    // can drop past the end of the list (any
                    // leading gap they create gets preserved as
                    // mid-list empties).
                    while self.custom_colors.len() < target {
                        self.custom_colors.push(None);
                    }
                    if target >= self.custom_colors.len() {
                        // Drop landed at or past the current end —
                        // push the color and keep the list compact.
                        self.custom_colors.push(Some(color));
                    } else if self.custom_colors[target].is_none() {
                        // REPLACE an empty slot — no shift.
                        self.custom_colors[target] = Some(color);
                    } else if let Some(absorb) = find_same_column_gap(&self.custom_colors, target) {
                        // INSERT-with-shift, but stop at the first
                        // `None` in the same column past the target.
                        // Items at [target..absorb] each move one
                        // slot later; the `None` at `absorb` is
                        // consumed by the shifted-in tail; items
                        // past `absorb` stay in place. List length
                        // unchanged.
                        for i in (target + 1..=absorb).rev() {
                            self.custom_colors[i] = self.custom_colors[i - 1];
                        }
                        self.custom_colors[target] = Some(color);
                    } else {
                        // No in-column gap to absorb — standard
                        // insert. List grows by one as everything
                        // from `target` onward shifts down.
                        self.custom_colors.insert(target, Some(color));
                    }
                    trim_trailing_empty_columns(&mut self.custom_colors);
                    crate::state::save_custom_colors(&self.custom_colors);
                } else if let Some(snapshot) = self.pre_drag_snapshot.take() {
                    // Cancel: full revert to the snapshot.
                    self.custom_colors = snapshot;
                }
                self.pre_drag_snapshot = None;
                self.dragging_preview_slot = None;
                // Drag is over — restore the per-swatch hover ring.
                if let Some(popover) = &self.color_popover {
                    popover.remove_css_class("dragging");
                }
                self.sync_color_action();
                self.refresh_color_popover(&sender);
                // Reap all the popover-grid pages that piled up while
                // the drag was held open (one per hover-enter event).
                // The cleanup runs after `STACK_FADE_MS` so the final
                // crossfade has a chance to finish before old children
                // disappear.
                if let Some(stack) = self.color_popover_stack.clone() {
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis(STACK_FADE_MS as u64 + 50),
                        move || {
                            clean_up_old_popover_pages(&stack);
                        },
                    );
                }
            }
            ToolsToolbarInput::EscapePressed => {
                // First Esc during a drag cancels the drag and keeps
                // the popover open. GTK4's popover has an internal
                // `GtkShortcutController` that binds Escape to
                // `popover.close` at a higher priority than our
                // `EventControllerKey`, so we can't suppress the
                // close — we let GTK close the popover (which also
                // triggers `drag_end`, but our drag-cancel logic
                // here runs first and clears the state before
                // `EndCustomDrag` sees it as already-handled), then
                // immediately re-popup so the user keeps the picker
                // open. There's a single-frame flicker on cancel —
                // acceptable tradeoff vs. fighting GTK's internal
                // shortcut binding.
                //
                // Esc with no drag in flight just lets GTK close the
                // popover normally.
                if self.dragging_color.is_some() {
                    self.dragging_color = None;
                    if let Some(snapshot) = self.pre_drag_snapshot.take() {
                        self.custom_colors = snapshot;
                    }
                    self.dragging_preview_slot = None;
                    if let Some(popover) = &self.color_popover {
                        popover.remove_css_class("dragging");
                        popover.popup();
                    }
                    self.sync_color_action();
                    self.refresh_color_popover(&sender);
                }
            }
            ToolsToolbarInput::RefreshColorPopover => {
                self.refresh_color_popover(&sender);
            }
            ToolsToolbarInput::CropDimensionsChanged { width, height } => {
                self.crop_width = width;
                self.crop_height = height;
                let s = self.display_scale.max(1);
                // Refresh the entries — but only when they don't
                // currently have focus, so a user mid-typing in
                // the W or H field doesn't see their text wiped
                // every drag tick. Values are divided by the
                // display scale so the user sees LOGICAL pixels
                // (the dimensions they perceive on screen) rather
                // than the doubled image-pixel count on HiDPI.
                if let Some(e) = &self.crop_width_entry
                    && !e.has_focus()
                {
                    e.set_text(&(width / s).to_string());
                }
                if let Some(e) = &self.crop_height_entry
                    && !e.has_focus()
                {
                    e.set_text(&(height / s).to_string());
                }
            }
            ToolsToolbarInput::CropWidthEntered(value) => {
                let s = self.display_scale.max(1);
                if let Some(w_logical) = value
                    && w_logical > 0
                {
                    sender
                        .output_sender()
                        .emit(ToolbarEvent::CropDimensionsSet {
                            width: w_logical * s,
                            height: self.crop_height.max(1),
                        });
                    sender.output_sender().emit(ToolbarEvent::FocusCanvas);
                } else if let Some(e) = &self.crop_width_entry {
                    // Snap back to the last known good value so the
                    // entry doesn't keep showing unparseable text
                    // after Enter on (e.g.) empty input.
                    e.set_text(&(self.crop_width / s).to_string());
                }
            }
            ToolsToolbarInput::CropHeightEntered(value) => {
                let s = self.display_scale.max(1);
                if let Some(h_logical) = value
                    && h_logical > 0
                {
                    sender
                        .output_sender()
                        .emit(ToolbarEvent::CropDimensionsSet {
                            width: self.crop_width.max(1),
                            height: h_logical * s,
                        });
                    sender.output_sender().emit(ToolbarEvent::FocusCanvas);
                } else if let Some(e) = &self.crop_height_entry {
                    e.set_text(&(self.crop_height / s).to_string());
                }
            }
            ToolsToolbarInput::CropDimensionsSwap => {
                if self.crop_width > 0 && self.crop_height > 0 {
                    sender
                        .output_sender()
                        .emit(ToolbarEvent::CropDimensionsSet {
                            width: self.crop_height,
                            height: self.crop_width,
                        });
                }
            }
            ToolsToolbarInput::CropBgColorSelected(bg) => {
                self.crop_bg_color = bg;
                // Refresh the popover's "Custom Color…" row so it
                // reflects the user's actual choice. Only update on
                // Custom selections — switching to a named preset
                // shouldn't wipe the previously-picked Custom value
                // (the user might want to flip back to it).
                if let crate::tools::CropBgColor::Custom(r, g, b) = bg {
                    if let Some(cell) = &self.crop_bg_custom_rgb {
                        cell.set((r, g, b));
                    }
                    if let Some(swatch) = &self.crop_bg_custom_swatch {
                        swatch
                            .set_from_pixbuf(Some(&create_icon_pixbuf(crop_bg_preset_swatch(bg))));
                    }
                }
                sender
                    .output_sender()
                    .emit(ToolbarEvent::CropBgColorChanged(bg));
                sender.output_sender().emit(ToolbarEvent::FocusCanvas);
            }
            ToolsToolbarInput::ImageDimensionsChanged { width, height } => {
                self.image_width = width;
                self.image_height = height;
                let s = self.display_scale.max(1);
                // Pre-populate the resize popover's entries so it
                // opens already showing the current image dims in
                // LOGICAL pixels. The popover only opens transiently
                // (close on Resize / Cancel / click-out), so we can
                // refresh these unconditionally without worrying
                // about clobbering live typing.
                if let Some(e) = &self.resize_width_entry {
                    e.set_text(&(width / s).to_string());
                }
                if let Some(e) = &self.resize_height_entry {
                    e.set_text(&(height / s).to_string());
                }
                // Mirror into the popover's shared state so
                // aspect-lock + percent-mode math has the live
                // original dimensions.
                if let Some(d) = &self.resize_orig_dims {
                    d.set((width.max(1), height.max(1)));
                }
            }
            ToolsToolbarInput::SetDisplayScale(scale) => {
                self.display_scale = scale.max(1);
                // Refresh the entries with the new scale applied —
                // covers the startup case where ImageDimensionsChanged
                // fired before this scale was known, leaving the
                // entries showing image-pixel values.
                let s = self.display_scale;
                if let Some(e) = &self.crop_width_entry
                    && !e.has_focus()
                {
                    e.set_text(&(self.crop_width / s).to_string());
                }
                if let Some(e) = &self.crop_height_entry
                    && !e.has_focus()
                {
                    e.set_text(&(self.crop_height / s).to_string());
                }
                if let Some(e) = &self.resize_width_entry {
                    e.set_text(&(self.image_width / s).to_string());
                }
                if let Some(e) = &self.resize_height_entry {
                    e.set_text(&(self.image_height / s).to_string());
                }
                if let Some(d) = &self.resize_display_scale {
                    d.set(s);
                }
            }
            ToolsToolbarInput::SetLayout(target) => {
                if self.layout == target {
                    return;
                }
                let (Some(right), Some(end_host), Some(wrap_row), Some(left), Some(start_box)) = (
                    self.right_cluster.as_ref(),
                    self.normal_end_host.as_ref(),
                    self.top_wrap_row.as_ref(),
                    self.left_cluster.as_ref(),
                    self.start_widget_box.as_ref(),
                ) else {
                    // Init hasn't run yet — stash the target and
                    // let the next post-init `SetLayout` (the
                    // resize handler fires every frame width
                    // moves, so this is harmless) drive the work.
                    self.layout = target;
                    return;
                };
                match target {
                    TopBarLayout::Normal => {
                        wrap_row.remove(left);
                        wrap_row.remove(right);
                        start_box.prepend(left);
                        // Prepend the right cluster so it sits
                        // ahead of the crop-mode end cluster
                        // sibling (preserves the original
                        // z-order between regular and crop
                        // content).
                        end_host.prepend(right);
                    }
                    TopBarLayout::Wrap => {
                        start_box.remove(left);
                        end_host.remove(right);
                        // Append left then right so they read
                        // left-to-right inside the centered group.
                        wrap_row.append(left);
                        wrap_row.append(right);
                    }
                }
                self.layout = target;
            }
        }
    }

    fn init(
        _: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let sender_tmp: ComponentSender<ToolsToolbar> = sender.clone();
        let tool_action: RelmAction<ToolsAction> = RelmAction::new_stateful_with_target_value(
            &APP_CONFIG.read().initial_tool(),
            move |_, state, value| {
                *state = value;
                // notify parent of change
                sender_tmp
                    .output_sender()
                    .emit(ToolbarEvent::ToolSelected(*state));
            },
        );

        // Resolve the starting color via the shared helper so the
        // toolbar swatch and sketch_board's drawing style agree on the
        // first stroke. The helper restores the user's previous color
        // across launches; falls back to red so a fresh state file
        // starts on the most-reached-for annotation color.
        let palette: Vec<Color> = APP_CONFIG.read().color_palette().palette().to_vec();
        let saved_customs = crate::state::load_custom_colors();
        let initial_color = crate::state::initial_color();
        // Mirror the popover's "checked" highlight onto whichever
        // swatch represents the restored color: a palette entry, one
        // of the persisted saved customs, or — failing both — the
        // generic `Custom` bucket (no slot in the popover).
        let initial_button = palette
            .iter()
            .position(|c| *c == initial_color)
            .map(|i| ColorButtons::Palette(i as u64))
            .or_else(|| {
                saved_customs
                    .iter()
                    .position(|slot| matches!(slot, Some(c) if *c == initial_color))
                    .map(|i| ColorButtons::CustomSaved(i as u64))
            })
            .unwrap_or(ColorButtons::Custom);
        // Seed the dialog with the restored color so re-opening the
        // picker shows where the user left off.
        let custom_color = initial_color;
        let initial_color_pixbuf = create_icon_pixbuf(initial_color);

        // Color action — palette-or-Custom enum, tracks current selection
        // and routes through `ColorButtonSelected` so the swatch updates.
        // Initial state matches `initial_button` so the popover's
        // ":checked" highlight lands on the restored color on first open.
        let sender_tmp = sender.clone();
        let color_action: RelmAction<ColorAction> =
            RelmAction::new_stateful_with_target_value(&initial_button, move |_, state, value| {
                *state = value;
                sender_tmp.input(ToolsToolbarInput::ColorButtonSelected(value));
            });

        let mut model = ToolsToolbar {
            visible: !APP_CONFIG.read().default_hide_toolbars(),
            active_button: None,
            tool_buttons: HashMap::new(),
            tool_action: tool_action.clone().into(),
            current_tool: Tools::Pointer,
            crop_width: 0,
            crop_height: 0,
            crop_width_entry: None,
            crop_height_entry: None,
            image_width: 0,
            image_height: 0,
            resize_width_entry: None,
            resize_height_entry: None,
            crop_bg_color: crate::tools::CropBgColor::Auto,
            crop_bg_custom_swatch: None,
            crop_bg_custom_rgb: None,
            display_scale: 1,
            resize_orig_dims: None,
            resize_display_scale: None,
            resize_aspect_locked: None,
            resize_units: None,
            current_color: initial_color,
            current_color_pixbuf: initial_color_pixbuf,
            custom_color,
            custom_colors: saved_customs,
            color_action: SimpleAction::from(color_action.clone()),
            color_button: None,
            color_popover: None,
            dragging_color: None,
            pre_drag_snapshot: None,
            dragging_preview_slot: None,
            color_popover_stack: None,
            color_popover_page_id: 0,
            picker_expanded: false,
            picker_revealer: None,
            picker_chooser: None,
            picker_caret_icon: None,
            selected_empty_slot: None,
            layout: TopBarLayout::Normal,
            right_cluster: None,
            normal_end_host: None,
            top_wrap_row: None,
            left_cluster: None,
            start_widget_box: None,
        };
        let widgets = view_output!();

        // Stash the W/H entries so the `CropDimensionsChanged`
        // handler can has-focus-check before refreshing their text.
        model.crop_width_entry = Some(widgets.crop_width_entry.clone());
        model.crop_height_entry = Some(widgets.crop_height_entry.clone());

        // Wrap-layout plumbing. We capture the host containers and
        // the two re-parentable cluster Boxes so `SetLayout` can
        // imperatively move left/right clusters between their
        // Normal and Wrap homes.
        model.right_cluster = Some(widgets.right_cluster.clone());
        model.normal_end_host = Some(widgets.normal_end_host.clone());
        model.top_wrap_row = Some(widgets.top_wrap_row.clone());
        model.left_cluster = Some(widgets.left_cluster.clone());
        model.start_widget_box = Some(widgets.start_widget_box.clone());

        // Build the "Image size" popover imperatively and attach to
        // the MenuButton in the crop-mode center cluster. Built here
        // rather than in `view!` because the relm4 inline macro
        // doesn't have a clean syntax for "popover containing a
        // grid + two entries + lock toggle + units dropdown + two
        // buttons" with all the cross-widget connect_* wiring.
        use std::cell::Cell as StdCell;
        use std::rc::Rc as StdRc;

        // Shared state — the closures need `Rc<Cell>` access to
        // (a) the original image pixel dims (for aspect-ratio +
        // percent calculations), (b) the display DPR (logical →
        // image pixels at Resize time), (c) whether the aspect
        // lock is active, and (d) the current units. Updated by
        // the corresponding ToolsToolbarInput handlers.
        let resize_orig_dims = StdRc::new(StdCell::new((
            model.image_width.max(1),
            model.image_height.max(1),
        )));
        let resize_display_scale_state = StdRc::new(StdCell::new(model.display_scale.max(1)));
        let resize_aspect_locked = StdRc::new(StdCell::new(false));
        let resize_units = StdRc::new(StdCell::new(ResizeUnits::Pixels));

        let resize_popover = gtk::Popover::builder().has_arrow(true).build();
        let popover_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(8)
            .margin_end(8)
            .build();

        let grid = gtk::Grid::builder()
            .row_spacing(6)
            .column_spacing(8)
            .build();
        let w_label = gtk::Label::builder()
            .label("Width:")
            .halign(gtk::Align::End)
            .build();
        let w_entry = gtk::Entry::builder()
            .input_purpose(gtk::InputPurpose::Digits)
            .width_chars(3)
            .max_width_chars(6)
            .max_length(6)
            .hexpand(false)
            .halign(gtk::Align::Start)
            .build();
        let h_label = gtk::Label::builder()
            .label("Height:")
            .halign(gtk::Align::End)
            .build();
        let h_entry = gtk::Entry::builder()
            .input_purpose(gtk::InputPurpose::Digits)
            .width_chars(3)
            .max_width_chars(6)
            .max_length(6)
            .hexpand(false)
            .halign(gtk::Align::Start)
            .build();

        // Lock toggle (vertically centered, spans both rows). Icon
        // flips between locked / unlocked. Active state means
        // "changing W or H auto-syncs the other to the original
        // image's aspect ratio".
        let lock_btn = gtk::ToggleButton::builder()
            .icon_name("changes-allow-symbolic")
            .focusable(false)
            .css_classes(["flat"])
            .build();
        lock_btn.set_tooltip_text(Some("Lock aspect ratio"));

        // Units dropdown — pixels vs. percent. Spans both rows so
        // it sits vertically centered next to the lock toggle,
        // matching the lock's "between W and H" placement.
        let units_model = gtk::StringList::new(&["pixels", "percent"]);
        let units_dropdown = gtk::DropDown::builder()
            .model(&units_model)
            .selected(0)
            .focusable(false)
            .valign(gtk::Align::Center)
            .build();

        grid.attach(&w_label, 0, 0, 1, 1);
        grid.attach(&w_entry, 1, 0, 1, 1);
        grid.attach(&lock_btn, 2, 0, 1, 2);
        grid.attach(&units_dropdown, 3, 0, 1, 2);
        grid.attach(&h_label, 0, 1, 1, 1);
        grid.attach(&h_entry, 1, 1, 1, 1);
        popover_box.append(&grid);

        // Buttons split the popover width 50/50 so the row reads
        // as a balanced footer even after the W/H entries shrink.
        let button_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .homogeneous(true)
            .margin_top(4)
            .build();
        let cancel_btn = gtk::Button::builder().label("Cancel").hexpand(true).build();
        let resize_btn = gtk::Button::builder()
            .label("Resize")
            .css_classes(["suggested-action"])
            .hexpand(true)
            .build();
        button_row.append(&cancel_btn);
        button_row.append(&resize_btn);
        popover_box.append(&button_row);
        resize_popover.set_child(Some(&popover_box));
        widgets.resize_menu_btn.set_popover(Some(&resize_popover));
        install_menu_toggle_dismiss(&widgets.resize_menu_btn, &resize_popover);

        // Helper Rc<Cell> to break the W↔H change-feedback loop:
        // when aspect-lock is on and the W handler updates H (or
        // vice versa), the recipient's `connect_changed` fires; this
        // flag tells the recipient to no-op so we don't ping-pong.
        let is_syncing = StdRc::new(StdCell::new(false));

        // Aspect-lock toggle — flip the Rc<Cell> and refresh the
        // icon. The lock state only affects future typing; the
        // entries don't auto-rebalance the moment the lock is
        // engaged (locking "captures" the
        // current ratio, leaving values alone until the next edit).
        let aspect_lock_for_toggle = resize_aspect_locked.clone();
        lock_btn.connect_toggled(move |btn| {
            let active = btn.is_active();
            aspect_lock_for_toggle.set(active);
            btn.set_icon_name(if active {
                "changes-prevent-symbolic"
            } else {
                "changes-allow-symbolic"
            });
        });

        // Units dropdown — refresh the entries with values in the
        // newly-selected units. Switching to percent shows "100"
        // (= no change); switching to pixels shows the current
        // image dims in logical pixels.
        let units_for_dd = resize_units.clone();
        let orig_for_dd = resize_orig_dims.clone();
        let scale_for_dd = resize_display_scale_state.clone();
        let w_for_dd = w_entry.clone();
        let h_for_dd = h_entry.clone();
        let syncing_for_dd = is_syncing.clone();
        units_dropdown.connect_selected_notify(move |dd| {
            let new_units = if dd.selected() == 0 {
                ResizeUnits::Pixels
            } else {
                ResizeUnits::Percent
            };
            units_for_dd.set(new_units);
            let (orig_w, orig_h) = orig_for_dd.get();
            let scale = scale_for_dd.get().max(1);
            // Mute change handlers while we set programmatic text.
            syncing_for_dd.set(true);
            match new_units {
                ResizeUnits::Pixels => {
                    w_for_dd.set_text(&(orig_w / scale).to_string());
                    h_for_dd.set_text(&(orig_h / scale).to_string());
                }
                ResizeUnits::Percent => {
                    w_for_dd.set_text("100");
                    h_for_dd.set_text("100");
                }
            }
            syncing_for_dd.set(false);
        });

        // W → H sync when aspect-locked.
        let h_for_w = h_entry.clone();
        let orig_for_w = resize_orig_dims.clone();
        let lock_for_w = resize_aspect_locked.clone();
        let units_for_w = resize_units.clone();
        let syncing_for_w = is_syncing.clone();
        w_entry.connect_changed(move |w| {
            if syncing_for_w.get() || !lock_for_w.get() {
                return;
            }
            let Some(w_val) = w.text().trim().parse::<f32>().ok() else {
                return;
            };
            if w_val <= 0.0 {
                return;
            }
            let (orig_w, orig_h) = orig_for_w.get();
            if orig_w <= 0 || orig_h <= 0 {
                return;
            }
            let h_val = match units_for_w.get() {
                // Percent locked: same percent for both axes.
                ResizeUnits::Percent => w_val,
                // Pixels locked: H = W × (orig_h / orig_w).
                ResizeUnits::Pixels => w_val * (orig_h as f32) / (orig_w as f32),
            };
            syncing_for_w.set(true);
            h_for_w.set_text(&(h_val.round() as i32).to_string());
            syncing_for_w.set(false);
        });
        // H → W sync, mirror image.
        let w_for_h = w_entry.clone();
        let orig_for_h = resize_orig_dims.clone();
        let lock_for_h = resize_aspect_locked.clone();
        let units_for_h = resize_units.clone();
        let syncing_for_h = is_syncing.clone();
        h_entry.connect_changed(move |h| {
            if syncing_for_h.get() || !lock_for_h.get() {
                return;
            }
            let Some(h_val) = h.text().trim().parse::<f32>().ok() else {
                return;
            };
            if h_val <= 0.0 {
                return;
            }
            let (orig_w, orig_h) = orig_for_h.get();
            if orig_w <= 0 || orig_h <= 0 {
                return;
            }
            let w_val = match units_for_h.get() {
                ResizeUnits::Percent => h_val,
                ResizeUnits::Pixels => h_val * (orig_w as f32) / (orig_h as f32),
            };
            syncing_for_h.set(true);
            w_for_h.set_text(&(w_val.round() as i32).to_string());
            syncing_for_h.set(false);
        });

        let popover_for_cancel = resize_popover.clone();
        let sender_for_cancel = sender.clone();
        cancel_btn.connect_clicked(move |_| {
            popover_for_cancel.popdown();
            sender_for_cancel
                .output_sender()
                .emit(ToolbarEvent::FocusCanvas);
        });

        // Resize button: convert the typed values into image-pixel
        // dimensions based on the current units, then emit
        // ToolbarEvent::ResizeImage directly (we have all the state
        // here — display scale, units, orig dims — without needing
        // an intermediate input message).
        let popover_for_resize = resize_popover.clone();
        let sender_resize = sender.clone();
        let w_entry_resize = w_entry.clone();
        let h_entry_resize = h_entry.clone();
        let orig_for_resize = resize_orig_dims.clone();
        let scale_for_resize = resize_display_scale_state.clone();
        let units_for_resize = resize_units.clone();
        resize_btn.connect_clicked(move |_| {
            let w_val = w_entry_resize.text().trim().parse::<f32>().ok();
            let h_val = h_entry_resize.text().trim().parse::<f32>().ok();
            let (Some(w_val), Some(h_val)) = (w_val, h_val) else {
                return;
            };
            if w_val <= 0.0 || h_val <= 0.0 {
                return;
            }
            let (orig_w, orig_h) = orig_for_resize.get();
            let scale = scale_for_resize.get().max(1) as f32;
            let (target_w_px, target_h_px) = match units_for_resize.get() {
                ResizeUnits::Pixels => (
                    (w_val * scale).round() as i32,
                    (h_val * scale).round() as i32,
                ),
                ResizeUnits::Percent => (
                    (w_val / 100.0 * orig_w as f32).round() as i32,
                    (h_val / 100.0 * orig_h as f32).round() as i32,
                ),
            };
            if target_w_px > 0 && target_h_px > 0 {
                sender_resize
                    .output_sender()
                    .emit(ToolbarEvent::ResizeImage {
                        width: target_w_px,
                        height: target_h_px,
                    });
                popover_for_resize.popdown();
                sender_resize
                    .output_sender()
                    .emit(ToolbarEvent::FocusCanvas);
            }
        });

        // Enter in either entry submits the popover. `emit_clicked`
        // re-uses the same handler (with all its validation /
        // unit-conversion / popdown behavior) instead of duplicating
        // the logic per-entry.
        let resize_btn_for_w = resize_btn.clone();
        w_entry.connect_activate(move |_| {
            resize_btn_for_w.emit_clicked();
        });
        let resize_btn_for_h = resize_btn.clone();
        h_entry.connect_activate(move |_| {
            resize_btn_for_h.emit_clicked();
        });

        // Stash everything for handler access.
        model.resize_width_entry = Some(w_entry);
        model.resize_height_entry = Some(h_entry);
        model.resize_orig_dims = Some(resize_orig_dims);
        model.resize_display_scale = Some(resize_display_scale_state);
        model.resize_aspect_locked = Some(resize_aspect_locked);
        model.resize_units = Some(resize_units);

        // Crop bg-color picker popover — labeled-swatch list mirroring
        // the main color-picker UX (vs the prior text-only DropDown).
        // Each row is a flat button with a swatch image + label;
        // clicking emits `CropBgColorSelected` which updates the
        // MenuButton's `crop_bg_color` mirror (refreshing its own
        // swatch via #[watch]) and re-emits the outbound
        // `CropBgColorChanged` for sketch_board.
        use crate::tools::CropBgColor;
        let bg_popover = gtk::Popover::builder().has_arrow(true).build();
        let bg_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_top(4)
            .margin_bottom(4)
            .margin_start(4)
            .margin_end(4)
            .spacing(0)
            .build();
        // Seed the Custom row's swatch from the live `crop_bg_color`
        // so the popover already reflects a previously-picked color
        // (or mid-gray as the default placeholder before the user
        // has picked anything). Stashed below so the
        // `CropBgColorSelected` handler can refresh it on the fly.
        let custom_seed_rgb = match model.crop_bg_color {
            CropBgColor::Custom(r, g, b) => (r, g, b),
            _ => (0.5, 0.5, 0.5),
        };
        let custom_seed =
            CropBgColor::Custom(custom_seed_rgb.0, custom_seed_rgb.1, custom_seed_rgb.2);
        let custom_rgb_cell = std::rc::Rc::new(std::cell::Cell::new(custom_seed_rgb));
        let mut custom_swatch_handle: Option<gtk::Image> = None;
        for (bg, label_text) in [
            (CropBgColor::Transparent, "Transparent"),
            (CropBgColor::Auto, "Auto"),
            (CropBgColor::White, "White"),
            (CropBgColor::Gray, "Gray"),
            (CropBgColor::Black, "Black"),
            (custom_seed, "Custom Color\u{2026}"),
        ] {
            let row_btn = gtk::Button::builder()
                .css_classes(["flat"])
                .focusable(false)
                .build();
            let row_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(8)
                .build();
            let swatch = gtk::Image::from_pixbuf(Some(&crop_bg_swatch_pixbuf(bg)));
            swatch.set_pixel_size(SWATCH_DISPLAY_SIZE);
            let lbl = gtk::Label::new(Some(label_text));
            lbl.set_xalign(0.0);
            lbl.set_hexpand(true);
            row_box.append(&swatch);
            row_box.append(&lbl);
            row_btn.set_child(Some(&row_box));

            let popover_for_row = bg_popover.clone();
            let sender_for_row = sender.clone();
            let is_custom_row = matches!(bg, CropBgColor::Custom(..));
            if is_custom_row {
                custom_swatch_handle = Some(swatch.clone());
            }
            // Cloned per-row so each closure owns its handle; only
            // the custom row actually reads from it but every row
            // captures one to keep the closure signatures uniform.
            let custom_rgb_for_row = custom_rgb_cell.clone();
            row_btn.connect_clicked(move |btn| {
                popover_for_row.popdown();
                if is_custom_row {
                    // Open a modal color chooser so the user can pick
                    // an arbitrary matte color. On OK, push back as a
                    // `Custom(r, g, b)` selection (alpha is dropped —
                    // the matte is always fully opaque, the named
                    // "Auto" preset is the semi-transparent option).
                    let top = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
                    let mut builder = gtk::ColorChooserDialog::builder()
                        .modal(true)
                        .title("Pick crop background color");
                    if let Some(w) = &top {
                        builder = builder.transient_for(w);
                    }
                    let dialog = builder.build();
                    let (r, g, b) = custom_rgb_for_row.get();
                    dialog.set_rgba(&gtk::gdk::RGBA::new(r, g, b, 1.0));
                    let sender_for_dialog = sender_for_row.clone();
                    dialog.connect_response(move |dlg, response| {
                        if response == gtk::ResponseType::Ok {
                            let rgba = dlg.rgba();
                            let picked = CropBgColor::Custom(rgba.red(), rgba.green(), rgba.blue());
                            sender_for_dialog.input(ToolsToolbarInput::CropBgColorSelected(picked));
                        }
                        dlg.close();
                    });
                    dialog.show();
                } else {
                    sender_for_row.input(ToolsToolbarInput::CropBgColorSelected(bg));
                }
            });
            bg_box.append(&row_btn);
        }
        model.crop_bg_custom_swatch = custom_swatch_handle;
        model.crop_bg_custom_rgb = Some(custom_rgb_cell);
        bg_popover.set_child(Some(&bg_box));
        widgets
            .crop_bg_color_menu_btn
            .set_popover(Some(&bg_popover));
        install_menu_toggle_dismiss(&widgets.crop_bg_color_menu_btn, &bg_popover);

        // Build the popover for the unified color picker. Stash the
        // popover, the swatch_stack (for crossfade rebuilds), and the
        // inline-picker handles (revealer / chooser / arrow) so the
        // toggle handler can drive them without walking the tree.
        let handles = build_color_popover(&model, &sender);
        widgets.color_button.set_popover(Some(&handles.popover));
        let popover = handles.popover.clone();
        model.color_popover = Some(handles.popover);
        model.color_button = Some(widgets.color_button.clone());
        model.color_popover_stack = Some(handles.swatch_stack);
        model.color_popover_page_id = 1;
        model.picker_revealer = Some(handles.picker_revealer);
        model.picker_chooser = Some(handles.picker_chooser);
        model.picker_caret_icon = Some(handles.caret_icon);

        // Refocus the canvas when the popover closes so keyboard shortcuts
        // resume working without the user having to click on the canvas.
        // Also drop any pending empty-slot selection so reopening doesn't
        // strand the user mid-intent.
        {
            let sender = sender.clone();
            popover.connect_closed(move |_| {
                sender.input(ToolsToolbarInput::ClearEmptySlotSelection);
                sender.output_sender().emit(ToolbarEvent::FocusCanvas);
            });
        }

        // Toggle the color popover on a second click of the
        // MenuButton — same `focus_on_click: false` workaround as
        // the style-picker MenuButtons (see
        // `install_menu_toggle_dismiss`).
        install_menu_toggle_dismiss(&widgets.color_button, &popover);

        // Rebuild the swatch grid every time the popover is about to
        // open so preferences that affect the layout (e.g. "hide
        // default palette" toggling whether column 0 is the palette
        // or the first column of saved-customs) take effect on the
        // next open. Hooked on `notify::active` on the MenuButton
        // rather than `popover.connect_show` so the rebuild runs
        // BEFORE GTK maps the popover surface — otherwise the stack
        // mutation lands mid-fade and the Wayland surface gets
        // reconfigured to a different size/position one frame later,
        // producing a visible glitch render.
        {
            let sender = sender.clone();
            widgets
                .color_button
                .connect_notify_local(Some("active"), move |btn, _| {
                    if btn.property::<bool>("active") {
                        sender.input(ToolsToolbarInput::RefreshColorPopover);
                    }
                });
        }

        model.tool_buttons = HashMap::from([
            (Tools::Pointer, widgets.pointer_button.clone()),
            (Tools::Crop, widgets.crop_button.clone()),
            (Tools::Brush, widgets.brush_button.clone()),
            (Tools::Line, widgets.line_button.clone()),
            (Tools::Arrow, widgets.arrow_button.clone()),
            (Tools::Rectangle, widgets.rectangle_button.clone()),
            (Tools::Ellipse, widgets.ellipse_button.clone()),
            (Tools::Text, widgets.text_button.clone()),
            (Tools::Marker, widgets.marker_button.clone()),
            (Tools::Blur, widgets.blur_button.clone()),
            (Tools::Highlighter, widgets.highlight_button.clone()),
            (Tools::Spotlight, widgets.spotlight_button.clone()),
        ]);

        // reverse shortcuts mapping
        let config = APP_CONFIG.read();
        let tool_to_key_map: HashMap<&Tools, &char> = config
            .keybinds()
            .shortcuts()
            .iter()
            .inspect(|(hotkey, tool)| if hotkey.is_ascii_digit() {
                eprintln!("Warning: hotkey `{}` for tool `{}` overrides built-in hotkey to select a color from the palette", hotkey, tool);
            })
            .map(|(k, v)| (v, k))
            .collect();

        // Update tooltips based on configured keybinds.
        for (tool, button) in &model.tool_buttons {
            let display_name = tool.display_name();

            let tooltip = if let Some(key) = tool_to_key_map.get(tool) {
                format!("{} ({})", display_name, key.to_uppercase())
            } else {
                display_name.to_string()
            };
            button.install_tooltip(&tooltip);
        }

        // Set initial active button correctly
        let initial_tool = APP_CONFIG.read().initial_tool();
        model.current_tool = initial_tool;
        if let Some(button) = model.tool_buttons.get(&initial_tool) {
            model.active_button = Some(button.clone());
        }

        let mut group = RelmActionGroup::<ToolsToolbarActionGroup>::new();
        group.add_action(tool_action);
        group.register_for_widget(&widgets.root);

        // Color action lives in its own group so it can target both the
        // palette buttons inside the popover and any external triggers
        // (e.g. number-key shortcuts).
        let mut color_group = RelmActionGroup::<StyleToolbarActionGroup>::new();
        color_group.add_action(color_action);
        color_group.register_for_widget(&widgets.root);

        // Suppress unused-root warning; we keep the parameter in case a
        // later popover needs to anchor itself to the toplevel.
        let _ = root;

        ComponentParts { model, widgets }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ColorButtons {
    Palette(u64),
    /// Legacy "live custom" slot — the result of the last
    /// ColorChooserDialog "Select". Kept for the dialog seed value;
    /// the popover itself no longer surfaces a separate slot for it.
    Custom,
    /// One of the user's *saved* custom colors at the given index
    /// into `ToolsToolbar::custom_colors` (persisted via
    /// `state::append_custom_color`).
    CustomSaved(u64),
}

/// Variant-encoding offset that separates `CustomSaved(i)` from
/// `Palette(i)` within the single u64 the gio action carries.
/// `1 << 32` leaves a full 32-bit range each side — more than enough
/// for both palettes and saved-custom slots.
const CUSTOM_SAVED_OFFSET: u64 = 1 << 32;

impl StyleToolbar {
    /// Rebuild the size slider's six tick marks (XS…XXL), bolding
    /// the letter that matches the current tool's saved default
    /// (via Pango markup). Idempotent — clears any existing marks
    /// first. Called from `init()` for the first paint, on
    /// `ToolChanged` so the bold letter follows the active tool,
    /// and on `SaveSizeAsDefault` so the bold updates the moment
    /// the user persists a new default. Pointer / Crop don't own
    /// a size default; marks stay all-plain in those modes.
    /// Re-position the brush-smoothness slider's single tick mark so
    /// it points at the user's currently-persisted default
    /// (`state.toml :: brush-post-smooth-iterations`, falling back to
    /// the config / built-in 2). Called on init and after the user
    /// right-clicks → "Save as default" so the visible feedback
    /// confirms the save — without this the tick stays where it was
    /// at construction time and the user can't tell whether the save
    /// stuck.
    /// The size the slider should snap to when entering or
    /// deselecting from `tool`, honoring the
    /// `sticky_session_defaults` preference. When the preference is
    /// on AND the user has touched the slider for `tool` already in
    /// this session, return that in-session value; otherwise fall
    /// back to `state.toml`'s saved default, then to the tool's
    /// `builtin_default_size`. Returns `None` for tools with no saved
    /// and no builtin default — most tools — so the existing callers
    /// keep their `if let Some` shape.
    fn effective_size_for_tool(&self, tool: Tools) -> Option<Size> {
        if APP_CONFIG.read().sticky_session_defaults()
            && let Some(s) = self.session_size_per_tool.get(&tool).copied()
        {
            return Some(s);
        }
        crate::state::load_size_for_tool(tool).or_else(|| tool.builtin_default_size())
    }

    fn refresh_brush_smooth_slider_marks(&self) {
        let Some(slider) = self.brush_smooth_slider.as_ref() else {
            return;
        };
        slider.clear_marks();
        // "Smoothing" label rides the midpoint (3 out of 0..=6) —
        // same below-trough rhythm as the size / spotlight /
        // highlighter sliders so all the sliders end up the same
        // height when the cluster swaps between them.
        slider.add_mark(3.0, gtk::PositionType::Bottom, Some("Smoothing"));
        let saved = crate::state::load_brush_post_smooth_iterations()
            .unwrap_or_else(|| APP_CONFIG.read().brush_post_smooth_iterations());
        // Plain tick at the saved-default position. Tickless when
        // the default IS the midpoint — otherwise the "Smoothing"
        // label mark already carries that position's tick and
        // adding a second one there would render as a doubled
        // glyph in the same slot.
        if saved != 3 {
            slider.add_mark(saved as f64, gtk::PositionType::Bottom, None);
        }
    }

    fn refresh_size_slider_marks(&self) {
        let Some(slider) = self.size_slider.as_ref() else {
            return;
        };
        let labels = [
            (0.0_f64, "XS", Size::XSmall),
            (1.0, "S", Size::Small),
            (2.0, "M", Size::Medium),
            (3.0, "L", Size::Large),
            (4.0, "XL", Size::XLarge),
            (5.0, "XXL", Size::XXLarge),
        ];
        let default_size: Option<Size> =
            if matches!(self.current_tool, Tools::Pointer | Tools::Crop) {
                None
            } else {
                crate::state::load_size_for_tool(self.current_tool)
                    .or_else(|| self.current_tool.builtin_default_size())
            };
        slider.clear_marks();
        for (pos, letter, size) in labels {
            let markup = if Some(size) == default_size {
                // `weight="heavy"` (900) is the densest Pango stop;
                // falls back to bold on fonts without a heavy face,
                // but on fonts that have one it's distinctly chunkier
                // than `<b>`. Paired with `size="large"` (~+20%, the
                // equivalent of ~+2px on the slider's typical font)
                // for a visible-at-a-glance pop without overflowing
                // the tick row's vertical budget.
                format!("<span weight=\"heavy\" size=\"large\">{}</span>", letter)
            } else {
                letter.to_string()
            };
            slider.add_mark(pos, gtk::PositionType::Bottom, Some(&markup));
        }
    }
}

#[relm4::component(pub)]
impl Component for StyleToolbar {
    type Init = ();
    type Input = StyleToolbarInput;
    type Output = ToolbarEvent;
    type CommandOutput = ();

    view! {
        root = gtk::Box {
            set_orientation: gtk::Orientation::Horizontal,
            // 12 px sits between the size slider and the
            // tool-specific cluster when BOTH are visible. GTK
            // skips spacing for hidden children, so when one side
            // is hidden (Spotlight hides the size slider; Pointer
            // and Line/Marker hide the cluster) the natural width
            // tracks the visible content exactly. Trimmed from
            // 18 px to claw back a bit more of the bottom row so
            // the single-row layout survives at narrower windows.
            set_spacing: 12,
            // Center the toolbar vertically in the bottom row so the
            // slider's trough lines up with the visual midline (and the
            // compact buttons stay aligned to it). Was Align::End — that
            // pinned the whole toolbar to the bottom of the row, which
            // pushed the slider trough below center.
            set_valign: Align::Center,
            set_halign: Align::Center,
            add_css_class: "toolbar",
            add_css_class: "toolbar-bottom",

            // Crop is a focused, one-and-done mode; hide the entire
            // style toolbar while it's active so the bottom row
            // reduces to "zoom indicator + snap controls (left) /
            // Revert to Original (right)". Returning to a regular tool
            // brings the style toolbar back.
            #[watch]
            set_visible: model.visible && model.current_tool != Tools::Crop,

            // (A left mirror spacer used to live here to counter the
            // right cluster's reserved width when the annotation-size-
            // factor pill sat between the size slider and the right
            // cluster. With the pill gone, the size slider and the
            // tool-specific cluster sit together as a single visual
            // group and the parent CenterBox slot self-centers them —
            // no mirror needed.)

            // Size slider with detents at each step (XS, S, M, L, XL,
            // XXL). Replaces a row of six ToggleButtons — takes less
            // space and stays one widget wide regardless of which step
            // is active. `set_round_digits(0)` enforces integer snap so
            // dragging always lands on a labeled detent. `set_digits(0)`
            // hides any decimal places from the (unused) value readout.
            #[name = "size_slider"]
            gtk::Scale {
                add_css_class: "compact-slider",
                set_orientation: gtk::Orientation::Horizontal,
                set_focusable: false,
                set_hexpand: false,
                // Slimmer than the original 200 px so the whole
                // bottom row fits on a single line down to
                // narrower window widths before the wrap kicks
                // in. 140 px still spreads the six detents (XS …
                // XXL) far enough apart to drag precisely.
                set_width_request: 140,
                set_valign: gtk::Align::Center,
                // Hidden when the slider doesn't apply: Spotlight
                // (no `style.size`), a multi-selection with mixed
                // sizes (no single value to set), or Pointer with
                // nothing selected (the slider would change the
                // size of what, exactly?). With a selection the
                // Pointer-tool slider DOES do something — it
                // resizes the picked drawable — so it stays.
                #[watch]
                set_visible: !model.size_slider_disabled
                    && model.current_tool != Tools::Spotlight
                    && (model.current_tool != Tools::Pointer || model.has_selection),
                // GTK's valign:Center splits remaining space evenly above
                // and below the widget, but the slider's mark labels
                // hang below the trough — so the "visual center" (the
                // trough) ends up below the row midline. A 4 px bottom
                // margin shifts the centered widget up by 2 px so the
                // trough lines up with the compact buttons' midlines.
                set_margin_bottom: 4,
                set_range: (0.0, 5.0),
                set_increments: (1.0, 1.0),
                set_round_digits: 0,
                set_digits: 0,
                set_draw_value: false,
                // Tooltip is installed dynamically in `init` so the
                // text can flip between the with-selection and no-
                // selection variants (different wheel modifiers).
                // Marks are added imperatively in init() via
                // `refresh_size_slider_marks` so the letter for the
                // current tool's saved default can be bolded.
                #[watch]
                #[block_signal(size_changed)]
                set_value: size_to_slider_value(model.current_size),
                connect_value_changed[sender] => move |scale| {
                    let size = slider_value_to_size(scale.value());
                    sender.input(StyleToolbarInput::SizeChanged(size));
                } @size_changed,
            },
            // (The annotation-size-factor pill used to live here, with
            // an "x" label preceding it. It moved into Preferences
            // because the factor is fundamentally a one-time
            // display-scale calibration, not a per-stroke knob — the
            // toolbar surface was inviting confusion every time it
            // re-synced from a selection. Alt+scroll on the canvas
            // still nudges the live factor (sketch_board emits a
            // toast for feedback).)
            // (Output dimensions moved out of the center cluster into
            // `bottom_row.end_widget` so they live opposite the zoom
            // indicator and stay visible during Crop mode, where the
            // whole StyleToolbar is hidden. The Fill button moved
            // into the right cluster below as the tool-specific
            // control for Rectangle/Ellipse.)
            // Right cluster: every tool-specific control lives here.
            // Exactly one inner widget is visible at a time; the
            // leading label re-targets per tool via
            // `tool_cluster_label(current_tool)`. The cluster sizes
            // to its natural width — switching tools shifts the
            // centered group slightly, but the alternative (a fixed-
            // width slot) leaves invisible trailing space inside the
            // cluster on short-content tools and that broke the
            // "size slider + cluster as one centered group" feel the
            // bar needs after the mirror spacer was retired. Hidden
            // outright for tools that have no cluster content
            // (Pointer / Line / Marker / Crop) so an empty Box
            // doesn't contribute phantom width to the StyleToolbar's
            // natural-min — which would knock the bar's center off
            // the row midpoint under hexpand-spacer centering.
            gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,
                set_spacing: 6,
                set_halign: gtk::Align::Start,
                #[watch]
                set_visible: matches!(
                    model.current_tool,
                    Tools::Highlighter
                        | Tools::Arrow
                        | Tools::Blur
                        | Tools::Text
                        | Tools::Spotlight
                        | Tools::Rectangle
                        | Tools::Ellipse
                        | Tools::Brush
                ) || model.brush_smooth_slider_show_for_multi,

                // Highlighter style picker — placed BEFORE the
                // cluster label so the picker reads as the primary
                // choice and the "Opacity" label introduces the
                // slider that follows. Visible only while the
                // Highlighter tool is active; the other tools' style
                // pickers below remain after the label as before
                // (they don't have a secondary slider to introduce).
                #[name = "highlighter_style_menu"]
                gtk::MenuButton {
                    add_css_class: "compact-control",
                    set_focusable: false,
                    set_focus_on_click: false,
                    set_hexpand: false,
                    set_valign: gtk::Align::Center,
                    set_height_request: 34,
                    set_always_show_arrow: true,
                    #[watch]
                    set_visible: model.current_tool == Tools::Highlighter,
                    #[wrap(Some)]
                    set_child = &gtk::Image {
                        #[watch]
                        set_icon_name: Some(highlighter_style_icon(model.highlighter_style)),
                    },
                },

                gtk::Label {
                    add_css_class: "dim-label",
                    // Inline header for the chip-tool clusters
                    // (Arrow / Blur / Text / Rectangle / Ellipse).
                    // Slider-tool clusters (Spotlight / Highlighter
                    // / Brush) now carry their label as a mark
                    // below the slider trough — same visual rhythm
                    // as the size slider's XS–XXL letters — so the
                    // inline label collapses for those to keep both
                    // sliders at the same height.
                    #[watch]
                    set_label: tool_cluster_label(model.current_tool),
                    #[watch]
                    set_visible: matches!(
                        model.current_tool,
                        Tools::Arrow
                            | Tools::Blur
                            | Tools::Text
                            | Tools::Rectangle
                            | Tools::Ellipse
                    ),
                },

                // Arrow geometry picker. MenuButton + popover of
                // preview+label rows. The leading widget and tooltip are
                // wired imperatively in init() so they can re-render
                // (DrawingArea queue_draw) and re-word (tooltip label)
                // when `model.arrow_style` changes.
                #[name = "arrow_style_menu"]
                gtk::MenuButton {
                    add_css_class: "compact-control",
                    set_focusable: false,
                    set_focus_on_click: false,
                    set_hexpand: false,
                    set_valign: gtk::Align::Center,
                    set_height_request: 34,
                    set_always_show_arrow: true,
                    #[watch]
                    set_visible: model.current_tool == Tools::Arrow,
                },

                // Blur algorithm picker, same shape as the arrow menu.
                // Leading icon + tooltip are installed in init() so the
                // tooltip text can name the active variant.
                #[name = "blur_style_menu"]
                gtk::MenuButton {
                    add_css_class: "compact-control",
                    set_focusable: false,
                    set_focus_on_click: false,
                    set_hexpand: false,
                    set_valign: gtk::Align::Center,
                    set_height_request: 34,
                    set_always_show_arrow: true,
                    #[watch]
                    set_visible: model.current_tool == Tools::Blur,
                    #[wrap(Some)]
                    set_child = &gtk::Image {
                        #[watch]
                        set_icon_name: Some(blur_style_icon(model.blur_style)),
                    },
                },

                #[name = "text_background_dropdown"]
                gtk::DropDown {
                    add_css_class: "compact-control",
                    set_focusable: false,
                    set_focus_on_click: false,
                    set_hexpand: false,
                    set_valign: gtk::Align::Center,
                    set_height_request: 34,
                    set_model: Some(&gtk::StringList::new(&["Rounded", "Plain"])),
                    install_tooltip_above: "Text background",
                    #[watch]
                    set_visible: model.current_tool == Tools::Text,
                    connect_selected_notify[sender, text_background_silent]
                        => move |dropdown| {
                        // Silent flag: when the selection-sync path
                        // programmatically flips `selected`, this
                        // notify still fires — short-circuit so the
                        // sync stays a pure UI update (no toast, no
                        // re-apply on the already-applied drawable).
                        if text_background_silent.get() {
                            return;
                        }
                        let bg = match dropdown.selected() {
                            0 => crate::tools::TextBackground::Rounded,
                            1 => crate::tools::TextBackground::Plain,
                            _ => return,
                        };
                        sender.output_sender().emit(ToolbarEvent::TextBackgroundSelected(bg));
                        sender.output_sender().emit(ToolbarEvent::FocusCanvas);
                    },
                },

                // Fill Shape button — visible only for tools that
                // actually honor fill (Rectangle / Ellipse). Tooltip
                // reflects the current state so hovering the icon
                // tells the user what they're about to leave behind.
                // We use the custom hover-tooltip system (750 ms
                // delay) wired up in init() rather than GTK's built-in
                // `set_tooltip_text` (which only appears after the
                // window-manager delay and never matches our toolbar's
                // styling).
                #[name = "fill_button"]
                gtk::Button {
                    add_css_class: "compact-control",
                    set_focusable: false,
                    set_hexpand: false,
                    set_valign: gtk::Align::Center,
                    set_height_request: 34,
                    #[watch]
                    set_visible: matches!(
                        model.current_tool,
                        Tools::Rectangle | Tools::Ellipse
                    ),
                    #[watch]
                    set_icon_name: if model.fill_shapes {
                        "paint-bucket-filled"
                    } else {
                        "paint-bucket-regular"
                    },
                    connect_clicked => StyleToolbarInput::ToggleFill,
                },

                #[name(spotlight_slider)]
                gtk::Scale {
                    add_css_class: "compact-slider",
                    set_orientation: gtk::Orientation::Horizontal,
                    set_focusable: false,
                    set_hexpand: false,
                    set_width_request: CLUSTER_SLIDER_WIDTH,
                    set_valign: gtk::Align::Center,
                    set_range: (0.10, 0.90),
                    set_increments: (0.01, 0.10),
                    set_draw_value: false,
                    #[watch]
                    #[block_signal(spotlight_value_changed)]
                    set_value: model.spotlight_darkness as f64,
                    // "Darkness" rides below the trough at the
                    // midpoint mark — same below-trough rhythm as
                    // the size slider's letter labels, so both
                    // sliders end up the same height when the
                    // cluster swaps between them.
                    add_mark: (0.50, gtk::PositionType::Bottom, Some("Darkness")),
                    #[watch]
                    set_visible: model.current_tool == Tools::Spotlight,
                    connect_value_changed[sender] => move |scale| {
                        // Detent: snap to 0.50 within ±0.025 so the
                        // user can land on the default without
                        // pixel-precise dragging. set_value with the
                        // already-displayed value is a no-op signal-
                        // wise, so no recursion.
                        let mut v = scale.value() as f32;
                        if (v - 0.50).abs() < 0.025 {
                            v = 0.50;
                            scale.set_value(0.50);
                        }
                        // Keep the model field in sync with the live
                        // slider value. Without this, the `#[watch]` on
                        // `set_value: model.spotlight_darkness` re-applies
                        // the stale model value on the next model update
                        // (e.g., when `current_tool` changes during a
                        // tool switch), snapping the slider back to its
                        // pre-drag value when the user returns to
                        // Spotlight. The block_signal stops the watch
                        // re-application from re-firing this handler.
                        sender.input_sender().emit(StyleToolbarInput::SetSpotlightDarkness(v));
                        sender.output_sender().emit(ToolbarEvent::SpotlightDarknessChanged(v));
                    } @spotlight_value_changed,
                },

                #[name(highlighter_slider)]
                gtk::Scale {
                    add_css_class: "compact-slider",
                    set_orientation: gtk::Orientation::Horizontal,
                    set_focusable: false,
                    set_hexpand: false,
                    set_width_request: CLUSTER_SLIDER_WIDTH,
                    set_valign: gtk::Align::Center,
                    set_range: (0.10, 1.00),
                    set_increments: (0.01, 0.10),
                    set_draw_value: false,
                    #[watch]
                    #[block_signal(highlighter_value_changed)]
                    set_value: model.highlighter_opacity as f64,
                    // "Opacity" label sits below the midpoint —
                    // see the spotlight slider's note for the
                    // shared-height rationale.
                    add_mark: (0.50, gtk::PositionType::Bottom, Some("Opacity")),
                    #[watch]
                    set_visible: model.current_tool == Tools::Highlighter,
                    connect_value_changed[sender] => move |scale| {
                        let v = scale.value() as f32;
                        sender.output_sender().emit(ToolbarEvent::HighlighterOpacityChanged(v));
                    } @highlighter_value_changed,
                },

                // Brush post-stroke smoothing slider — only visible
                // while the Brush tool is active. Integer step 0..=6.
                // 0 disables smoothing entirely (raw polyline as the
                // user drew it). 1–2 are pure Chaikin corner-cutting.
                // 3+ adds Ramer–Douglas–Peucker simplification (with
                // tolerance scaling per level) before Chaikin's, so
                // the upper half of the range produces genuinely
                // progressive smoothing. Capped at 6 (RDP @ 9px) —
                // higher tolerances collapse strokes too aggressively
                // to be useful for the annotation use case.
                // Detent mark at 2 matches the built-in default.
                #[name(brush_smooth_slider)]
                gtk::Scale {
                    add_css_class: "compact-slider",
                    set_orientation: gtk::Orientation::Horizontal,
                    set_focusable: false,
                    set_hexpand: false,
                    set_width_request: CLUSTER_SLIDER_WIDTH,
                    set_valign: gtk::Align::Center,
                    set_range: (0.0, 6.0),
                    set_increments: (1.0, 1.0),
                    set_round_digits: 0,
                    set_draw_value: false,
                    #[watch]
                    #[block_signal(brush_smooth_value_changed)]
                    set_value: model.brush_post_smooth_iterations as f64,
                    // The single tick mark moves to the saved-default
                    // position (see `refresh_brush_smooth_slider_marks`)
                    // so the user sees clear feedback when they save a
                    // new default. No static mark here.
                    // Visible whenever brush smoothness is meaningful:
                    // either the active tool is Brush (new-stroke
                    // path) or a Pointer-tool multi-selection has
                    // brush strokes (group-edit path). The
                    // multi-flag is gated on "all selected are
                    // brushes" so we don't show the slider for a
                    // mixed selection that includes arrows / text.
                    // Hide outright when a multi-selection's
                    // smoothness levels disagree — same rationale
                    // as the size slider's mixed-multi hide above.
                    #[watch]
                    set_visible: (model.current_tool == Tools::Brush
                        || model.brush_smooth_slider_show_for_multi)
                        && !model.brush_smooth_slider_disabled,
                    connect_value_changed[sender] => move |scale| {
                        let v = scale.value().round().clamp(0.0, 6.0) as usize;
                        sender.output_sender().emit(ToolbarEvent::BrushPostSmoothChanged(v));
                    } @brush_smooth_value_changed,
                },
            },
            // ("Revert to Original" lives in `bottom_row.end_widget`
            // — outside the StyleToolbar — so its visibility toggling
            // doesn't shift the centered toolbar's width.)
        },
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match message {
            StyleToolbarInput::SaveSizeAsDefault => {
                // Persist the live size as the default for THE
                // CURRENT TOOL only — different tools each get their
                // own saved default. Pointer / Crop don't use a size,
                // so saving while they're active is a no-op (the
                // size slider isn't even visible then, but guard
                // anyway in case the slider lingers).
                if !matches!(self.current_tool, Tools::Pointer | Tools::Crop) {
                    crate::state::save_size_for_tool(self.current_tool, self.current_size);
                    // Re-bold the now-default letter on the slider so
                    // the user gets immediate visual confirmation that
                    // the save took effect (no UI lag between the
                    // popover click and the bold update).
                    self.refresh_size_slider_marks();
                }
            }

            StyleToolbarInput::SetVisibility(visible) => self.visible = visible,
            StyleToolbarInput::ToggleVisibility => {
                self.visible = !self.visible;
            }
            StyleToolbarInput::ToolChanged(tool) => {
                self.current_tool = tool;
                // Per-tool size default: when switching tools, snap
                // the size slider to the new tool's saved default
                // (if the user has saved one). Pointer / Crop don't
                // own a meaningful "size" — leave the slider where
                // it was so coming back to a drawing tool isn't
                // disorienting.
                //
                // Crucially, skip when there's a live selection:
                // the auto-tool-switch path fires `ToolChanged` after
                // `SyncFromSelection` / `SyncMultiAgreement` have
                // already set the slider to the selected drawables'
                // size, and emitting `SizeSelected` here would cascade
                // through `dispatch_style_change` and rewrite the
                // selection back to the tool default — silently undoing
                // any size edit the user just made.
                if !self.has_selection
                    && !matches!(tool, Tools::Pointer | Tools::Crop)
                    && let Some(default_size) = self.effective_size_for_tool(tool)
                    && default_size != self.current_size
                {
                    self.current_size = default_size;
                    sender
                        .output_sender()
                        .emit(ToolbarEvent::SizeSelected(default_size));
                }
                // Bold the letter matching the new tool's saved
                // default (or none, if Pointer / Crop, or if the user
                // hasn't saved a default for this tool yet).
                self.refresh_size_slider_marks();
            }
            StyleToolbarInput::SetCurrentSize(size) => {
                // Mirror sketch_board's tool-size change into the
                // slider without re-broadcasting — sketch_board has
                // already applied the value via dispatch_style_change.
                self.current_size = size;
                // This input is only fired from the canvas-side
                // wheel-resize path (Ctrl+wheel with no selection),
                // which is by definition a user-driven adjustment to
                // the active tool's next-stroke size — record it so
                // the sticky-defaults pref preserves it on tool
                // switch. (Selection-driven size changes go through
                // SyncFromSelection / SyncMultiAgreement, not here.)
                if !matches!(self.current_tool, Tools::Pointer | Tools::Crop) {
                    self.session_size_per_tool.insert(self.current_tool, size);
                }
            }
            StyleToolbarInput::SyncToToolDefault => {
                // Fired by main.rs on deselect — slide back to the
                // active tool's saved default. Same fall-through
                // rules as ToolChanged: skip Pointer/Crop, and only
                // act if the user has actually saved a default for
                // this tool.
                if !matches!(self.current_tool, Tools::Pointer | Tools::Crop)
                    && let Some(default_size) = self.effective_size_for_tool(self.current_tool)
                    && default_size != self.current_size
                {
                    self.current_size = default_size;
                    sender
                        .output_sender()
                        .emit(ToolbarEvent::SizeSelected(default_size));
                }
                // Empty selection — re-enable sliders so the user
                // can adjust the next-stroke defaults, and drop the
                // multi-show flag so the smoothness slider returns
                // to its usual `current_tool == Brush` visibility.
                self.size_slider_disabled = false;
                self.brush_smooth_slider_disabled = false;
                self.brush_smooth_slider_show_for_multi = false;
                self.has_selection = false;
                // No selection → wheel-resize requires Ctrl;
                // tooltip says so.
                if let Some(label) = &self.size_tooltip_label {
                    label.set_label(size_tooltip_text(false));
                }
            }
            StyleToolbarInput::CropPresenceChanged(present) => {
                self.has_crop = present;
            }
            StyleToolbarInput::SizeChanged(size) => {
                self.current_size = size;
                // Remember the in-session size for this tool so
                // `effective_size_for_tool` (used by ToolChanged /
                // SyncToToolDefault) can prefer it over the saved
                // default when `sticky_session_defaults` is on.
                // Recorded unconditionally — costs nothing while the
                // pref is off, and lets toggling the pref mid-session
                // pick up the user's already-made adjustments.
                if !matches!(self.current_tool, Tools::Pointer | Tools::Crop) {
                    self.session_size_per_tool.insert(self.current_tool, size);
                }
                sender
                    .output_sender()
                    .emit(ToolbarEvent::SizeSelected(size));
                sender.output_sender().emit(ToolbarEvent::FocusCanvas);
            }
            StyleToolbarInput::SyncFromSelection(style) => {
                // Reflect the selected shape in the toolbar widgets
                // without re-broadcasting — pushing `SizeSelected`
                // back to sketch_board would feedback into the same
                // selection and clobber its other style fields.
                self.current_size = style.size;
                self.fill_shapes = style.fill;
                if let Some(label) = &self.fill_tooltip_label {
                    label.set_text(fill_tooltip_text(self.fill_shapes));
                }
                // Single selection has by definition one value for
                // each property — re-enable any slider that a prior
                // multi-select had disabled, and drop the multi-show
                // flag so the smoothness slider falls back to its
                // usual `current_tool == Brush` visibility rule.
                self.size_slider_disabled = false;
                self.brush_smooth_slider_disabled = false;
                self.brush_smooth_slider_show_for_multi = false;
                self.has_selection = true;
                // Selection is active → wheel resizes it; tooltip
                // points at the unmodified-wheel gesture.
                if let Some(label) = &self.size_tooltip_label {
                    label.set_label(size_tooltip_text(true));
                }
            }
            StyleToolbarInput::SyncMultiAgreement { size, smooth } => {
                // Size: `Some(v)` → reflect on slider + enable it (so
                // a slider drag will group-update); `None` → disable
                // so a stray drag can't collapse a mixed selection.
                // The matching `set_sensitive` watch in view!
                // consumes the flag.
                match size {
                    Some(s) => {
                        self.current_size = s;
                        self.size_slider_disabled = false;
                    }
                    None => self.size_slider_disabled = true,
                }
                // Smoothness has three states: hide entirely when
                // selection isn't all-brush (NotApplicable); show
                // disabled when all brushes but levels differ
                // (Mixed); show enabled at the shared value when
                // they agree (Shared). Drives both `set_visible` and
                // `set_sensitive` on the slider.
                use crate::sketch_board::SmoothLevelMulti;
                match smooth {
                    SmoothLevelMulti::NotApplicable => {
                        self.brush_smooth_slider_show_for_multi = false;
                        self.brush_smooth_slider_disabled = false;
                    }
                    SmoothLevelMulti::Shared(level) => {
                        self.brush_post_smooth_iterations = level;
                        self.brush_smooth_slider_show_for_multi = true;
                        self.brush_smooth_slider_disabled = false;
                    }
                    SmoothLevelMulti::Mixed => {
                        self.brush_smooth_slider_show_for_multi = true;
                        self.brush_smooth_slider_disabled = true;
                    }
                }
                self.has_selection = true;
                // Multi-selection counts as "selection active" for
                // tooltip purposes — wheel still resizes the group.
                if let Some(label) = &self.size_tooltip_label {
                    label.set_label(size_tooltip_text(true));
                }
            }
            StyleToolbarInput::ToggleFill => {
                // Flip local state so the icon refreshes via #[watch],
                // and broadcast upstream so sketch_board applies the
                // new fill flag to current style + any selection.
                self.fill_shapes = !self.fill_shapes;
                if let Some(label) = &self.fill_tooltip_label {
                    label.set_text(fill_tooltip_text(self.fill_shapes));
                }
                sender.output_sender().emit(ToolbarEvent::ToggleFill);
                sender.output_sender().emit(ToolbarEvent::FocusCanvas);
            }
            StyleToolbarInput::SetFillShapes(fill) => {
                // Mirror sketch_board's flipped `style.fill` (driven
                // by the `F` keyboard shortcut) without broadcasting
                // back upstream — sketch_board has already applied
                // the change everywhere it needs to land.
                self.fill_shapes = fill;
                if let Some(label) = &self.fill_tooltip_label {
                    label.set_text(fill_tooltip_text(self.fill_shapes));
                }
            }
            StyleToolbarInput::SetBlurStyle {
                style,
                emit_upstream,
            } => {
                // Local mirror drives the MenuButton's `#[watch]`ed icon;
                // tooltip wording refreshes either way. Upstream emit
                // skipped on the selection-sync path so the same value
                // isn't redundantly re-applied + toasted.
                self.blur_style = style;
                if let Some(label) = &self.blur_style_tooltip_label {
                    label.set_text(&blur_tooltip_text(style));
                }
                if emit_upstream {
                    sender
                        .output_sender()
                        .emit(ToolbarEvent::BlurStyleSelected(style));
                    sender.output_sender().emit(ToolbarEvent::FocusCanvas);
                }
            }
            StyleToolbarInput::SetArrowStyle {
                style,
                emit_upstream,
            } => {
                self.arrow_style = style;
                // Flip the MenuButton's preview to the new shape.
                if let Some(cell) = &self.arrow_preview_cell {
                    cell.set(style);
                }
                if let Some(area) = &self.arrow_preview_area {
                    area.queue_draw();
                }
                if let Some(label) = &self.arrow_style_tooltip_label {
                    label.set_text(&arrow_tooltip_text(style));
                }
                if emit_upstream {
                    sender
                        .output_sender()
                        .emit(ToolbarEvent::ArrowStyleSelected(style));
                    sender.output_sender().emit(ToolbarEvent::FocusCanvas);
                }
            }
            StyleToolbarInput::SetHighlighterStyle(style) => {
                self.highlighter_style = style;
                if let Some(label) = &self.highlighter_style_tooltip_label {
                    label.set_text(&highlighter_tooltip_text(style));
                }
                sender
                    .output_sender()
                    .emit(ToolbarEvent::HighlighterStyleSelected(style));
                sender.output_sender().emit(ToolbarEvent::FocusCanvas);
            }
            StyleToolbarInput::SetTextBackground { bg, emit_upstream } => {
                // Match the popover-list / init mapping for the
                // dropdown's index. The notify-handler in init fires
                // `TextBackgroundSelected` upstream on every selection
                // change — we suppress it via the silent flag on the
                // selection-sync path so the just-clicked drawable
                // isn't redundantly re-styled or toasted.
                if let Some(dd) = &self.text_background_dropdown {
                    let idx = match bg {
                        crate::tools::TextBackground::Rounded => 0,
                        crate::tools::TextBackground::Plain => 1,
                    };
                    if dd.selected() != idx {
                        if !emit_upstream {
                            self.text_background_silent.set(true);
                        }
                        dd.set_selected(idx);
                        if !emit_upstream {
                            self.text_background_silent.set(false);
                        }
                    }
                }
            }
            StyleToolbarInput::SetSpotlightDarkness(value) => {
                // Slider widget's `set_value` is `#[watch]`ed on this
                // field with the upstream signal blocked, so the
                // assignment alone drives the snapback.
                self.spotlight_darkness = value;
            }
            StyleToolbarInput::SetHighlighterOpacity(value) => {
                self.highlighter_opacity = value;
            }
            StyleToolbarInput::SetBrushPostSmooth(value) => {
                // Same pattern as the spotlight / highlighter sliders:
                // the `#[watch]`ed value drives the visible position
                // with the upstream connect_value_changed signal
                // suppressed, so we don't bounce back to sketch_board.
                self.brush_post_smooth_iterations = value;
            }
            StyleToolbarInput::RefreshBrushSmoothMarks => {
                self.refresh_brush_smooth_slider_marks();
            }
        }
    }

    fn init(
        _: Self::Init,
        _root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        // (Size selection is now driven by `current_size` + the
        // SizeChanged input message — see the `gtk::Scale` in the
        // view! block. The old `SizeAction` for the 6-button radio
        // bank is no longer needed.)

        // Captured by the text-background DropDown's `connect_selected_notify`
        // so the selection-sync path (`SetTextBackground { emit_upstream: false }`)
        // can suppress the upstream emit. Lives outside the model field
        // because relm4's view! macro captures plain identifiers from
        // the enclosing scope, not model paths.
        let text_background_silent = std::rc::Rc::new(std::cell::Cell::new(false));

        // create model
        let initial_tool = APP_CONFIG.read().initial_tool();
        let initial_size = crate::state::load_size_for_tool(initial_tool)
            .or_else(|| initial_tool.builtin_default_size())
            .unwrap_or_default();
        let mut model = StyleToolbar {
            visible: !APP_CONFIG.read().default_hide_toolbars(),
            current_tool: initial_tool,
            spotlight_darkness: crate::state::load_spotlight_darkness().unwrap_or(0.50),
            highlighter_opacity: crate::state::load_highlighter_opacity().unwrap_or(0.40),
            brush_post_smooth_iterations: crate::state::load_brush_post_smooth_iterations()
                .unwrap_or_else(|| APP_CONFIG.read().brush_post_smooth_iterations()),
            size_slider_disabled: false,
            has_selection: false,
            size_tooltip_label: None,
            brush_smooth_slider_disabled: false,
            brush_smooth_slider_show_for_multi: false,
            size_slider: None,
            brush_smooth_slider: None,
            has_crop: false,
            current_size: initial_size,
            fill_shapes: APP_CONFIG.read().default_fill_shapes(),
            fill_tooltip_label: None,
            blur_style: crate::state::load_blur_style().unwrap_or_default(),
            blur_style_popover: None,
            arrow_style: crate::state::load_arrow_style().unwrap_or_default(),
            arrow_style_popover: None,
            arrow_preview_area: None,
            arrow_preview_cell: None,
            arrow_style_tooltip_label: None,
            blur_style_tooltip_label: None,
            text_background_dropdown: None,
            text_background_silent: text_background_silent.clone(),
            highlighter_style: crate::state::load_highlighter_style().unwrap_or_default(),
            highlighter_style_popover: None,
            highlighter_style_tooltip_label: None,
            session_size_per_tool: HashMap::new(),
        };

        // create widgets
        let widgets = view_output!();

        // Build the arrow- and blur-style popovers programmatically.
        // relm4's view! macro can't iterate enum variants, and we want
        // row order to stay anchored to a single source of truth.
        // Arrow rows show a cairo-drawn miniature of the actual arrow
        // shape; blur rows still use icons (the algorithms don't have
        // a distinctive silhouette to preview).
        model.arrow_style_popover = Some(build_style_popover(
            &widgets.arrow_style_menu,
            &sender,
            &[
                ArrowStyle::Standard,
                ArrowStyle::Pointy,
                ArrowStyle::Curved,
                ArrowStyle::Double,
            ],
            |s| {
                make_arrow_preview(s, ARROW_ROW_PREVIEW_W, ARROW_ROW_PREVIEW_H)
                    .0
                    .upcast::<gtk::Widget>()
            },
            arrow_style_label,
            |s| StyleToolbarInput::SetArrowStyle {
                style: s,
                emit_upstream: true,
            },
        ));
        model.blur_style_popover = Some(build_style_popover(
            &widgets.blur_style_menu,
            &sender,
            &[
                BlurStyle::Pixelate,
                BlurStyle::SecureBlur,
                BlurStyle::Gaussian,
                BlurStyle::BlackOut,
            ],
            |s| gtk::Image::from_icon_name(blur_style_icon(s)).upcast::<gtk::Widget>(),
            blur_style_label,
            |s| StyleToolbarInput::SetBlurStyle {
                style: s,
                emit_upstream: true,
            },
        ));
        model.highlighter_style_popover = Some(build_style_popover(
            &widgets.highlighter_style_menu,
            &sender,
            &[
                crate::tools::HighlighterStyle::TextLocked,
                crate::tools::HighlighterStyle::Normal,
            ],
            |s| gtk::Image::from_icon_name(highlighter_style_icon(s)).upcast::<gtk::Widget>(),
            highlighter_style_label,
            StyleToolbarInput::SetHighlighterStyle,
        ));

        // Build the arrow MenuButton's leading preview — same drawing
        // code as the popover rows, with a Cell driving which variant
        // it renders so we can flip it on SetArrowStyle without
        // rebuilding the widget.
        let (arrow_preview, arrow_cell) =
            make_arrow_preview(model.arrow_style, ARROW_PREVIEW_W, ARROW_PREVIEW_H);
        widgets.arrow_style_menu.set_child(Some(&arrow_preview));
        model.arrow_preview_area = Some(arrow_preview);
        model.arrow_preview_cell = Some(arrow_cell);

        // Custom hover-tooltips on both style MenuButtons. The wording
        // names the *active* variant; SetArrowStyle / SetBlurStyle (and
        // their handlers) refresh the label so a second hover reflects
        // the new selection.
        // Size-slider tooltip starts in the "no selection" variant —
        // that's the initial state and the most common cold-start.
        let size_tooltip = install_dynamic_tooltip(
            &widgets.size_slider,
            size_tooltip_text(false),
            gtk::PositionType::Top,
        );
        model.size_tooltip_label = Some(size_tooltip);
        let arrow_tooltip = install_dynamic_tooltip(
            &widgets.arrow_style_menu,
            &arrow_tooltip_text(model.arrow_style),
            gtk::PositionType::Top,
        );
        model.arrow_style_tooltip_label = Some(arrow_tooltip);
        let blur_tooltip = install_dynamic_tooltip(
            &widgets.blur_style_menu,
            &blur_tooltip_text(model.blur_style),
            gtk::PositionType::Top,
        );
        model.blur_style_tooltip_label = Some(blur_tooltip);
        let highlighter_tooltip = install_dynamic_tooltip(
            &widgets.highlighter_style_menu,
            &highlighter_tooltip_text(model.highlighter_style),
            gtk::PositionType::Top,
        );
        model.highlighter_style_tooltip_label = Some(highlighter_tooltip);
        if let Some(bg) = crate::state::load_text_background() {
            let idx = match bg {
                crate::tools::TextBackground::Rounded => 0,
                crate::tools::TextBackground::Plain => 1,
            };
            widgets.text_background_dropdown.set_selected(idx);
        }
        // Stash the DropDown so `SetTextBackground` can drive its
        // selected index when sketch_board cycles the variant via the
        // double-tap shortcut. The view! macro only fires on widget
        // creation, so without this handle the update() arm has no
        // way to reach the dropdown.
        model.text_background_dropdown = Some(widgets.text_background_dropdown.clone());

        // Right-click → "Save as default" on the controls users
        // tweak in the central / right cluster. The size slider
        // persists through a StyleToolbar internal input (the toolbar
        // owns the live value); the opacity / darkness sliders' live
        // values live in sketch_board, so they go out as ToolbarEvents
        // and the sketch_board handler writes to state.toml.
        {
            let s = sender.clone();
            attach_save_default_popover(&widgets.size_slider, move || {
                s.input(StyleToolbarInput::SaveSizeAsDefault);
            });
        }
        {
            let s = sender.clone();
            attach_save_default_popover(&widgets.spotlight_slider, move || {
                s.output_sender()
                    .emit(ToolbarEvent::SaveSpotlightDarknessAsDefault);
            });
        }
        {
            let s = sender.clone();
            attach_save_default_popover(&widgets.highlighter_slider, move || {
                s.output_sender()
                    .emit(ToolbarEvent::SaveHighlighterOpacityAsDefault);
            });
        }
        {
            let s = sender.clone();
            let slider = widgets.brush_smooth_slider.clone();
            attach_save_default_popover(&widgets.brush_smooth_slider, move || {
                // Read the slider's CURRENT value rather than the local
                // model field — the field doesn't update on user drag
                // (the value_changed callback only emits upstream), so
                // it can lag the visible position.
                let v = slider.value().round().clamp(0.0, 6.0) as usize;
                s.output_sender()
                    .emit(ToolbarEvent::SaveBrushPostSmoothAsDefault(v));
                // Mirror the slider value into the local model field.
                // Necessary because the next input we emit
                // (RefreshBrushSmoothMarks) triggers a view-tree
                // re-evaluation, and the `#[watch]` `set_value:
                // model.brush_post_smooth_iterations` would otherwise
                // push the stale model value (still at whatever it was
                // before the user dragged) back to the slider, snapping
                // it visibly away from the user's intent.
                s.input(StyleToolbarInput::SetBrushPostSmooth(v));
                // Re-position the tick mark so the user gets immediate
                // visible confirmation (the mark jumps to the slider's
                // current position). The state.toml write the output
                // emit triggered is synchronous in `sketch_board`, so
                // by the time this input is processed
                // `load_brush_post_smooth_iterations` returns the
                // just-saved value.
                s.input(StyleToolbarInput::RefreshBrushSmoothMarks);
            });
        }
        {
            // Right-click the paint-bucket → "Save as default". Lets
            // the user pin the current fill state (filled / outline)
            // as the per-tool default for Rectangle or Ellipse,
            // matching the affordance the other tool-specific
            // controls offer. Left-click still toggles fill, since
            // `ToggleFill` is wired to `connect_clicked` upstream.
            let s = sender.clone();
            attach_save_default_popover(&widgets.fill_button, move || {
                s.output_sender().emit(ToolbarEvent::SaveFillAsDefault);
            });
        }

        // Attach the custom hover-tooltip to the Fill button (using
        // the same 750 ms-delay system the other toolbar buttons use)
        // and stash its inner Label so `ToggleFill` can update the
        // wording when the filled/outline state flips.
        let fill_label = install_dynamic_tooltip(
            &widgets.fill_button,
            fill_tooltip_text(model.fill_shapes),
            gtk::PositionType::Top,
        );
        model.fill_tooltip_label = Some(fill_label);

        // The color picker still uses RelmActions for its swatch row;
        // keep the group registered even though SizeAction was retired.
        let group = RelmActionGroup::<StyleToolbarActionGroup>::new();
        group.register_for_widget(&widgets.root);

        // Stash the size slider so the SaveSizeAsDefault / ToolChanged
        // handlers can re-render the mark labels with the saved-default
        // letter bolded. First refresh after the clone so the initial
        // paint matches the active tool's stored default.
        model.size_slider = Some(widgets.size_slider.clone());
        model.refresh_size_slider_marks();
        model.brush_smooth_slider = Some(widgets.brush_smooth_slider.clone());
        model.refresh_brush_smooth_slider_marks();

        ComponentParts { model, widgets }
    }
}
relm4::new_action_group!(ToolsToolbarActionGroup, "tools-toolbars");
relm4::new_stateful_action!(ToolsAction, ToolsToolbarActionGroup, "tools", Tools, Tools);

relm4::new_action_group!(StyleToolbarActionGroup, "style-toolbars");
relm4::new_stateful_action!(
    ColorAction,
    StyleToolbarActionGroup,
    "colors",
    ColorButtons,
    ColorButtons
);

impl Clone for ColorAction {
    fn clone(&self) -> Self {
        Self {}
    }
}

relm4::new_stateful_action!(SizeAction, StyleToolbarActionGroup, "sizes", Size, Size);

impl StaticVariantType for ColorButtons {
    fn static_variant_type() -> Cow<'static, VariantTy> {
        Cow::Borrowed(VariantTy::UINT64)
    }
}

impl ToVariant for ColorButtons {
    fn to_variant(&self) -> Variant {
        Variant::from(match *self {
            Self::Palette(i) => i,
            Self::Custom => u64::MAX,
            Self::CustomSaved(i) => CUSTOM_SAVED_OFFSET + i,
        })
    }
}

impl FromVariant for ColorButtons {
    fn from_variant(variant: &Variant) -> Option<Self> {
        <u64>::from_variant(variant).map(|v| match v {
            u64::MAX => Self::Custom,
            v if v >= CUSTOM_SAVED_OFFSET => Self::CustomSaved(v - CUSTOM_SAVED_OFFSET),
            v => Self::Palette(v),
        })
    }
}
