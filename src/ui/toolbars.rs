use std::{borrow::Cow, cell::RefCell, collections::HashMap};

use crate::{
    configuration::APP_CONFIG,
    style::{Color, Size},
    tools::{ArrowStyle, Tools},
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
trait RobustTooltipExt {
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

fn attach_tooltip<W: IsA<gtk::Widget> + Clone>(
    widget: &W,
    text: &str,
    position: gtk::PositionType,
) {
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

    let motion = gtk::EventControllerMotion::new();
    {
        let popover = popover.clone();
        motion.connect_enter(move |_, _, _| {
            show_tooltip(&popover);
        });
    }
    {
        let popover = popover.clone();
        motion.connect_leave(move |_| {
            hide_tooltip(&popover);
        });
    }
    widget.add_controller(motion);

    // GtkPopover::set_parent attaches the popover as a child of the
    // widget; we have to unparent it explicitly before the parent is
    // finalized or GTK warns on shutdown.
    widget.connect_destroy(move |_| {
        ACTIVE_TOOLTIP.with(|active| {
            let mut active = active.borrow_mut();
            if active.as_ref() == Some(&popover) {
                *active = None;
            }
        });
        popover.unparent();
    });
}

pub struct ToolsToolbar {
    visible: bool,
    active_button: Option<ToggleButton>,
    tool_buttons: HashMap<Tools, ToggleButton>,
    tool_action: SimpleAction,
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
}

impl ToolsToolbar {
    /// Regenerate the popover's grid with the current model state.
    /// Called after saved-customs change (save / reorder / delete) so
    /// the next popup reflects the new list.
    fn refresh_color_popover(&self, sender: &ComponentSender<ToolsToolbar>) {
        if let Some(popover) = self.color_popover.clone() {
            let grid = build_color_popover_grid(self, sender, &popover);
            popover.set_child(Some(&grid));
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

    fn show_color_dialog(&self, sender: ComponentSender<ToolsToolbar>, root: Option<Window>) {
        let current_color: RGBA = self.custom_color.into();
        let seeded_customs: Vec<RGBA> = APP_CONFIG
            .read()
            .color_palette()
            .custom()
            .iter()
            .copied()
            .chain(self.custom_colors.iter().copied())
            .map(RGBA::from)
            .collect();
        relm4::spawn_local(async move {
            // Custom window instead of `ColorChooserDialog`: GTK4's
            // dialog forces action buttons into the headerbar (or a
            // right-aligned action area), and the user wants them
            // bottom-center. Wrapping a `ColorChooserWidget` lets us
            // place the buttons wherever we want.
            let dialog = gtk::Window::builder()
                .modal(true)
                .title("Choose Color")
                .hide_on_close(true)
                .resizable(false)
                .build();
            if let Some(w) = root {
                dialog.set_transient_for(Some(&w));
            }

            let content = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(12)
                .margin_top(12)
                .margin_bottom(12)
                .margin_start(12)
                .margin_end(12)
                .build();

            let chooser = gtk::ColorChooserWidget::new();
            chooser.set_use_alpha(true);
            chooser.set_rgba(&current_color);
            // Open directly into the gradient/hue/eyedropper editor —
            // skip the palette grid, since the picker popover already
            // serves that role and the editor is what users came here
            // for (eyedropper + hex input + fine-tuning).
            chooser.set_show_editor(true);
            if !seeded_customs.is_empty() {
                chooser.add_palette(gtk::Orientation::Horizontal, 8, &seeded_customs);
            }
            chooser.set_hexpand(true);
            chooser.set_vexpand(true);
            content.append(&chooser);

            // Bottom-center button row. `halign = Center` keeps the
            // buttons centered horizontally regardless of dialog
            // width, while the inner box keeps them tight together.
            let button_row = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(8)
                .halign(gtk::Align::Center)
                .build();

            let save_btn = gtk::Button::with_label("Save Custom Color");
            let choose_btn = gtk::Button::with_label("Choose Color");
            choose_btn.add_css_class("suggested-action");

            button_row.append(&save_btn);
            button_row.append(&choose_btn);
            content.append(&button_row);

            dialog.set_child(Some(&content));

            // Wiring. `Save` persists the current chooser color and
            // keeps the dialog open; `Choose` applies it and closes.
            // Escape closes without applying.
            let chooser_for_save = chooser.clone();
            let sender_for_save = sender.clone();
            save_btn.connect_clicked(move |_| {
                let color = Color::from_gdk(chooser_for_save.rgba());
                sender_for_save.input(ToolsToolbarInput::SaveCustomColor(color));
            });

            let chooser_for_choose = chooser.clone();
            let dialog_for_choose = dialog.clone();
            let sender_for_choose = sender.clone();
            choose_btn.connect_clicked(move |_| {
                let color = Color::from_gdk(chooser_for_choose.rgba());
                sender_for_choose.input(ToolsToolbarInput::ColorDialogFinished(Some(color)));
                dialog_for_choose.close();
            });

            let key_ctrl = gtk::EventControllerKey::new();
            let dialog_for_esc = dialog.clone();
            key_ctrl.connect_key_pressed(move |_, key, _, _| {
                if key == relm4::gtk::gdk::Key::Escape {
                    dialog_for_esc.close();
                    relm4::gtk::glib::Propagation::Stop
                } else {
                    relm4::gtk::glib::Propagation::Proceed
                }
            });
            dialog.add_controller(key_ctrl);

            dialog.present();
        });
    }
}

/// Number of saved-custom slots per popover column. Once the user
/// saves more than this many, an extra column appears to the right
/// and the popover grows wider — matches convention's "fill columns
/// then wrap" behavior. Set to 11 so each custom column is one row
/// taller than the 10 palette swatches; the extra row sits next to
/// the pick-button footer in the left column.
const SLOTS_PER_COLUMN: usize = 11;

/// Build the popover that hangs off the unified color-picker MenuButton.
/// style:a vertical 2-column grid with palette colors on
/// the left and the user's saved-custom colors (plus dashed empty
/// slots) on the right, with a color-wheel button at the bottom that
/// opens the system color dialog.
fn build_color_popover(
    model: &ToolsToolbar,
    sender: &ComponentSender<ToolsToolbar>,
) -> gtk::Popover {
    let popover = gtk::Popover::new();
    popover.add_css_class("color-picker-popover");
    popover.set_position(gtk::PositionType::Bottom);
    popover.set_has_arrow(true);

    let grid = build_color_popover_grid(model, sender, &popover);
    popover.set_child(Some(&grid));
    popover
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
    let grid = gtk::Grid::builder()
        .row_spacing(4)
        .column_spacing(8)
        .margin_start(8)
        .margin_end(8)
        .margin_top(8)
        .margin_bottom(8)
        .build();

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
        btn.set_tooltip_text(Some(&format!("{name} ({shortcut})")));
        grid.attach(&btn, 0, i as i32, 1, 1);
    }

    // Right column(s): persisted saved-custom colors, then dashed
    // placeholders filling out the rest of each column. We keep at
    // least one column visible even when nothing is saved (so the
    // user sees where the slots will appear), and grow rightward
    // each time the saved list crosses an `SLOTS_PER_COLUMN`
    // boundary. Every slot — filled or empty — accepts a drop, so
    // colors can be dragged into any position including past the
    // current end of the list.
    let saved = &model.custom_colors;
    let n_custom_cols = saved.len().div_ceil(SLOTS_PER_COLUMN).max(1);
    let total_slots = n_custom_cols * SLOTS_PER_COLUMN;
    for slot in 0..total_slots {
        let col_idx = slot / SLOTS_PER_COLUMN;
        let row_idx = slot % SLOTS_PER_COLUMN;
        let grid_col = (1 + col_idx) as i32;
        let widget: gtk::Widget = if let Some(color) = saved.get(slot).copied() {
            build_saved_custom_swatch(color, slot, sender)
        } else {
            build_dashed_placeholder()
        };
        // Every slot — filled or empty — is a drop target so the
        // user can drag-rearrange or push a swatch off the end.
        attach_reorder_drop_target(&widget, slot, sender);
        grid.attach(&widget, grid_col, row_idx as i32, 1, 1);
    }

    // Color-wheel button → opens the custom-color dialog (eyedropper
    // + hex + "Save Custom Color"). Anchored directly below the last
    // palette swatch (row 10 in column 0) so it stays put visually
    // even when the right column grows past 10 saved customs. The
    // popover is dismissed first so it doesn't sit on top of the
    // dialog (Wayland in particular routes clicks to the popover
    // otherwise).
    let pick_btn = gtk::Button::builder()
        .focusable(false)
        .focus_on_click(false)
        .hexpand(false)
        .icon_name("color-regular")
        .build();
    pick_btn.add_css_class("flat");
    pick_btn.add_css_class("color-wheel-button");
    pick_btn.set_tooltip_text(Some("Pick custom color"));
    let sender_clone = sender.clone();
    let popover_clone = popover.clone();
    pick_btn.connect_clicked(move |_| {
        popover_clone.popdown();
        sender_clone.input(ToolsToolbarInput::ShowColorDialog);
    });
    grid.attach(&pick_btn, 0, 10, 1, 1);

    grid
}

pub struct StyleToolbar {
    visible: bool,
    annotation_size: f32,
    annotation_size_formatted: String,
    annotation_dialog_controller: Option<Controller<AnnotationSizeDialog>>,
    output_dimensions: String,
    /// Tracks the currently-active tool so tool-specific controls (e.g. the
    /// arrow-style dropdown) can show/hide reactively.
    current_tool: Tools,
}

pub struct AnnotationSizeDialog {
    annotation_size: f32,
}

#[derive(Debug, Copy, Clone)]
pub enum ToolbarEvent {
    ToolSelected(Tools),
    ColorSelected(Color),
    SizeSelected(Size),
    ArrowStyleSelected(ArrowStyle),
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
}

#[derive(Debug, Copy, Clone)]
pub enum ToolsToolbarInput {
    SetVisibility(bool),
    ToggleVisibility,
    SwitchSelectedTool(Tools),
    ColorButtonSelected(ColorButtons),
    ShowColorDialog,
    ColorDialogFinished(Option<Color>),
    /// Append the given color to the user's persisted saved-custom
    /// palette, then refresh the popover so the new swatch shows up
    /// next to its dashed placeholder neighbors. Fired by the dialog's
    /// "Save Custom Color" button.
    SaveCustomColor(Color),
    /// Move the saved-custom color at `from` to position `to` (clamped
    /// to the current list length). Fired by drag-and-drop within the
    /// popover's right column(s).
    ReorderCustomColor { from: usize, to: usize },
    /// Drop the saved-custom color at the given index. Fired by the
    /// per-swatch right-click → "Delete" menu.
    DeleteCustomColor(usize),
}

#[derive(Debug, Copy, Clone)]
pub enum StyleToolbarInput {
    SetVisibility(bool),
    ToggleVisibility,
    ShowAnnotationDialog,
    AnnotationDialogFinished(Option<f32>),
    DimensionsChanged((i32, i32)),
    /// The active drawing tool changed; tool-specific controls re-evaluate
    /// their visibility.
    ToolChanged(Tools),
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
/// swatch buttons on the left column.
const SWATCH_DISPLAY_SIZE: i32 = 20;

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
    sender: &ComponentSender<ToolsToolbar>,
) -> gtk::Widget {
    use relm4::gtk::gdk;

    let btn = gtk::ToggleButton::builder()
        .focusable(false)
        .focus_on_click(false)
        .hexpand(false)
        .child(&create_icon(color))
        .build();
    btn.add_css_class("flat");
    btn.add_css_class("color-swatch");
    btn.set_action::<ColorAction>(ColorButtons::CustomSaved(slot as u64));
    let tooltip = match color.name() {
        Some(name) => format!("{name} (saved {})", slot + 1),
        None => format!("Saved color {}", slot + 1),
    };
    btn.set_tooltip_text(Some(&tooltip));

    // DragSource — the payload is the source slot index. GTK only
    // starts a drag once the pointer crosses the motion threshold, so
    // a quick click still falls through to the action handler.
    let drag = gtk::DragSource::new();
    drag.set_actions(gdk::DragAction::MOVE);
    let slot_for_prepare = slot;
    drag.connect_prepare(move |_src, _x, _y| {
        let value = (slot_for_prepare as u32).to_value();
        Some(gdk::ContentProvider::for_value(&value))
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

    btn.upcast::<gtk::Widget>()
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
    let sender = sender.clone();
    drop_target.connect_drop(move |_dt, value, _x, _y| {
        let Ok(from) = value.get::<u32>() else {
            return false;
        };
        let from = from as usize;
        if from == target_slot {
            // Self-drop: nothing to do, but report success so GTK
            // doesn't render a "drop refused" cursor flash.
            return true;
        }
        sender.input(ToolsToolbarInput::ReorderCustomColor {
            from,
            to: target_slot,
        });
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
        root = gtk::Box {
            set_orientation: gtk::Orientation::Horizontal,
            set_spacing: 2,
            set_valign: Align::Start,
            set_halign: Align::Center,
            add_css_class: "toolbar",
            add_css_class: "toolbar-top",

            #[watch]
            set_visible: model.visible,

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
            gtk::Separator {},
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
                ActionablePlus::set_action::<ToolsAction>: Tools::Highlight,
            },
            gtk::Separator {},
            // Unified color picker — single MenuButton showing the current
            // color; the popover (built in init) holds the palette and a
            // custom-color picker, mirroring a standard X's compact picker.
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
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
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
            ToolsToolbarInput::ShowColorDialog => {
                self.show_color_dialog(sender, root.toplevel_window());
            }
            ToolsToolbarInput::ColorDialogFinished(color) => {
                if let Some(color) = color {
                    self.custom_color = color;
                    // If the picked color happens to match an existing
                    // palette entry or saved-custom swatch, sync the
                    // action state so that swatch lights up as checked.
                    // Otherwise leave the action at `Custom` — no slot
                    // in the popover represents this transient pick.
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
            }
            ToolsToolbarInput::SaveCustomColor(color) => {
                // Append-and-persist, then regenerate the popover's
                // grid so the new swatch shows up immediately. The
                // popover is typically closed while the dialog is
                // open, so the rebuild is invisible to the user — but
                // it's ready the next time they open the picker.
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

        // Resolve the starting color: persisted last-color from the XDG
        // state file wins; otherwise fall back to the palette's red entry
        // (or `Color::red()` directly if the user removed it). Black is
        // intentionally not the default — the prior `palette.first()`
        // behavior gave the wrong impression that black was the chosen
        // annotation color on every launch.
        let palette: Vec<Color> = APP_CONFIG
            .read()
            .color_palette()
            .palette()
            .to_vec();
        let saved_last_color = crate::state::load_last_color();
        let saved_customs = crate::state::load_custom_colors();
        let initial_color = saved_last_color.unwrap_or_else(|| {
            palette
                .iter()
                .copied()
                .find(|c| *c == Color::red())
                .unwrap_or_else(Color::red)
        });
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
            current_color: initial_color,
            current_color_pixbuf: initial_color_pixbuf,
            custom_color,
            custom_colors: saved_customs,
            color_action: SimpleAction::from(color_action.clone()),
            color_popover: None,
        };
        let widgets = view_output!();

        // Build the popover for the unified color picker. Stash the
        // handle on the model so `SaveCustomColor` can regenerate the
        // grid in place without rebuilding the whole popover.
        let popover = build_color_popover(&model, &sender);
        widgets.color_button.set_popover(Some(&popover));
        model.color_popover = Some(popover.clone());

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
            (Tools::Highlight, widgets.highlight_button.clone()),
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
            set_valign: Align::End,
            set_halign: Align::Center,
            add_css_class: "toolbar",
            add_css_class: "toolbar-bottom",

            #[watch]
            set_visible: model.visible,

            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,
                set_label: "XS",
                install_tooltip_above: "Annotation size: X-Small",
                ActionablePlus::set_action::<SizeAction>: Size::XSmall,
            },
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,
                set_label: "S",
                install_tooltip_above: "Annotation size: Small",
                ActionablePlus::set_action::<SizeAction>: Size::Small,
            },
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,
                set_label: "M",
                install_tooltip_above: "Annotation size: Medium",
                ActionablePlus::set_action::<SizeAction>: Size::Medium,
            },
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,
                set_label: "L",
                install_tooltip_above: "Annotation size: Large",
                ActionablePlus::set_action::<SizeAction>: Size::Large,
            },
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,
                set_label: "XL",
                install_tooltip_above: "Annotation size: X-Large",
                ActionablePlus::set_action::<SizeAction>: Size::XLarge,
            },
            gtk::ToggleButton {
                set_focusable: false,
                set_hexpand: false,
                set_label: "XXL",
                install_tooltip_above: "Annotation size: XX-Large",
                ActionablePlus::set_action::<SizeAction>: Size::XXLarge,
            },
            // Arrow style dropdown — only relevant when the Arrow tool is
            // active. Hidden otherwise so it doesn't clutter the toolbar.
            gtk::DropDown {
                set_focusable: false,
                set_hexpand: false,
                set_model: Some(&gtk::StringList::new(&["Standard", "Fancy", "Curved", "Double"])),
                install_tooltip_above: "Arrow style",
                set_margin_start: 4,
                #[watch]
                set_visible: model.current_tool == Tools::Arrow,
                connect_selected_notify[sender] => move |dropdown| {
                    let style = match dropdown.selected() {
                        0 => ArrowStyle::Standard,
                        1 => ArrowStyle::Fancy,
                        2 => ArrowStyle::Curved,
                        3 => ArrowStyle::Double,
                        _ => return,
                    };
                    sender.output_sender().emit(ToolbarEvent::ArrowStyleSelected(style));
                },
            },
            gtk::Label {
                set_focusable: false,
                set_hexpand: false,

                set_text: "x",
            },
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,

                #[watch]
                set_label: &model.annotation_size_formatted,
                install_tooltip_above: "Edit Annotation Size Factor",

                connect_clicked => StyleToolbarInput::ShowAnnotationDialog
            },
            gtk::Separator {},
            gtk::Label {
                set_focusable: false,
                set_hexpand: false,
                set_margin_start: 10,
                set_width_chars: 11,

                #[watch]
                set_text: &model.output_dimensions,
                install_tooltip_above: "Output dimensions (width x height)",
            },
            gtk::Separator {},
            gtk::Button {
                set_focusable: false,
                set_hexpand: false,

                set_icon_name: if APP_CONFIG.read().default_fill_shapes() {
                    "paint-bucket-filled"
                } else {
                    "paint-bucket-regular"
                },
                install_tooltip_above: "Fill shape",
                connect_clicked[sender] => move |button| {
                    sender.output_sender().emit(ToolbarEvent::ToggleFill);
                    let new_icon = if button.icon_name() == Some("paint-bucket-regular".into()) {
                        "paint-bucket-filled"
                    } else {
                        "paint-bucket-regular"
                    };
                    button.set_icon_name(new_icon);
                },
            },
        },
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
        match message {
            StyleToolbarInput::ShowAnnotationDialog => {
                self.show_annotation_dialog(sender, root.toplevel_window());
            }

            StyleToolbarInput::AnnotationDialogFinished(value) => {
                if let Some(value) = value {
                    self.annotation_size = value;
                    self.annotation_size_formatted = format!("{value:.2}");

                    sender
                        .output_sender()
                        .emit(ToolbarEvent::AnnotationSizeChanged(value));
                }
            }

            StyleToolbarInput::SetVisibility(visible) => self.visible = visible,
            StyleToolbarInput::ToggleVisibility => {
                self.visible = !self.visible;
            }
            StyleToolbarInput::DimensionsChanged((width, height)) => {
                self.output_dimensions = format!("{}x{}", width, height);
            }
            StyleToolbarInput::ToolChanged(tool) => {
                self.current_tool = tool;
            }
        }
    }

    fn init(
        _: Self::Init,
        _root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        // Size Action for selecting sizes
        let sender_tmp = sender.clone();
        let size_action: RelmAction<SizeAction> =
            RelmAction::new_stateful_with_target_value(&Size::Medium, move |_, state, value| {
                *state = value;
                sender_tmp
                    .output_sender()
                    .emit(ToolbarEvent::SizeSelected(*state));
            });

        // create model
        let model = StyleToolbar {
            visible: !APP_CONFIG.read().default_hide_toolbars(),
            annotation_size: APP_CONFIG.read().annotation_size_factor(),
            annotation_size_formatted: format!(
                "{0:.2}",
                APP_CONFIG.read().annotation_size_factor()
            ),
            annotation_dialog_controller: None,
            output_dimensions: String::new(),
            current_tool: APP_CONFIG.read().initial_tool(),
        };

        // create widgets
        let widgets = view_output!();

        let mut group = RelmActionGroup::<StyleToolbarActionGroup>::new();
        group.add_action(size_action);

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
                    set_icon_name: "edit-reset-symbolic",
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
