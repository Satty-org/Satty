use std::{
    borrow::Cow,
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    time::Duration,
};

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
    gtk::{Align, Window, gdk::RGBA, prelude::*},
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
    let pending_show: Rc<RefCell<Option<gtk::glib::SourceId>>> =
        Rc::new(RefCell::new(None));

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
    /// Persisted "saved custom colors" — rendered as filled swatches
    /// in the right column of the color picker popover. Each entry
    /// is addressable via `ColorButtons::CustomSaved(i)` and survives
    /// across launches via `crate::state`.
    custom_colors: Vec<Color>,
    color_action: SimpleAction,
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
    /// `gtk::Revealer` wrapping the inline picker panel. Stashed so
    /// `TogglePickerExpansion` can flip its `reveal_child` without
    /// walking the widget tree.
    picker_revealer: Option<gtk::Revealer>,
    /// The embedded `ColorChooserWidget` inside the inline picker
    /// panel. `AddCurrentPickerToCustoms` reads its `rgba` so the
    /// "+ Add to My Colors" button knows what to persist.
    picker_chooser: Option<gtk::ColorChooserWidget>,
    /// Handle to the bottom-row arrow button so the toggle handler
    /// can flip its icon between pan-end and pan-start without
    /// rebuilding the controls.
    picker_arrow_btn: Option<gtk::Button>,
    /// Color currently being dragged within the saved-custom column,
    /// captured at drag-begin. While set, `custom_colors` is rendered
    /// WITHOUT this entry (it's been temporarily pulled out so the
    /// remaining items shift up to fill the gap); the popover
    /// instead shows a `.color-slot-ghost` placeholder at
    /// `dragging_preview_slot`. `None` between drags.
    dragging_color: Option<Color>,
    /// Snapshot of `custom_colors` taken at drag-begin so a cancelled
    /// drag (drop outside the popover, Esc, etc.) can fully restore
    /// the pre-drag list. Both the pull-out and the ghost-position
    /// changes happen against the live `custom_colors`, so the
    /// snapshot is the only way back to the original order.
    pre_drag_snapshot: Option<Vec<Color>>,
    /// While a drag is in flight, the index in the *post-pullout*
    /// `custom_colors` list where the ghost placeholder is currently
    /// drawn — i.e. the slot the dragged color will land in if the
    /// user drops right now. Updated each time the pointer enters a
    /// new slot's drop area; rendered by `build_color_popover_grid`
    /// as a brighter outlined placeholder so the user sees where
    /// other swatches have shifted aside to make room.
    dragging_preview_slot: Option<usize>,
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
    fn sync_color_action(&self) {
        let palette = APP_CONFIG.read().color_palette().palette().to_vec();
        let button = palette
            .iter()
            .position(|c| *c == self.current_color)
            .map(|i| ColorButtons::Palette(i as u64))
            .or_else(|| {
                self.custom_colors
                    .iter()
                    .position(|c| *c == self.current_color)
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
                // Out-of-range indices shouldn't be reachable from the
                // UI (the swatch isn't rendered until the slot exists)
                // but if a stale action target ever fires after a
                // refresh, fall back to the legacy custom color rather
                // than panic.
                self.custom_colors
                    .get(n as usize)
                    .copied()
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

/// Handles returned by `build_color_popover` so the caller (`init`)
/// can stash everything it needs on the model — the popover itself,
/// the swatch-grid stack (for crossfade rebuilds), the inline picker's
/// revealer + chooser (for live color updates), and the bottom arrow
/// button (so the toggle handler can flip its icon).
struct ColorPopoverHandles {
    popover: gtk::Popover,
    swatch_stack: gtk::Stack,
    picker_revealer: gtk::Revealer,
    picker_chooser: gtk::ColorChooserWidget,
    arrow_btn: gtk::Button,
}

/// Build the popover that hangs off the unified color-picker MenuButton.
/// Layout:
///
///   ┌── popover ───────────────────────────┬── revealer ──┐
///   │ ┌── swatch_stack ─┐                   │ inline       │
///   │ │ swatches grid   │                   │ picker       │
///   │ └─────────────────┘                   │ panel        │
///   │ ┌── controls box ─┐                   │ (chooser +   │
///   │ │ [wheel]   [⇄]   │                   │ + Add to     │
///   │ └─────────────────┘                   │ My Colors)   │
///   └──────────────────────────────────────┴──────────────┘
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

    // Outer: horizontal box. Left = swatches+controls column. Right =
    // revealer for the inline picker.
    let outer = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(0)
        .build();
    outer.add_css_class("color-picker-content");

    // Left column. Vertical: swatch_stack + controls_row.
    let left = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(0)
        .build();
    left.add_css_class("color-picker-left");

    let swatch_stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(STACK_FADE_MS)
        .hhomogeneous(true)
        .vhomogeneous(true)
        .build();
    swatch_stack.add_css_class("swatches-area");
    let grid = build_color_popover_grid(model, sender, &popover);
    swatch_stack.add_named(&grid, Some("page-0"));
    swatch_stack.set_visible_child(&grid);
    left.append(&swatch_stack);

    // Bottom controls: color-wheel on the left, expand arrow on the
    // right. Both toggle the inline picker — the wheel as a visual
    // hint that the expanded view lets you mix any color, the arrow
    // as the explicit collapse/expand toggle (standard convention).
    let controls = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(0)
        .hexpand(true)
        .margin_start(16)
        .margin_end(16)
        .margin_top(0)
        .margin_bottom(12)
        .build();
    controls.add_css_class("color-picker-controls");

    let wheel_btn = gtk::Button::builder()
        .focusable(false)
        .focus_on_click(false)
        .hexpand(false)
        .icon_name("color-regular")
        .halign(gtk::Align::Start)
        .build();
    wheel_btn.add_css_class("flat");
    wheel_btn.add_css_class("color-wheel-button");
    attach_floating_swatch_tooltip(&wheel_btn, "Open color picker");
    let sender_for_wheel = sender.clone();
    wheel_btn.connect_clicked(move |_| {
        sender_for_wheel.input(ToolsToolbarInput::TogglePickerExpansion);
    });

    let spacer = gtk::Box::builder()
        .hexpand(true)
        .orientation(gtk::Orientation::Horizontal)
        .build();

    let arrow_btn = gtk::Button::builder()
        .focusable(false)
        .focus_on_click(false)
        .hexpand(false)
        .icon_name(if model.picker_expanded {
            "pan-start-symbolic"
        } else {
            "pan-end-symbolic"
        })
        .halign(gtk::Align::End)
        .build();
    arrow_btn.add_css_class("flat");
    arrow_btn.add_css_class("picker-expand-arrow");
    attach_floating_swatch_tooltip(&arrow_btn, "Expand picker");
    let sender_for_arrow = sender.clone();
    arrow_btn.connect_clicked(move |_| {
        sender_for_arrow.input(ToolsToolbarInput::TogglePickerExpansion);
    });

    controls.append(&wheel_btn);
    controls.append(&spacer);
    controls.append(&arrow_btn);
    left.append(&controls);

    outer.append(&left);

    // Right side: revealer holding the inline picker panel. The
    // chooser inside is built once and kept alive — its in-progress
    // state (saturation/value cursor, hex entry text) needs to
    // survive across saved-customs rebuilds.
    let (picker_revealer, picker_chooser) = build_inline_picker_panel(model, sender);
    outer.append(&picker_revealer);

    popover.set_child(Some(&outer));
    ColorPopoverHandles {
        popover,
        swatch_stack,
        picker_revealer,
        picker_chooser,
        arrow_btn,
    }
}

/// Build the inline color-picker panel — a `ColorChooserWidget` in
/// editor mode plus a "+ Add to My Colors" button below — wrapped in a
/// `Revealer` that slides in from the right when the user clicks the
/// expand arrow / wheel button. The chooser broadcasts color changes
/// live via `InlinePickerColorChanged` so the picked color is applied
/// to subsequent annotations without a separate "Apply" step.
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

    let panel = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        // Tight on the leading edge so the picker hugs the swatches
        // column instead of floating in dead space. Trailing/top/bottom
        // keep modest breathing room.
        .margin_start(8)
        .margin_end(12)
        .margin_top(12)
        .margin_bottom(12)
        .build();
    panel.add_css_class("inline-picker-panel");

    let chooser = gtk::ColorChooserWidget::new();
    chooser.set_use_alpha(true);
    chooser.set_rgba(&RGBA::from(model.current_color));
    // Skip the palette grid built into ColorChooserWidget — the
    // popover's left column already serves that role. The editor
    // (saturation/value, hue, alpha, hex/RGB) is the new value-add.
    chooser.set_show_editor(true);
    // Constrain to a compact natural size so the saturation/value
    // square + hue/alpha sliders don't tower over the swatches
    // column. With hexpand/vexpand off the chooser uses its natural
    // size, which the CSS rules clamp via `min-*` on the inner
    // `colorplane` / `colorscale` nodes.
    chooser.set_hexpand(false);
    chooser.set_vexpand(false);
    chooser.set_halign(gtk::Align::Fill);
    chooser.set_valign(gtk::Align::Start);

    // Broadcast color changes live. The chooser fires `notify::rgba`
    // on every cursor movement — forward as `InlinePickerColorChanged`
    // so the active drawing color tracks what the user is mixing.
    let sender_for_chooser = sender.clone();
    chooser.connect_rgba_notify(move |c| {
        let color = Color::from_gdk(c.rgba());
        sender_for_chooser.input(ToolsToolbarInput::InlinePickerColorChanged(color));
    });

    panel.append(&chooser);

    let add_btn = gtk::Button::with_label("+ Add to My Colors");
    add_btn.add_css_class("suggested-action");
    add_btn.add_css_class("add-to-my-colors-btn");
    add_btn.set_focusable(false);
    add_btn.set_focus_on_click(false);
    // Don't stretch the button to the chooser's full width — it reads
    // as a giant slab below the gradient. Centered + natural width is
    // tighter and matches convention's compact CTA.
    add_btn.set_halign(gtk::Align::Center);
    add_btn.set_hexpand(false);
    let chooser_for_add = chooser.clone();
    let sender_for_add = sender.clone();
    add_btn.connect_clicked(move |_| {
        let color = Color::from_gdk(chooser_for_add.rgba());
        sender_for_add.input(ToolsToolbarInput::SaveCustomColor(color));
    });
    panel.append(&add_btn);

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
    right_click.connect_pressed(move |_g, _n, x, y| {
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
    let pending_show: Rc<RefCell<Option<gtk::glib::SourceId>>> =
        Rc::new(RefCell::new(None));

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
    // picker breathes more easily than the default
    // Adwaita popover chrome. The earlier 8-px margins felt cramped
    // once the saved-custom column landed (right edge sat right
    // against the dashed placeholders); 16-px outer margins + larger
    // inter-swatch gaps restore the airy look the user expected.
    let grid = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(14)
        .margin_start(16)
        .margin_end(16)
        .margin_top(14)
        .margin_bottom(14)
        .build();

    // Per-swatch tooltips are attached via `attach_floating_swatch_tooltip`
    // below. See its docstring for why we use a custom shared popover
    // parented to the top-level window rather than GTK's tooltip system.

    // Left column: 10 palette swatches, one per row, with shortcut
    // keys 1..9, 0 mapped to indexes 0..9.
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

    // Right column(s): persisted saved-custom colors, then dashed
    // placeholders filling out the rest of each column. Every slot
    // — filled or empty — accepts a drop, so colors can be dragged
    // into any position including past the current end of the list.
    //
    // During an in-flight drag, the dragged color has already been
    // pulled out of `custom_colors` by `BeginCustomDrag` and the
    // ghost preview slot at `dragging_preview_slot` is rendered as
    // an outlined placeholder. Subsequent items shift down by one
    // visual position around the ghost so the user sees exactly
    // where the swatch will land.
    let saved = &model.custom_colors;
    let dragging = model.dragging_color.is_some();
    // Visual length: when dragging we have the truncated list plus
    // one ghost slot (so the column doesn't visibly shrink while the
    // user is mid-drag).
    let visual_len = if dragging { saved.len() + 1 } else { saved.len() };
    let n_custom_cols = visual_len.div_ceil(SLOTS_PER_COLUMN).max(1);
    let total_slots = n_custom_cols * SLOTS_PER_COLUMN;
    let ghost_at = model.dragging_preview_slot.unwrap_or(usize::MAX);
    for visual_slot in 0..total_slots {
        let col_idx = visual_slot / SLOTS_PER_COLUMN;
        let row_idx = visual_slot % SLOTS_PER_COLUMN;
        let grid_col = (1 + col_idx) as i32;

        // Map `visual_slot` back to either a `saved` index, the
        // ghost, or an empty trailing placeholder.
        let (widget, tooltip) = if dragging && visual_slot == ghost_at {
            // Outlined ghost placeholder — drop here on release.
            (build_ghost_placeholder(), None)
        } else {
            // Adjust for the ghost shifting subsequent items by one.
            let saved_idx = if dragging && visual_slot > ghost_at {
                visual_slot - 1
            } else {
                visual_slot
            };
            if let Some(color) = saved.get(saved_idx).copied() {
                let selected = color == model.current_color;
                let w = build_saved_custom_swatch(color, saved_idx, selected, sender);
                let t = match color.name() {
                    Some(name) => format!("{name} (saved {})", saved_idx + 1),
                    None => format!("Saved color {}", saved_idx + 1),
                };
                (w, Some(t))
            } else {
                (build_dashed_placeholder(), None)
            }
        };
        if let Some(t) = tooltip {
            attach_floating_swatch_tooltip(&widget, &t);
        }
        // Every slot — filled, ghost, or empty — accepts a drop so
        // the user can drag through any position. The drop target's
        // closure uses `visual_slot` (not `saved_idx`) so the ghost
        // preview tracks the pointer position directly.
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
    annotation_size: f32,
    annotation_size_formatted: String,
    annotation_dialog_controller: Option<Controller<AnnotationSizeDialog>>,
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
    /// True iff a crop region currently exists (in either edit or
    /// committed state). Drives the "Revert to Original" button's
    /// visibility — pushed via `CropPresenceChanged` from sketch_board.
    has_crop: bool,
    /// Current fill state — true means "fill shapes", false means
    /// "outline only". Mirrored locally so the Fill Shape button's
    /// icon and tooltip can update via `#[watch]`.
    fill_shapes: bool,
    /// Annotation-size value captured at the start of a drag-to-edit
    /// gesture on the multiplier pill, in factor units. `AnnotationDragMove`
    /// adds (delta_x_in_pixels × ANNOTATION_DRAG_GAIN) to this to get the
    /// new value. Cleared on `AnnotationDragEnd`.
    annotation_drag_origin: Option<f32>,
    /// Accumulator for the scroll-over-pill gesture. A notched mouse
    /// wheel reports |dy| = 1.0 per click so each notch fires one
    /// `ANNOTATION_STEP` bump; trackpads emit many sub-1 dy events
    /// per swipe, which would otherwise either undershoot (if we
    /// require |dy| ≥ 1) or overshoot (if we step on every event).
    /// Accumulate dy and consume it in whole-notch chunks.
    annotation_scroll_accum: f32,
    /// True while the annotation pill is showing its inline `gtk::Entry`
    /// (click without drag flips this on). Drives the stack's visible
    /// child via imperative `set_visible_child_name` calls in the
    /// update handlers — using `#[watch]` here would emit a startup
    /// warning because the watch fires before the named children have
    /// been attached.
    editing_annotation: bool,
    /// Handle to the inline entry so the update path can grab focus +
    /// select-all the moment edit mode is entered. Stashed after
    /// `view_output!` in `init`.
    annotation_entry: Option<gtk::Entry>,
    /// Handle to the display ↔ edit stack so update handlers can flip
    /// the visible child without going through `#[watch]`.
    annotation_stack: Option<gtk::Stack>,
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
    /// Popover hanging off the arrow-style MenuButton.
    arrow_style_popover: Option<gtk::Popover>,
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

/// Human label for the blur-style popover rows.
fn blur_style_label(s: BlurStyle) -> &'static str {
    match s {
        BlurStyle::Pixelate => "Pixelate",
        BlurStyle::SecureBlur => "Blur (secure)",
        BlurStyle::Gaussian => "Blur (smooth)",
        BlurStyle::BlackOut => "Black Out",
    }
}

/// Icon for the arrow-style MenuButton + popover rows.
fn arrow_style_icon(s: ArrowStyle) -> &'static str {
    match s {
        ArrowStyle::Standard => "arrow-left-filled",
        ArrowStyle::Fancy => "arrow-left-regular",
        ArrowStyle::Curved => "arrow-undo-regular",
        ArrowStyle::Double => "arrow-bidirectional-left-right-regular",
    }
}

/// Human label for the arrow-style popover rows.
fn arrow_style_label(s: ArrowStyle) -> &'static str {
    match s {
        ArrowStyle::Standard => "Standard",
        ArrowStyle::Fancy => "Fancy",
        ArrowStyle::Curved => "Curved",
        ArrowStyle::Double => "Double",
    }
}

/// Build a popover full of icon+label rows for an enum-style picker,
/// attach it to `menu`, and wire each row to dispatch the matching
/// `StyleToolbarInput`. Shared by the arrow and blur menus — they
/// differ only in the variant list and the icon/label/input mapping
/// functions, so factoring it out keeps the two pickers structurally
/// identical (they were drifting in the previous DropDown version).
fn build_style_popover<S>(
    menu: &gtk::MenuButton,
    sender: &ComponentSender<StyleToolbar>,
    variants: &[S],
    icon_for: fn(S) -> &'static str,
    label_for: fn(S) -> &'static str,
    to_input: fn(S) -> StyleToolbarInput,
) -> gtk::Popover
where
    S: Copy + 'static,
{
    let popover = gtk::Popover::new();
    popover.add_css_class("compact-control-popover");
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    for &style in variants {
        let row = gtk::Button::new();
        row.add_css_class("flat");
        row.set_focus_on_click(false);
        let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let icon = gtk::Image::from_icon_name(icon_for(style));
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
    popover
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

/// How many factor units one pointer-pixel of horizontal drag is worth
/// on the annotation-size pill before quantising to `ANNOTATION_STEP`.
/// 0.02 × 5 px = one 0.1 step — so a comfortable wrist-flick of ~50 px
/// covers a 1.0-unit change without making smaller tweaks awkward.
const ANNOTATION_DRAG_GAIN: f32 = 0.02;
/// Quantisation step for the annotation-size value. Both drag and
/// inline-edit commits snap to this, and the displayed string uses one
/// decimal place to match.
const ANNOTATION_STEP: f32 = 0.1;
/// Pointer-pixel threshold before a press counts as a drag rather than
/// a plain click. Below this, releasing falls through to the
/// inline-edit path (the entry takes focus).
const ANNOTATION_DRAG_THRESHOLD: f64 = 3.0;
/// Hard limits for the annotation-size factor — keeps both drag and
/// inline-edit inputs in the same range that the welcome dialog and
/// state-persistence layers expect.
const ANNOTATION_MIN: f32 = 0.10;
const ANNOTATION_MAX: f32 = 10.0;

/// Format a factor value the way the pill (and the entry) should show
/// it: single decimal, always present, no leading "0" stripping.
fn format_annotation(value: f32) -> String {
    format!("{value:.1}")
}

/// Snap a raw factor to the nearest `ANNOTATION_STEP` and clamp into
/// `[ANNOTATION_MIN, ANNOTATION_MAX]`. Shared between drag and inline
/// commits so both paths produce identical, persistable values.
fn quantise_annotation(value: f32) -> f32 {
    let stepped = (value / ANNOTATION_STEP).round() * ANNOTATION_STEP;
    stepped.clamp(ANNOTATION_MIN, ANNOTATION_MAX)
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
        Tools::Rectangle | Tools::Ellipse => "Fill Shape",
        _ => "",
    }
}

/// Total horizontal width reserved for the bottom bar's centering
/// hardware: the left mirror spacer + right tool cluster each get
/// half of this, so the content (size slider, x, factor, dims,
/// fill) stays centered between two equal-width slots regardless
/// of which tool's controls are showing. Set to roughly match the
/// top bar's natural width so the window's natural-min stays
/// consistent top-vs-bottom and there's no width oscillation
/// during compositor-driven resize negotiations.
const TOOL_CLUSTER_WIDTH: i32 = 220;
/// Width of the Spotlight darkness / Highlighter opacity sliders
/// inside the cluster. Narrower than they used to be — wide enough
/// to drag precisely, slim enough that they don't dominate the
/// cluster slot.
const CLUSTER_SLIDER_WIDTH: i32 = 140;

pub struct AnnotationSizeDialog {
    annotation_size: f32,
}

#[derive(Debug, Copy, Clone)]
pub enum ToolbarEvent {
    ToolSelected(Tools),
    ColorSelected(Color),
    SizeSelected(Size),
    ArrowStyleSelected(ArrowStyle),
    BlurStyleSelected(BlurStyle),
    /// Crop tool's "Snap to edges" checkbox toggled. sketch_board
    /// forwards the value to `CropTool` and persists it to state.
    SnapToEdgesChanged(bool),
    Redo,
    Undo,
    SaveFile,
    CopyClipboard,
    ToggleFill,
    AnnotationSizeChanged(f32),
    Reset,
    SaveFileAs,
    Resize,
    OriginalScale,
    /// A toolbar popover (e.g. the unified color picker) has closed; the
    /// canvas should grab keyboard focus back so single-key shortcuts
    /// (z, r, b, …) keep working without the user having to click first.
    FocusCanvas,
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
    CropDimensionsSet { width: i32, height: i32 },
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
    ResizeImage { width: i32, height: i32 },
    /// User picked a different background style for new text
    /// drawables (Plain or Rounded). Sketch_board pushes through to
    /// the Text tool's `set_text_background`.
    TextBackgroundSelected(crate::tools::TextBackground),
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
    /// Move the saved-custom color at `from` to position `to` (clamped
    /// to the current list length). Fired by drag-and-drop within the
    /// popover's right column(s).
    ReorderCustomColor { from: usize, to: usize },
    /// Drop the saved-custom color at the given index. Fired by the
    /// per-swatch right-click → "Delete" menu.
    DeleteCustomColor(usize),
    /// A drag of a saved-custom swatch is starting at `slot`. The
    /// handler stashes both the color (so the live-reorder path can
    /// keep tracking it as the list mutates) and a snapshot of the
    /// pre-drag order so a cancel can revert.
    BeginCustomDrag(usize),
    /// While a drag is in flight, the pointer entered the drop area
    /// for `target_slot`. The handler relocates the dragged color to
    /// that slot in real time so the user sees a live preview instead
    /// of having to release to see the final order.
    LiveReorderCustomColor { target: usize },
    /// Drag finished. `success = true` if the drop landed on a valid
    /// target (we persist the latest order); `false` means cancel
    /// (drop outside the popover, Esc, etc.) and the handler restores
    /// the pre-drag snapshot.
    EndCustomDrag { success: bool },
    /// Crop tool emitted a new (width, height) for its current rect
    /// (drag tick, ratio snap, or explicit set). The handler updates
    /// `crop_width` / `crop_height` and refreshes the W/H entries
    /// unless they currently have focus (don't clobber typed input).
    CropDimensionsChanged { width: i32, height: i32 },
    /// User pressed Enter in the W entry (or `None` if the typed
    /// text didn't parse — we ignore it).
    CropWidthEntered(Option<i32>),
    /// User pressed Enter in the H entry.
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
    ImageDimensionsChanged { width: i32, height: i32 },
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
    /// User clicked "Resize" in the image-size popover with
    /// logical-pixel values. Handler multiplies by `display_scale`
    /// and emits `ToolbarEvent::ResizeImage` with image pixels.
    ResizeImageRequested { width: i32, height: i32 },
}

#[derive(Debug, Copy, Clone)]
pub enum StyleToolbarInput {
    SetVisibility(bool),
    ToggleVisibility,
    ShowAnnotationDialog,
    AnnotationDialogFinished(Option<f32>),
    /// Drag started on the annotation-size pill. Snapshot the current
    /// value so subsequent `AnnotationDragMove` events can compute the
    /// new factor relative to drag-start (avoids drift from repeated
    /// rounding).
    AnnotationDragStart,
    /// In-flight drag delta in pointer-pixels (positive = right). The
    /// handler converts pixels to factor units and broadcasts a fresh
    /// `AnnotationSizeChanged` so the change is live, not deferred to
    /// release.
    AnnotationDragMove(f64),
    /// Drag finished. `dragged` is true if the pointer moved enough to
    /// count as a value-edit; when false we treat the gesture as a
    /// plain click and flip the pill into inline-edit mode.
    AnnotationDragEnd { dragged: bool },
    /// Inline `gtk::Entry` confirmed a value — fires on Enter (the
    /// entry's `connect_activate`) and on focus-out. The handler reads
    /// the live text out of the stashed entry handle so we don't have
    /// to ship the String through the input enum (every event source
    /// would otherwise need its own widget reference).
    AnnotationCommitEditFromEntry,
    /// Esc was pressed while editing — abandon the entry without
    /// committing, restoring the prior value.
    AnnotationCancelEdit,
    /// Right-click → "Save as default" on the multiplier pill.
    /// Writes the current annotation_size to persisted state so the
    /// next launch starts at this value instead of falling back to
    /// the welcome-dialog default or the config.toml fallback.
    SaveAnnotationAsDefault,
    /// Scroll wheel over the multiplier pill — `dy` is GTK's signed
    /// scroll delta (positive = scroll down). The handler accumulates
    /// and bumps the annotation factor by ±`ANNOTATION_STEP` per
    /// virtual notch so trackpads don't blow past the value in one
    /// flick.
    AnnotationScrollBump(f64),
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
    DimensionsChanged((i32, i32)),
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
    /// Fill-shape button clicked. Mirrors `ToolbarEvent::ToggleFill`
    /// upstream and flips the local `fill_shapes` flag so the icon +
    /// tooltip in the right cluster update reactively.
    ToggleFill,
    /// Set the local `fill_shapes` mirror to the given value without
    /// emitting outbound `ToggleFill`. Used when the `F` keyboard
    /// shortcut toggles fill from outside the toolbar so the
    /// button icon + tooltip stay in sync.
    SetFillShapes(bool),
    /// Blur-algorithm popover picked a style. Mirror locally for the
    /// MenuButton icon and forward `BlurStyleSelected` upstream so
    /// sketch_board updates the active BlurTool + persists.
    SetBlurStyle(BlurStyle),
    /// Same shape as `SetBlurStyle` for the arrow-geometry picker.
    SetArrowStyle(ArrowStyle),
}

#[derive(Debug, Copy, Clone)]
pub enum AnnotationSizeDialogInput {
    ValueChanged(f32),
    Reset,
    Show(f32),
    Submit,
    Cancel,
}

#[derive(Debug, Copy, Clone)]
pub enum AnnotationSizeDialogOutput {
    AnnotationSizeSubmitted(f32),
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
/// semi-transparent black (the legacy dim color); Transparent
/// renders fully transparent (relies on the row's label text for
/// recognition); the named presets are solid; Custom keeps the
/// user's stored RGB at full alpha.
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

/// Displayed size for popover swatches and placeholders — chosen so
/// the dashed `.color-slot-empty` boxes line up with the filled
/// swatch buttons on the left column. Bumped from the earlier 20 px
/// so the palette has more on-screen presence; the source pixbuf is
/// rendered at 40 px so this scales without softening.
const SWATCH_DISPLAY_SIZE: i32 = 26;

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

    if !selected {
        return btn.upcast::<gtk::Widget>();
    }

    // The currently-selected saved-custom swatch gets a small X badge
    // pinned to its top-left corner — single-click deletes the swatch
    // (faster than the right-click → Delete fallback). Wrap the swatch
    // in a `gtk::Overlay` and add the X as an overlay child; halign /
    // valign Start pins it to the corner, and `set_measure_overlay`
    // keeps the X out of the overlay's natural-size calculation so
    // adjacent grid cells don't reflow.
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&btn));

    let x_btn = gtk::Button::builder()
        .focusable(false)
        .focus_on_click(false)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .build();
    x_btn.add_css_class("swatch-delete-x");
    let x_label = gtk::Label::new(Some("×"));
    x_label.add_css_class("swatch-delete-glyph");
    x_btn.set_child(Some(&x_label));
    attach_floating_swatch_tooltip(&x_btn, "Delete this saved color");
    let sender_for_x = sender.clone();
    x_btn.connect_clicked(move |_| {
        sender_for_x.input(ToolsToolbarInput::DeleteCustomColor(slot));
    });
    overlay.add_overlay(&x_btn);

    overlay.upcast::<gtk::Widget>()
}

/// Build the dashed empty-slot placeholder. Inert by itself —
/// drop-target wiring is added separately in
/// `attach_reorder_drop_target` so the same code path works for
/// filled swatches and empty placeholders.
fn build_dashed_placeholder() -> gtk::Widget {
    let placeholder = gtk::Box::builder()
        .width_request(SWATCH_DISPLAY_SIZE)
        .height_request(SWATCH_DISPLAY_SIZE)
        .hexpand(false)
        .vexpand(false)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    placeholder.add_css_class("color-slot-empty");
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

/// Attach a `DropTarget` to a slot widget that, on drop, fires a
/// `ReorderCustomColor` input with the source slot (drag payload)
/// and the target slot (closure-captured). Accepting on filled and
/// empty slots alike means the user can drag a color into any
/// position, including past the end of the saved list.
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
        // CenterBox mirrors the bottom row's layout so the toolbar's
        // three logical clusters (view+history on the left, drawing
        // tools in the middle, color+save on the right) sit at the
        // window's left/center/right edges instead of clustering in
        // the middle with empty space on each side. The cluster
        // pattern matches convention's editor toolbar.
        root = gtk::CenterBox {
            set_valign: Align::Start,
            add_css_class: "toolbar",
            add_css_class: "toolbar-top",

            #[watch]
            set_visible: model.visible,

            #[wrap(Some)]
            set_start_widget = &gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,

                // Normal start cluster — view + history ops. Hidden
                // when the Crop tool is active so the crop-mode top
                // toolbar can show its own start contents (just the
                // Crop indicator) without these competing for width.
                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 2,
                    #[watch]
                    set_visible: model.current_tool != Tools::Crop,

                    gtk::Button {
                        set_focusable: false,
                        set_hexpand: false,

                        set_icon_name: "resize-large-regular",
                        install_tooltip: "1:1",
                        connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::OriginalScale);},
                    },
                    gtk::Button {
                        set_focusable: false,
                        set_hexpand: false,

                        set_icon_name: "page-fit-regular",
                        install_tooltip: "Fit to window",
                        connect_clicked[sender] => move |_| {sender.output_sender().emit(ToolbarEvent::Resize);},
                    },
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
                #[name(normal_center_box)]
                gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,
                set_spacing: 2,
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
                            set_from_pixbuf: Some(&create_icon_pixbuf(
                                crop_bg_preset_swatch(model.crop_bg_color),
                            )),
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

                    gtk::Separator {
                        set_orientation: gtk::Orientation::Vertical,
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
            set_end_widget = &gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,

                // Normal end cluster — color picker + copy/save
                // actions. Hidden in Crop mode; the Cancel/Crop
                // buttons take over the right edge instead.
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
                            .position(|c| *c == color)
                            .map(|i| ColorButtons::CustomSaved(i as u64))
                    })
                    .unwrap_or(ColorButtons::Custom);
                self.color_action
                    .change_state(&matched_button.to_variant());
                self.current_color = color;
                self.current_color_pixbuf = create_icon_pixbuf(color);
                crate::state::save_last_color(color);
                sender
                    .output_sender()
                    .emit(ToolbarEvent::ColorSelected(color));
            }
            ToolsToolbarInput::TogglePickerExpansion => {
                self.picker_expanded = !self.picker_expanded;
                if let Some(rev) = &self.picker_revealer {
                    rev.set_reveal_child(self.picker_expanded);
                }
                if let Some(btn) = &self.picker_arrow_btn {
                    btn.set_icon_name(if self.picker_expanded {
                        "pan-start-symbolic"
                    } else {
                        "pan-end-symbolic"
                    });
                }
                // Re-seed the chooser to the current color each time
                // the panel opens so reopening doesn't strand the user
                // at a previously-edited hue.
                if self.picker_expanded {
                    if let Some(chooser) = &self.picker_chooser {
                        chooser.set_rgba(&RGBA::from(self.current_color));
                    }
                }
            }
            ToolsToolbarInput::SaveCustomColor(color) => {
                self.custom_colors = crate::state::append_custom_color(color);
                self.refresh_color_popover(&sender);
            }
            ToolsToolbarInput::ReorderCustomColor { from, to } => {
                // Drag-drop reorder. `from` is the source slot index;
                // `to` is the target slot. If `to` is past the end of
                // the saved list, clamp to the last position so the
                // color isn't dropped into thin air.
                if from >= self.custom_colors.len() || from == to {
                    return;
                }
                let color = self.custom_colors.remove(from);
                let insert_at = std::cmp::min(to, self.custom_colors.len());
                // When dragging downward, the removal above shifts
                // every subsequent index left by one, so a target
                // that originally lived past `from` lands one slot
                // earlier than the user's drop coordinate suggests.
                // Compensate by *not* subtracting here — `to` was
                // already the post-removal position the user picked.
                self.custom_colors.insert(insert_at, color);
                crate::state::save_custom_colors(&self.custom_colors);
                self.sync_color_action();
                self.refresh_color_popover(&sender);
            }
            ToolsToolbarInput::DeleteCustomColor(index) => {
                if index >= self.custom_colors.len() {
                    return;
                }
                self.custom_colors.remove(index);
                crate::state::save_custom_colors(&self.custom_colors);
                self.sync_color_action();
                self.refresh_color_popover(&sender);
            }
            ToolsToolbarInput::BeginCustomDrag(slot) => {
                // Pull the dragged color out of the live list so the
                // remaining saved customs shift up to fill the gap
                // (the user sees the dragged item "lifted off" the
                // grid). The pre-drag snapshot is the only way back
                // if the user cancels the drag; on a successful drop
                // we reinsert at `dragging_preview_slot`.
                if slot >= self.custom_colors.len() {
                    return;
                }
                let snapshot = self.custom_colors.clone();
                let color = self.custom_colors.remove(slot);
                self.dragging_color = Some(color);
                self.pre_drag_snapshot = Some(snapshot);
                // Ghost lands at the same logical position as the
                // pulled item — clamped to the new (shorter) list
                // length so dragging from the last slot doesn't put
                // the preview slot out of range.
                self.dragging_preview_slot =
                    Some(slot.min(self.custom_colors.len()));
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
                // *not* touched here — `custom_colors` already had
                // the dragged entry pulled out at drag-begin, so the
                // surrounding swatches stay in their current order;
                // only the ghost placeholder moves through them.
                if self.dragging_color.is_none() {
                    return;
                }
                let clamped = target.min(self.custom_colors.len());
                if self.dragging_preview_slot == Some(clamped) {
                    return;
                }
                self.dragging_preview_slot = Some(clamped);
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
                    // Reinsert at the slot the ghost preview was
                    // sitting in. The snapshot is discarded — the
                    // post-insert list is the new canonical order.
                    let insert_at = self
                        .dragging_preview_slot
                        .unwrap_or(self.custom_colors.len())
                        .min(self.custom_colors.len());
                    self.custom_colors.insert(insert_at, color);
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
                    sender.output_sender().emit(ToolbarEvent::CropDimensionsSet {
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
                    sender.output_sender().emit(ToolbarEvent::CropDimensionsSet {
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
                    sender.output_sender().emit(ToolbarEvent::CropDimensionsSet {
                        width: self.crop_height,
                        height: self.crop_width,
                    });
                }
            }
            ToolsToolbarInput::CropBgColorSelected(bg) => {
                self.crop_bg_color = bg;
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
            ToolsToolbarInput::ResizeImageRequested { width, height } => {
                let s = self.display_scale.max(1);
                sender
                    .output_sender()
                    .emit(ToolbarEvent::ResizeImage {
                        width: width * s,
                        height: height * s,
                    });
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
        let palette: Vec<Color> = APP_CONFIG
            .read()
            .color_palette()
            .palette()
            .to_vec();
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
                    .position(|c| *c == initial_color)
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
        let color_action: RelmAction<ColorAction> = RelmAction::new_stateful_with_target_value(
            &initial_button,
            move |_, state, value| {
                *state = value;
                sender_tmp.input(ToolsToolbarInput::ColorButtonSelected(value));
            },
        );

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
            color_popover: None,
            dragging_color: None,
            pre_drag_snapshot: None,
            dragging_preview_slot: None,
            color_popover_stack: None,
            color_popover_page_id: 0,
            picker_expanded: false,
            picker_revealer: None,
            picker_chooser: None,
            picker_arrow_btn: None,
        };
        let widgets = view_output!();

        // Stash the W/H entries so the `CropDimensionsChanged`
        // handler can has-focus-check before refreshing their text.
        model.crop_width_entry = Some(widgets.crop_width_entry.clone());
        model.crop_height_entry = Some(widgets.crop_height_entry.clone());

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

        let grid = gtk::Grid::builder().row_spacing(6).column_spacing(8).build();
        let w_label = gtk::Label::builder()
            .label("Width:")
            .halign(gtk::Align::End)
            .build();
        let w_entry = gtk::Entry::builder()
            .input_purpose(gtk::InputPurpose::Digits)
            .width_chars(6)
            .max_length(6)
            .build();
        let h_label = gtk::Label::builder()
            .label("Height:")
            .halign(gtk::Align::End)
            .build();
        let h_entry = gtk::Entry::builder()
            .input_purpose(gtk::InputPurpose::Digits)
            .width_chars(6)
            .max_length(6)
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

        // Units dropdown — pixels vs. percent.
        let units_model = gtk::StringList::new(&["pixels", "percent"]);
        let units_dropdown = gtk::DropDown::builder()
            .model(&units_model)
            .selected(0)
            .focusable(false)
            .build();

        grid.attach(&w_label, 0, 0, 1, 1);
        grid.attach(&w_entry, 1, 0, 1, 1);
        grid.attach(&lock_btn, 2, 0, 1, 2);
        grid.attach(&units_dropdown, 3, 0, 1, 1);
        grid.attach(&h_label, 0, 1, 1, 1);
        grid.attach(&h_entry, 1, 1, 1, 1);
        popover_box.append(&grid);

        let button_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::End)
            .margin_top(4)
            .build();
        let cancel_btn = gtk::Button::builder().label("Cancel").build();
        let resize_btn = gtk::Button::builder()
            .label("Resize")
            .css_classes(["suggested-action"])
            .build();
        button_row.append(&cancel_btn);
        button_row.append(&resize_btn);
        popover_box.append(&button_row);
        resize_popover.set_child(Some(&popover_box));
        widgets.resize_menu_btn.set_popover(Some(&resize_popover));

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
        for (bg, label_text) in [
            (CropBgColor::Transparent, "Transparent"),
            (CropBgColor::Auto, "Auto"),
            (CropBgColor::White, "White"),
            (CropBgColor::Gray, "Gray"),
            (CropBgColor::Black, "Black"),
            (CropBgColor::Custom(0.5, 0.5, 0.5), "Custom Color\u{2026}"),
        ] {
            let row_btn = gtk::Button::builder()
                .css_classes(["flat"])
                .focusable(false)
                .build();
            let row_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(8)
                .build();
            let swatch = gtk::Image::from_pixbuf(Some(&create_icon_pixbuf(
                crop_bg_preset_swatch(bg),
            )));
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
            row_btn.connect_clicked(move |btn| {
                popover_for_row.popdown();
                if is_custom_row {
                    // Open a modal color chooser so the user can pick
                    // an arbitrary matte color. On OK, push back as a
                    // `Custom(r, g, b)` selection (alpha is dropped —
                    // the matte is always fully opaque, the named
                    // "Auto" preset is the semi-transparent option).
                    let top = btn
                        .root()
                        .and_then(|r| r.downcast::<gtk::Window>().ok());
                    let mut builder = gtk::ColorChooserDialog::builder()
                        .modal(true)
                        .title("Pick crop background color");
                    if let Some(w) = &top {
                        builder = builder.transient_for(w);
                    }
                    let dialog = builder.build();
                    let sender_for_dialog = sender_for_row.clone();
                    dialog.connect_response(move |dlg, response| {
                        if response == gtk::ResponseType::Ok {
                            let rgba = dlg.rgba();
                            let picked = CropBgColor::Custom(
                                rgba.red(),
                                rgba.green(),
                                rgba.blue(),
                            );
                            sender_for_dialog
                                .input(ToolsToolbarInput::CropBgColorSelected(picked));
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
        bg_popover.set_child(Some(&bg_box));
        widgets.crop_bg_color_menu_btn.set_popover(Some(&bg_popover));

        // Build the popover for the unified color picker. Stash the
        // popover, the swatch_stack (for crossfade rebuilds), and the
        // inline-picker handles (revealer / chooser / arrow) so the
        // toggle handler can drive them without walking the tree.
        let handles = build_color_popover(&model, &sender);
        widgets.color_button.set_popover(Some(&handles.popover));
        let popover = handles.popover.clone();
        model.color_popover = Some(handles.popover);
        model.color_popover_stack = Some(handles.swatch_stack);
        model.color_popover_page_id = 1;
        model.picker_revealer = Some(handles.picker_revealer);
        model.picker_chooser = Some(handles.picker_chooser);
        model.picker_arrow_btn = Some(handles.arrow_btn);

        // Refocus the canvas when the popover closes so keyboard shortcuts
        // resume working without the user having to click on the canvas.
        {
            let sender = sender.clone();
            popover.connect_closed(move |_| {
                sender.output_sender().emit(ToolbarEvent::FocusCanvas);
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
    fn show_annotation_dialog(
        &mut self,
        sender: ComponentSender<StyleToolbar>,
        root: Option<Window>,
    ) {
        if self.annotation_dialog_controller.is_none() {
            let mut builder = AnnotationSizeDialog::builder();
            if let Some(w) = root {
                builder = builder.transient_for(&w);
            }

            let connector = builder.launch(self.annotation_size);

            let mut controller = connector.forward(sender.input_sender(), |output| match output {
                AnnotationSizeDialogOutput::AnnotationSizeSubmitted(value) => {
                    StyleToolbarInput::AnnotationDialogFinished(Some(value))
                }
            });

            controller.detach_runtime();
            self.annotation_dialog_controller = Some(controller);
        }

        let ctrl = self.annotation_dialog_controller.as_mut().unwrap();
        ctrl.emit(AnnotationSizeDialogInput::Show(self.annotation_size));
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
            set_spacing: 2,
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

            // Mirror spacer. Reserves `TOOL_CLUSTER_WIDTH` on the
            // left so the visible "main controls" (size slider, x,
            // factor, dimensions, fill) stay centered between the
            // empty left zone and the right tool-specific cluster.
            // Without this, the cluster's reserved width on the
            // right would visually pull all content left of center.
            gtk::Box {
                set_width_request: TOOL_CLUSTER_WIDTH,
            },

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
                set_width_request: 200,
                set_valign: gtk::Align::Center,
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
                install_tooltip_above: "Annotation size",
                add_mark: (0.0, gtk::PositionType::Bottom, Some("XS")),
                add_mark: (1.0, gtk::PositionType::Bottom, Some("S")),
                add_mark: (2.0, gtk::PositionType::Bottom, Some("M")),
                add_mark: (3.0, gtk::PositionType::Bottom, Some("L")),
                add_mark: (4.0, gtk::PositionType::Bottom, Some("XL")),
                add_mark: (5.0, gtk::PositionType::Bottom, Some("XXL")),
                #[watch]
                #[block_signal(size_changed)]
                set_value: size_to_slider_value(model.current_size),
                connect_value_changed[sender] => move |scale| {
                    let size = slider_value_to_size(scale.value());
                    sender.input(StyleToolbarInput::SizeChanged(size));
                } @size_changed,
            },
            // (Tool-specific controls — arrow style, blur style,
            // text background, spotlight darkness, highlighter
            // opacity — used to sit inline here. They've been moved
            // to the fixed-width right cluster below so toggling
            // between tools doesn't make the central toolbar's
            // width pulse.)
            gtk::Label {
                set_focusable: false,
                set_hexpand: false,
                set_margin_start: 12,
                set_margin_end: 6,

                set_text: "x",
            },
            // Multiplier pill: a Stack with the drag-edit Button visible
            // most of the time, swapping in a `gtk::Entry` for inline
            // typing when the user single-clicks the pill. Both children
            // share the same outer width so the cluster doesn't reflow
            // mid-edit. The Stack's transition is `None` because the
            // flip is meant to feel instantaneous (focus has to land in
            // the entry the same frame the user clicked).
            #[name = "annotation_stack"]
            gtk::Stack {
                set_transition_type: gtk::StackTransitionType::None,
                set_hhomogeneous: true,
                set_vhomogeneous: true,
                set_valign: gtk::Align::Center,

                #[name = "annotation_pill"]
                add_named[Some("display")] = &gtk::Button {
                    add_css_class: "compact-control",
                    add_css_class: "drag-edit",
                    set_focusable: false,
                    set_hexpand: false,
                    set_valign: gtk::Align::Center,
                    set_height_request: 34,
                    // ew-resize cursor signals "horizontal scrub" the moment
                    // the pointer enters the pill; combined with the
                    // GestureDrag below this gives the same feel as the
                    // numeric inputs in Blender / web devtools.
                    set_cursor_from_name: Some("ew-resize"),

                    #[watch]
                    set_label: &model.annotation_size_formatted,
                    install_tooltip_above: "Drag to change · click to type a value",

                    add_controller = gtk::GestureDrag {
                        connect_drag_begin[sender] => move |_gesture, _x, _y| {
                            sender.input(StyleToolbarInput::AnnotationDragStart);
                        },
                        connect_drag_update[sender, dragged] => move |_gesture, offset_x, _offset_y| {
                            if offset_x.abs() >= ANNOTATION_DRAG_THRESHOLD {
                                dragged.set(true);
                            }
                            sender.input(StyleToolbarInput::AnnotationDragMove(offset_x));
                        },
                        connect_drag_end[sender, dragged] => move |_gesture, _x, _y| {
                            let was_dragged = dragged.replace(false);
                            sender.input(StyleToolbarInput::AnnotationDragEnd { dragged: was_dragged });
                        },
                    },
                },

                #[name = "annotation_entry"]
                add_named[Some("edit")] = &gtk::Entry {
                    add_css_class: "compact-control",
                    add_css_class: "annotation-entry",
                    set_hexpand: false,
                    set_valign: gtk::Align::Center,
                    set_height_request: 34,
                    // `EntryExt::set_alignment` and `EditableExt::set_alignment`
                    // both exist on `Entry` and the relm4 view-macro can't
                    // pick between them — call the editable one
                    // explicitly post-construction in init().
                    set_max_length: 6,
                    set_width_chars: 4,
                    set_max_width_chars: 5,
                    set_input_purpose: gtk::InputPurpose::Number,
                    set_text: &model.annotation_size_formatted,
                    install_tooltip_above: "Enter a multiplier (0.1–10.0)",

                    // Enter commits; focus-out below routes through the
                    // same path so clicking away persists whatever the
                    // user typed. Both signals dispatch the variant
                    // that reads the entry text from the stashed handle
                    // rather than threading it through the input enum.
                    connect_activate[sender] => move |_entry| {
                        sender.input(StyleToolbarInput::AnnotationCommitEditFromEntry);
                    },

                    // Focus-out commits — feels more forgiving than
                    // discarding the entry text when the user tabs
                    // away or clicks elsewhere.
                    add_controller = gtk::EventControllerFocus {
                        connect_leave[sender] => move |_| {
                            sender.input(StyleToolbarInput::AnnotationCommitEditFromEntry);
                        },
                    },

                    // Esc cancels — explicit escape hatch in case the
                    // user wants to bail out without saving whatever
                    // they typed.
                    add_controller = gtk::EventControllerKey {
                        connect_key_pressed[sender] => move |_, key, _, _| {
                            if key == gtk::gdk::Key::Escape {
                                sender.input(StyleToolbarInput::AnnotationCancelEdit);
                                gtk::glib::Propagation::Stop
                            } else {
                                gtk::glib::Propagation::Proceed
                            }
                        },
                    },
                },
            },
            // (Output dimensions moved out of the center cluster into
            // `bottom_row.end_widget` so they live opposite the zoom
            // indicator and stay visible during Crop mode, where the
            // whole StyleToolbar is hidden. The Fill button moved
            // into the right cluster below as the tool-specific
            // control for Rectangle/Ellipse.)
            // Right cluster: every tool-specific control lives here,
            // pinned to a fixed minimum width so swapping between
            // tools (Arrow → Blur → Text → Spotlight → Highlighter
            // → nothing) doesn't make the toolbar reflow. Exactly
            // one inner widget is visible at a time; the leading
            // label re-targets per tool via
            // `tool_cluster_label(current_tool)`.
            gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,
                set_spacing: 6,
                set_margin_start: 16,
                set_width_request: TOOL_CLUSTER_WIDTH,
                set_halign: gtk::Align::Start,

                gtk::Label {
                    add_css_class: "dim-label",
                    #[watch]
                    set_label: tool_cluster_label(model.current_tool),
                    #[watch]
                    set_visible: !tool_cluster_label(model.current_tool).is_empty(),
                },

                // Arrow geometry picker. MenuButton + popover of
                // icon+label rows. Leading icon mirrors the active
                // style via `#[watch]` on `model.arrow_style`.
                #[name = "arrow_style_menu"]
                gtk::MenuButton {
                    add_css_class: "compact-control",
                    set_focusable: false,
                    set_focus_on_click: false,
                    set_hexpand: false,
                    set_valign: gtk::Align::Center,
                    set_height_request: 34,
                    set_always_show_arrow: true,
                    install_tooltip_above: "Arrow style — Standard, Fancy, Curved, Double",
                    #[watch]
                    set_visible: model.current_tool == Tools::Arrow,
                    #[wrap(Some)]
                    set_child = &gtk::Image {
                        #[watch]
                        set_icon_name: Some(arrow_style_icon(model.arrow_style)),
                    },
                },

                // Blur algorithm picker, same shape as the arrow menu.
                #[name = "blur_style_menu"]
                gtk::MenuButton {
                    add_css_class: "compact-control",
                    set_focusable: false,
                    set_focus_on_click: false,
                    set_hexpand: false,
                    set_valign: gtk::Align::Center,
                    set_height_request: 34,
                    set_always_show_arrow: true,
                    install_tooltip_above: "Blur style — Pixelate, Blur (secure), Blur (smooth), Black Out",
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
                    connect_selected_notify[sender] => move |dropdown| {
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
                    set_value: model.spotlight_darkness as f64,
                    add_mark: (0.50, gtk::PositionType::Bottom, None),
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
                        sender.output_sender().emit(ToolbarEvent::SpotlightDarknessChanged(v));
                    },
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
                    set_value: model.highlighter_opacity as f64,
                    // Visual parity with the spotlight darkness slider:
                    // a single midpoint mark gives both sliders the same
                    // natural height (the mark indicator adds bottom
                    // chrome below the trough), so they line up
                    // vertically when the cluster swaps between them.
                    add_mark: (0.50, gtk::PositionType::Bottom, None),
                    #[watch]
                    set_visible: model.current_tool == Tools::Highlighter,
                    connect_value_changed[sender] => move |scale| {
                        let v = scale.value() as f32;
                        sender.output_sender().emit(ToolbarEvent::HighlighterOpacityChanged(v));
                    },
                },
            },
            // ("Revert to Original" lives in `bottom_row.end_widget`
            // — outside the StyleToolbar — so its visibility toggling
            // doesn't shift the centered toolbar's width.)
        },
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
        match message {
            StyleToolbarInput::ShowAnnotationDialog => {
                self.show_annotation_dialog(sender, root.toplevel_window());
            }

            StyleToolbarInput::AnnotationDialogFinished(value) => {
                if let Some(value) = value {
                    let snapped = quantise_annotation(value);
                    self.annotation_size = snapped;
                    self.annotation_size_formatted = format_annotation(snapped);

                    sender
                        .output_sender()
                        .emit(ToolbarEvent::AnnotationSizeChanged(snapped));
                }
            }
            StyleToolbarInput::AnnotationDragStart => {
                self.annotation_drag_origin = Some(self.annotation_size);
            }
            StyleToolbarInput::AnnotationDragMove(dx) => {
                if let Some(origin) = self.annotation_drag_origin {
                    let raw = origin + dx as f32 * ANNOTATION_DRAG_GAIN;
                    let new_value = quantise_annotation(raw);
                    if (new_value - self.annotation_size).abs() >= ANNOTATION_STEP / 2.0 {
                        self.annotation_size = new_value;
                        self.annotation_size_formatted = format_annotation(new_value);
                        sender
                            .output_sender()
                            .emit(ToolbarEvent::AnnotationSizeChanged(new_value));
                    }
                }
            }
            StyleToolbarInput::AnnotationDragEnd { dragged } => {
                self.annotation_drag_origin = None;
                if !dragged {
                    // Click without drag → flip into inline-edit mode and
                    // hand keyboard focus to the entry. Stack swap is
                    // synchronous; the focus+select needs an idle tick
                    // because the entry's first realize happens during
                    // this same event-loop turn and grab_focus before
                    // realize is a no-op.
                    self.editing_annotation = true;
                    if let Some(stack) = &self.annotation_stack {
                        stack.set_visible_child_name("edit");
                    }
                    if let Some(entry) = self.annotation_entry.clone() {
                        entry.set_text(&self.annotation_size_formatted);
                        gtk::glib::idle_add_local_once(move || {
                            entry.grab_focus();
                            entry.select_region(0, -1);
                        });
                    }
                } else {
                    sender.output_sender().emit(ToolbarEvent::FocusCanvas);
                }
            }
            StyleToolbarInput::AnnotationCommitEditFromEntry => {
                // Idempotent: focus-out can fire after we've already
                // exited edit mode (e.g. Enter committed first, then
                // focus moves out). Skip the work in that case so we
                // don't double-emit AnnotationSizeChanged.
                if !self.editing_annotation {
                    return;
                }
                let text = self
                    .annotation_entry
                    .as_ref()
                    .map(|e| e.text().to_string())
                    .unwrap_or_default();
                // Parse leniently, snap to the 0.1 step, clamp into the
                // supported range. Unparseable text just restores the
                // prior value (no error, no toast).
                let parsed = text.trim().parse::<f32>().ok();
                if let Some(value) = parsed {
                    let snapped = quantise_annotation(value);
                    if (snapped - self.annotation_size).abs() >= ANNOTATION_STEP / 2.0 {
                        self.annotation_size = snapped;
                        self.annotation_size_formatted = format_annotation(snapped);
                        sender
                            .output_sender()
                            .emit(ToolbarEvent::AnnotationSizeChanged(snapped));
                    } else {
                        // Even when the value didn't actually change, the
                        // formatted string we display might have drifted
                        // (e.g. user typed "2" → "2.0"); refresh so the
                        // pill reads back canonically on exit.
                        self.annotation_size_formatted = format_annotation(self.annotation_size);
                    }
                } else {
                    self.annotation_size_formatted = format_annotation(self.annotation_size);
                }
                self.editing_annotation = false;
                if let Some(stack) = &self.annotation_stack {
                    stack.set_visible_child_name("display");
                }
                sender.output_sender().emit(ToolbarEvent::FocusCanvas);
            }
            StyleToolbarInput::AnnotationCancelEdit => {
                // Drop edit mode and re-sync the entry text to the live
                // model value so re-opening starts clean.
                self.editing_annotation = false;
                if let Some(entry) = &self.annotation_entry {
                    entry.set_text(&self.annotation_size_formatted);
                }
                if let Some(stack) = &self.annotation_stack {
                    stack.set_visible_child_name("display");
                }
                sender.output_sender().emit(ToolbarEvent::FocusCanvas);
            }
            StyleToolbarInput::SaveAnnotationAsDefault => {
                // Persist the live multiplier to state so next launch
                // starts here. Side-effect-only — we don't re-emit
                // AnnotationSizeChanged because the value already IS
                // the live value (the drag/entry path emitted it on
                // change).
                crate::state::save_annotation_size_factor(self.annotation_size);
            }
            StyleToolbarInput::AnnotationScrollBump(dy) => {
                // Drop the editing-entry case — if the user is mid-edit
                // we shouldn't be hijacking their input.
                if self.editing_annotation {
                    return;
                }
                // Reset on direction reversal so a flick the other way
                // doesn't have to chew through the previous direction's
                // leftover (mirrors the canvas scroll-resize behavior).
                let dy = dy as f32;
                if self.annotation_scroll_accum != 0.0
                    && self.annotation_scroll_accum.signum() != (-dy).signum()
                {
                    self.annotation_scroll_accum = 0.0;
                }
                // GTK: dy>0 is scroll-down. User asked for scroll-up =
                // higher, so negate the sign.
                self.annotation_scroll_accum += -dy;
                let mut steps = 0i32;
                while self.annotation_scroll_accum >= 1.0 {
                    self.annotation_scroll_accum -= 1.0;
                    steps += 1;
                }
                while self.annotation_scroll_accum <= -1.0 {
                    self.annotation_scroll_accum += 1.0;
                    steps -= 1;
                }
                if steps == 0 {
                    return;
                }
                let raw = self.annotation_size + steps as f32 * ANNOTATION_STEP;
                let snapped = quantise_annotation(raw);
                if (snapped - self.annotation_size).abs() >= ANNOTATION_STEP / 2.0 {
                    self.annotation_size = snapped;
                    self.annotation_size_formatted = format_annotation(snapped);
                    sender
                        .output_sender()
                        .emit(ToolbarEvent::AnnotationSizeChanged(snapped));
                }
            }
            StyleToolbarInput::SaveSizeAsDefault => {
                // Persist the live size as the default for THE
                // CURRENT TOOL only — different tools each get their
                // own saved default. Pointer / Crop don't use a size,
                // so saving while they're active is a no-op (the
                // size slider isn't even visible then, but guard
                // anyway in case the slider lingers).
                if !matches!(self.current_tool, Tools::Pointer | Tools::Crop) {
                    crate::state::save_size_for_tool(self.current_tool, self.current_size);
                }
            }

            StyleToolbarInput::SetVisibility(visible) => self.visible = visible,
            StyleToolbarInput::ToggleVisibility => {
                self.visible = !self.visible;
            }
            StyleToolbarInput::DimensionsChanged((_width, _height)) => {
                // Dimensions display moved to App's bottom_row.end_widget;
                // ignore here so the variant can be deprecated later.
            }
            StyleToolbarInput::ToolChanged(tool) => {
                self.current_tool = tool;
                // Per-tool size default: when switching tools, snap
                // the size slider to the new tool's saved default
                // (if the user has saved one). Pointer / Crop don't
                // own a meaningful "size" — leave the slider where
                // it was so coming back to a drawing tool isn't
                // disorienting.
                if !matches!(tool, Tools::Pointer | Tools::Crop)
                    && let Some(default_size) = crate::state::load_size_for_tool(tool)
                    && default_size != self.current_size
                {
                    self.current_size = default_size;
                    sender
                        .output_sender()
                        .emit(ToolbarEvent::SizeSelected(default_size));
                }
            }
            StyleToolbarInput::SetCurrentSize(size) => {
                // Mirror sketch_board's tool-size change into the
                // slider without re-broadcasting — sketch_board has
                // already applied the value via dispatch_style_change.
                self.current_size = size;
            }
            StyleToolbarInput::SyncToToolDefault => {
                // Fired by main.rs on deselect — slide back to the
                // active tool's saved default. Same fall-through
                // rules as ToolChanged: skip Pointer/Crop, and only
                // act if the user has actually saved a default for
                // this tool.
                if !matches!(self.current_tool, Tools::Pointer | Tools::Crop)
                    && let Some(default_size) = crate::state::load_size_for_tool(self.current_tool)
                    && default_size != self.current_size
                {
                    self.current_size = default_size;
                    sender
                        .output_sender()
                        .emit(ToolbarEvent::SizeSelected(default_size));
                }
            }
            StyleToolbarInput::CropPresenceChanged(present) => {
                self.has_crop = present;
            }
            StyleToolbarInput::SizeChanged(size) => {
                self.current_size = size;
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
                self.annotation_size = style.annotation_size_factor;
                self.annotation_size_formatted =
                    format_annotation(style.annotation_size_factor);
                self.fill_shapes = style.fill;
                if let Some(label) = &self.fill_tooltip_label {
                    label.set_text(fill_tooltip_text(self.fill_shapes));
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
            StyleToolbarInput::SetBlurStyle(style) => {
                // Mirror locally for the MenuButton's `#[watch]`ed icon,
                // then forward upstream so sketch_board updates the
                // active BlurTool and writes state.toml.
                self.blur_style = style;
                sender
                    .output_sender()
                    .emit(ToolbarEvent::BlurStyleSelected(style));
                sender.output_sender().emit(ToolbarEvent::FocusCanvas);
            }
            StyleToolbarInput::SetArrowStyle(style) => {
                self.arrow_style = style;
                sender
                    .output_sender()
                    .emit(ToolbarEvent::ArrowStyleSelected(style));
                sender.output_sender().emit(ToolbarEvent::FocusCanvas);
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

        // Captured by the annotation-pill GestureDrag closures so they
        // can carry the "did the pointer move far enough to count as a
        // drag?" bit across begin/update/end without poking model state
        // (which would force a view rebuild on every update).
        let dragged = std::rc::Rc::new(std::cell::Cell::new(false));

        // create model
        let initial_tool = APP_CONFIG.read().initial_tool();
        let initial_size =
            crate::state::load_size_for_tool(initial_tool).unwrap_or_default();
        let mut model = StyleToolbar {
            visible: !APP_CONFIG.read().default_hide_toolbars(),
            annotation_size: APP_CONFIG.read().annotation_size_factor(),
            annotation_size_formatted: format_annotation(
                APP_CONFIG.read().annotation_size_factor(),
            ),
            annotation_dialog_controller: None,
            current_tool: initial_tool,
            spotlight_darkness: crate::state::load_spotlight_darkness().unwrap_or(0.50),
            highlighter_opacity: crate::state::load_highlighter_opacity().unwrap_or(0.40),
            has_crop: false,
            current_size: initial_size,
            fill_shapes: APP_CONFIG.read().default_fill_shapes(),
            annotation_drag_origin: None,
            annotation_scroll_accum: 0.0,
            editing_annotation: false,
            annotation_entry: None,
            annotation_stack: None,
            fill_tooltip_label: None,
            blur_style: crate::state::load_blur_style().unwrap_or_default(),
            blur_style_popover: None,
            arrow_style: crate::state::load_arrow_style().unwrap_or_default(),
            arrow_style_popover: None,
        };

        // create widgets
        let widgets = view_output!();

        // Center-align the inline entry's text — done here rather than
        // in the view! block because both `EntryExt::set_alignment` and
        // `EditableExt::set_alignment` resolve to the same name and the
        // relm4 macro can't disambiguate. They do the same thing; call
        // the Editable one explicitly.
        gtk::prelude::EditableExt::set_alignment(&widgets.annotation_entry, 0.5);

        // Stash the inline Entry + Stack so update() can drive focus +
        // select on edit-mode entry, and flip the visible child without
        // going through `#[watch]` (which would warn about missing
        // children if it ran before add_named).
        model.annotation_entry = Some(widgets.annotation_entry.clone());
        model.annotation_stack = Some(widgets.annotation_stack.clone());
        widgets.annotation_stack.set_visible_child_name("display");

        // Build the arrow- and blur-style popovers programmatically.
        // relm4's view! macro can't iterate enum variants, and we want
        // row order + icons to stay anchored to the single source of
        // truth (`*_style_icon` / `*_style_label`). MenuButton's
        // leading icon already updates reactively via `#[watch]`, so
        // each row only needs to push a `Set*Style` input.
        model.arrow_style_popover = Some(build_style_popover(
            &widgets.arrow_style_menu,
            &sender,
            &[
                ArrowStyle::Standard,
                ArrowStyle::Fancy,
                ArrowStyle::Curved,
                ArrowStyle::Double,
            ],
            arrow_style_icon,
            arrow_style_label,
            StyleToolbarInput::SetArrowStyle,
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
            blur_style_icon,
            blur_style_label,
            StyleToolbarInput::SetBlurStyle,
        ));
        if let Some(bg) = crate::state::load_text_background() {
            let idx = match bg {
                crate::tools::TextBackground::Rounded => 0,
                crate::tools::TextBackground::Plain => 1,
            };
            widgets.text_background_dropdown.set_selected(idx);
        }

        // Right-click → "Save as default" on the controls users
        // tweak in the central / right cluster. The multiplier pill
        // and size slider each persist their value through a
        // StyleToolbar internal input (the toolbar owns the live
        // value); the opacity / darkness sliders' live values live
        // in sketch_board, so they go out as ToolbarEvents and the
        // sketch_board handler writes to state.toml.
        {
            let s = sender.clone();
            attach_save_default_popover(&widgets.annotation_pill, move || {
                s.input(StyleToolbarInput::SaveAnnotationAsDefault);
            });
        }
        // Hover + scroll over the multiplier pill nudges the factor
        // by ±ANNOTATION_STEP per notch (up = larger, down = smaller).
        // Saves the user from having to click → type → enter for small
        // tweaks. The pill's existing drag-edit gesture only
        // responds to button-1 motion, so a scroll event passes
        // through without conflict.
        {
            let scroll = gtk::EventControllerScroll::new(
                gtk::EventControllerScrollFlags::VERTICAL,
            );
            let sender_for_scroll = sender.clone();
            scroll.connect_scroll(move |_c, _dx, dy| {
                sender_for_scroll
                    .input(StyleToolbarInput::AnnotationScrollBump(dy));
                relm4::gtk::glib::Propagation::Stop
            });
            widgets.annotation_pill.add_controller(scroll);
        }
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

#[relm4::component(pub)]
impl Component for AnnotationSizeDialog {
    type Init = f32;
    type Input = AnnotationSizeDialogInput;
    type Output = AnnotationSizeDialogOutput;
    type CommandOutput = ();

    view! {
        gtk::Window {
            set_modal: true,
            set_title: Some("Choose Annotation Size"),
            set_titlebar: Some(&header_bar),

            #[wrap(Some)]
            set_child = &gtk::Box {
                set_spacing: 10,
                set_margin_all: 12,
                set_orientation: gtk::Orientation::Horizontal,

                #[name = "spin"]
                gtk::SpinButton {
                    set_editable: true,
                    set_can_focus: true,
                    set_hexpand: false,

                    set_tooltip_text: Some("Annotation Size Factor"),
                    set_numeric: true,
                    set_adjustment: &gtk::Adjustment::new(0.0, 0.0, 100.0, 0.01, 0.1, 0.0),
                    set_climb_rate: 0.1,
                    set_digits: 2,
                    #[watch]
                    #[block_signal(value_changed)]
                    set_value: model.annotation_size.into(),

                    connect_value_changed[sender] => move |button| {
                        sender.input(AnnotationSizeDialogInput::ValueChanged(button.value() as f32));
                        } @value_changed,
                },
                #[name = "spin_reset"]
                gtk::Button {
                    set_focusable: false,
                    set_hexpand: false,

                    set_tooltip_text: Some("Reset Annotation Size Factor"),
                    // `edit-reset-symbolic` is a freedesktop standard
                    // icon and renders as GTK's red 🚫 missing-icon
                    // fallback on themes that don't ship it (which is
                    // most non-GNOME setups). Use a bundled icon from
                    // `relm4-icons` instead — the curved back-arrow
                    // reads as "revert" and matches the toolbar's
                    // visual vocabulary.
                    set_icon_name: "arrow-undo-filled",
                    connect_clicked[sender] => move |_| {
                        sender.input(AnnotationSizeDialogInput::Reset);
                    },
                },

            },
        }
    }

    fn init(
        init_value: f32,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = AnnotationSizeDialog {
            annotation_size: init_value,
        };

        // the title bar didn't really work within the view! macro.
        let title_label = gtk::Label::builder()
            .label("Choose Annotation Size")
            .margin_start(6)
            .build();

        let cancel_button = gtk::Button::builder().label("Cancel").build();
        let sender_clone = sender.clone();
        cancel_button.connect_clicked(move |_| {
            sender_clone.input(AnnotationSizeDialogInput::Cancel);
        });

        let ok_button = gtk::Button::builder().label("OK").build();

        let sender_clone = sender.clone();
        ok_button.connect_clicked(move |_| {
            sender_clone.input(AnnotationSizeDialogInput::Submit);
        });

        let header_bar = gtk::HeaderBar::builder().show_title_buttons(false).build();

        header_bar.set_title_widget(Some(&title_label));
        header_bar.pack_start(&cancel_button);
        header_bar.pack_end(&ok_button);

        let widgets = view_output!();

        let key_controller = gtk::EventControllerKey::builder()
            // not sure if this is the correct phase, but anything higher and Enter to close doesn't work consistently
            .propagation_phase(gtk::PropagationPhase::Capture)
            .build();

        key_controller.connect_key_pressed(move |_, keyval, _, _| {
            use gtk::gdk::Key;
            match keyval {
                Key::Return => {
                    sender.input(AnnotationSizeDialogInput::Submit);
                    relm4::gtk::glib::Propagation::Stop
                }
                Key::Escape => {
                    sender.input(AnnotationSizeDialogInput::Cancel);
                    relm4::gtk::glib::Propagation::Stop
                }
                _ => relm4::gtk::glib::Propagation::Proceed,
            }
        });
        root.add_controller(key_controller);

        ComponentParts { model, widgets }
    }

    fn update(
        &mut self,
        message: AnnotationSizeDialogInput,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            AnnotationSizeDialogInput::ValueChanged(value) => self.annotation_size = value,
            AnnotationSizeDialogInput::Reset => {
                let a = APP_CONFIG.read().annotation_size_factor();
                self.annotation_size = a;
            }
            AnnotationSizeDialogInput::Show(value) => {
                self.annotation_size = value;
                root.show();
            }
            AnnotationSizeDialogInput::Cancel => {
                root.hide();
            }
            AnnotationSizeDialogInput::Submit => {
                // yeah, not sure if this can even happen.
                if let Err(e) = sender.output(AnnotationSizeDialogOutput::AnnotationSizeSubmitted(
                    self.annotation_size,
                )) {
                    eprintln!("Error submitting annotation size factor: {e:?}");
                }
                root.hide();
            }
        }
    }
}
