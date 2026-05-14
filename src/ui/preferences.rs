//! Preferences dialog — keyboard-shortcut customization and other
//! session-wide settings (annotation size factor, scroll inversion,
//! Esc behavior, palette visibility, sticky in-session defaults).
//!
//! Lays out one row per tool with a recorder button that captures a
//! single keypress and writes it into the working keybind map. Save
//! commits keybinds to `APP_CONFIG`; Cancel discards keybind edits.
//! The behavior toggles apply immediately on change and persist to
//! `state.toml` on the spot — they're not part of the keybind
//! Cancel/Save transaction.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use relm4::Sender;
use relm4::gtk;
use relm4::gtk::gdk;
use relm4::gtk::prelude::*;

use crate::configuration::APP_CONFIG;
use crate::sketch_board::{SketchBoardInput, SketchBoardOutput};
use crate::tools::Tools;

/// Order tools appear in the prefs dialog. Mirrors the top
/// toolbar's left-to-right order so the user can scan visually
/// across both surfaces without re-translating positions.
const ROW_ORDER: &[Tools] = &[
    Tools::Pointer,
    Tools::Crop,
    Tools::Brush,
    Tools::Line,
    Tools::Arrow,
    Tools::Rectangle,
    Tools::Ellipse,
    Tools::Text,
    Tools::Marker,
    Tools::Blur,
    Tools::Highlighter,
    Tools::Spotlight,
];

/// Label shown on the recorder button while waiting for a keypress.
const PROMPT_LABEL: &str = "Press a key…";

/// Display fragment for an unset shortcut. Most tools won't be in
/// this state, but the configuration's default doesn't bind every
/// tool (e.g. there's no default for Spotlight in user config until
/// they set it), so this covers the gap.
const EMPTY_LABEL: &str = "—";

/// Per-row state — kept alive in a `Vec` on the dialog so each row's
/// closures can find and refresh sibling rows when a key reassignment
/// orphans them.
struct Row {
    tool: Tools,
    button: gtk::Button,
}

impl Row {
    /// Refresh the button label to reflect the working-map value for
    /// this row's tool (i.e. find the char that currently points to
    /// `self.tool`, or fall back to the empty marker).
    fn refresh(&self, working: &HashMap<char, Tools>) {
        let ch = current_char_for(working, self.tool);
        self.button.set_label(&label_for(ch));
    }
}

/// Locate the character currently mapped to `tool` in the working map,
/// if any. The map is char→Tool so a reverse lookup is necessary.
fn current_char_for(working: &HashMap<char, Tools>, tool: Tools) -> Option<char> {
    working.iter().find_map(|(c, t)| (*t == tool).then_some(*c))
}

/// Format a character (or its absence) for display on the recorder
/// button. Uppercased so single-letter shortcuts read consistently
/// regardless of how the user persisted them.
fn label_for(ch: Option<char>) -> String {
    match ch {
        Some(c) => c.to_ascii_uppercase().to_string(),
        None => EMPTY_LABEL.to_string(),
    }
}

/// Open the Preferences dialog, parented (transient) to `root` so the
/// window manager treats it as a modal child of the main satty window.
///
/// `sketch_board_sender` is the channel by which the annotation-size
/// SpinButton pushes its live value into sketch_board's `self.style`
/// so a change takes effect immediately for the next stroke (otherwise
/// APP_CONFIG would update but sketch_board's already-captured value
/// wouldn't refresh until the next launch).
///
/// `prefs_factor_spin_slot` is App's shared handle to this dialog's
/// annotation-size SpinButton + its `value-changed` signal id. The
/// dialog populates it on open and clears it on close so the welcome
/// modal's live updates can push values straight into this spin (and
/// be told whether to bother — `None` means "Preferences isn't open,
/// no UI to sync").
pub fn open<W: IsA<gtk::Widget>>(
    root: &W,
    sketch_board_sender: Sender<SketchBoardInput>,
    prefs_factor_spin_slot: std::rc::Rc<
        std::cell::RefCell<Option<(gtk::SpinButton, gtk::glib::SignalHandlerId)>>,
    >,
) {
    let toplevel = root
        .root()
        .and_then(|r| r.downcast::<gtk::Window>().ok());

    let dialog = gtk::Window::builder()
        .title("Preferences")
        .modal(true)
        .destroy_with_parent(true)
        // Sized to fit the longest tool label + recorder chip
        // comfortably; anything wider just adds dead space on each
        // side of the row.
        .default_width(320)
        .resizable(false)
        .build();
    if let Some(w) = &toplevel {
        dialog.set_transient_for(Some(w));
    }

    // Window-level shortcuts. Esc closes the dialog; Super+W also
    // closes it (so the user's "close window" muscle memory targets
    // the dialog instead of falling through to satty's main window,
    // which would otherwise be Hyprland's natural target). Bubble
    // phase so the per-row recorder controller — which uses Esc to
    // cancel a recording — gets first crack at the keystroke while
    // recording is in progress.
    {
        let dialog_for_keys = dialog.clone();
        let key_controller = gtk::EventControllerKey::new();
        key_controller.connect_key_pressed(move |_c, key, _code, mods| {
            if key == gdk::Key::Escape && mods.is_empty() {
                dialog_for_keys.close();
                return gtk::glib::Propagation::Stop;
            }
            if mods.contains(gdk::ModifierType::SUPER_MASK)
                && matches!(key, gdk::Key::w | gdk::Key::W)
            {
                dialog_for_keys.close();
                return gtk::glib::Propagation::Stop;
            }
            gtk::glib::Propagation::Proceed
        });
        dialog.add_controller(key_controller);
    }

    // Cap the dialog at 95% of the parent canvas height. The entire
    // dialog content (shortcuts + behavior + buttons) sits inside
    // ONE outer scroller — if it fits naturally the dialog shrinks
    // to its content with no scrollbar; if it overflows the parent
    // height, the whole panel scrolls together instead of just the
    // shortcuts list scrolling separately from the behavior section
    // beneath it.
    let parent_h = toplevel
        .as_ref()
        .map(|w| w.height())
        .filter(|h| *h > 0)
        .unwrap_or(900);
    let max_dialog_h = (((parent_h as f64) * 0.95) as i32).max(320);

    let outer = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .build();

    let heading = gtk::Label::builder()
        .label("Keyboard Shortcuts")
        .halign(gtk::Align::Start)
        .build();
    heading.add_css_class("title-3");
    outer.append(&heading);

    let hint = gtk::Label::builder()
        .label(
            "Click a shortcut and press a key to record. \
             Press Esc to cancel a recording.",
        )
        .wrap(true)
        .xalign(0.0)
        .build();
    hint.add_css_class("dim-label");
    outer.append(&hint);

    // Working keybind map — clones the current APP_CONFIG state so the
    // user's edits are scratch until they press Save.
    let initial_shortcuts: HashMap<char, Tools> = APP_CONFIG
        .read()
        .keybinds()
        .shortcuts()
        .clone();
    let working: Rc<RefCell<HashMap<char, Tools>>> = Rc::new(RefCell::new(initial_shortcuts));

    // Shared "is some row currently recording" flag. We only allow one
    // row in recording state at a time — clicking a second row while
    // the first is active cancels the first.
    let rows: Rc<RefCell<Vec<Row>>> = Rc::new(RefCell::new(Vec::new()));
    let recording_row: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));

    // List of recorder rows in a scrolled container so longer tool
    // lists scroll rather than blowing past the dialog's chrome.
    let list_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    for tool in ROW_ORDER {
        let row_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .build();
        let name = gtk::Label::builder()
            .label(tool.display_name())
            .halign(gtk::Align::Start)
            .hexpand(true)
            .build();
        row_box.append(&name);

        let ch = current_char_for(&working.borrow(), *tool);
        let button = gtk::Button::builder()
            .label(label_for(ch))
            .width_request(96)
            .halign(gtk::Align::End)
            .build();
        button.add_css_class("monospace");
        row_box.append(&button);

        list_box.append(&row_box);

        let row_index = rows.borrow().len();
        rows.borrow_mut().push(Row {
            tool: *tool,
            button: button.clone(),
        });

        // Click → enter recording mode for this row.
        let working_for_click = working.clone();
        let rows_for_click = rows.clone();
        let recording_for_click = recording_row.clone();
        let tool_for_click = *tool;
        let button_for_click = button.clone();
        button.connect_clicked(move |btn| {
            // Cancel any other row that was mid-recording — refresh its
            // label from the working map (its prior committed value).
            if let Some(prev) = recording_for_click.get()
                && prev != row_index
                && let Some(row) = rows_for_click.borrow().get(prev)
            {
                row.refresh(&working_for_click.borrow());
            }
            recording_for_click.set(Some(row_index));
            btn.set_label(PROMPT_LABEL);
            btn.grab_focus();

            // Attach a one-shot key controller. Esc reverts; any other
            // single character commits as the new shortcut for this
            // row's tool. We capture from the inner button (not the
            // window) so the controller's lifetime is tied to the
            // button — disconnecting from the button on the same tick
            // we capture would have to wait for the event handler to
            // return first.
            let controller = gtk::EventControllerKey::new();
            let working_inner = working_for_click.clone();
            let rows_inner = rows_for_click.clone();
            let recording_inner = recording_for_click.clone();
            let btn_inner = button_for_click.clone();
            let tool_inner = tool_for_click;
            controller.connect_key_pressed(move |ctrl, key, _code, modifier| {
                // Ignore plain modifier presses (Shift / Ctrl / etc.)
                // so the user can hold modifiers and then press a key
                // without the bare modifier being captured first.
                if matches!(
                    key,
                    gdk::Key::Shift_L
                        | gdk::Key::Shift_R
                        | gdk::Key::Control_L
                        | gdk::Key::Control_R
                        | gdk::Key::Alt_L
                        | gdk::Key::Alt_R
                        | gdk::Key::Super_L
                        | gdk::Key::Super_R
                ) {
                    return gtk::glib::Propagation::Proceed;
                }

                // Esc → cancel recording, revert label.
                if key == gdk::Key::Escape {
                    if let Some(row) = rows_inner.borrow().get(row_index) {
                        row.refresh(&working_inner.borrow());
                    }
                    recording_inner.set(None);
                    // One-shot: drop the controller so we don't keep
                    // intercepting subsequent presses.
                    btn_inner.remove_controller(ctrl);
                    return gtk::glib::Propagation::Stop;
                }

                // Disallow modifier-combined keys — shortcuts are
                // single chars throughout the codebase.
                if !modifier.is_empty()
                    && modifier
                        .intersection(
                            gdk::ModifierType::CONTROL_MASK
                                | gdk::ModifierType::ALT_MASK
                                | gdk::ModifierType::SUPER_MASK,
                        )
                        != gdk::ModifierType::empty()
                {
                    return gtk::glib::Propagation::Proceed;
                }

                // Try to turn the key into a single printable char.
                let Some(c_raw) = key.to_unicode() else {
                    return gtk::glib::Propagation::Proceed;
                };
                let ch = c_raw.to_ascii_lowercase();
                if !ch.is_ascii_alphanumeric() {
                    // Reject punctuation / control / etc. for now —
                    // matches the existing configuration's validation.
                    return gtk::glib::Propagation::Proceed;
                }

                // Commit: drop any other tool that owned `ch`, then
                // assign `ch` → this row's tool. Refresh BOTH rows so
                // the displaced tool's label updates to "—".
                let mut map = working_inner.borrow_mut();
                let displaced: Option<Tools> = map.get(&ch).copied();
                // First, drop the assignment this tool currently holds
                // (if any) so the map stays in (char → unique tool)
                // shape after the insert.
                map.retain(|_, t| *t != tool_inner);
                map.insert(ch, tool_inner);
                drop(map);

                let working_snapshot = working_inner.borrow();
                for (i, row) in rows_inner.borrow().iter().enumerate() {
                    if i == row_index
                        || displaced == Some(row.tool)
                    {
                        row.refresh(&working_snapshot);
                    }
                }
                drop(working_snapshot);

                recording_inner.set(None);
                btn_inner.remove_controller(ctrl);
                gtk::glib::Propagation::Stop
            });
            btn.add_controller(controller);
        });
    }

    outer.append(&list_box);

    // Behavior section sits BELOW the shortcuts list — the keyboard
    // recorder is the dialog's primary content, the behavior
    // toggles are secondary preferences. Each toggle applies
    // immediately and persists to state.toml on click; the dialog's
    // Save button only commits the keyboard shortcuts.
    let behavior_heading = gtk::Label::builder()
        .label("Behavior")
        .halign(gtk::Align::Start)
        .margin_top(8)
        .build();
    behavior_heading.add_css_class("title-3");
    outer.append(&behavior_heading);

    // Annotation size factor — the multiplier that scales every
    // Size-based metric (text height, line width, arrow heads, blur
    // radius). Mostly set once during onboarding to match the user's
    // display scale; this row lets them tune it later without hunting
    // through config files. Changes write to state.toml + APP_CONFIG
    // immediately and push directly into sketch_board so the very
    // next stroke uses the new factor.
    let factor_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    let factor_label = gtk::Label::builder()
        .label("Annotation size factor")
        .halign(gtk::Align::Start)
        .build();
    factor_row.append(&factor_label);
    // "?" help button — re-launches the first-run welcome dialog so the
    // user can revisit the explanation of what this factor controls and
    // re-pick a value through the onboarding UI (including the
    // "Use detected" / "1.00×" reset shortcuts that this spin button
    // alone doesn't expose). Sits right next to the label so the
    // affordance reads as "what is this setting?" rather than as a
    // sibling control of the spin.
    let factor_help = gtk::Button::builder()
        .label("?")
        .tooltip_text("What does this do? Re-open the welcome guide.")
        .valign(gtk::Align::Center)
        .hexpand(false)
        .build();
    factor_help.add_css_class("circular");
    factor_help.add_css_class("flat");
    let factor_help_sender = sketch_board_sender.clone();
    factor_help.connect_clicked(move |_| {
        let _ = factor_help_sender
            .send(SketchBoardInput::Output(SketchBoardOutput::OpenWelcomeDialog));
    });
    factor_row.append(&factor_help);
    // Spacer so the spin lands flush against the right edge instead of
    // hugging the help button. `hexpand` on a blank Label is the most
    // compact way to flex-fill the gap without dragging in an Adw box.
    let factor_row_spacer = gtk::Label::builder().hexpand(true).build();
    factor_row.append(&factor_row_spacer);
    let factor_spin = gtk::SpinButton::builder()
        .adjustment(&gtk::Adjustment::new(
            APP_CONFIG.read().annotation_size_factor().into(),
            // 0.10..=10.0 with 0.1 detents matches the canvas-side
            // Alt+scroll constants in `scroll_annotation_multiplier`
            // so both paths land on the same grid.
            0.10,
            10.0,
            0.10,
            0.50,
            0.0,
        ))
        .climb_rate(0.1)
        .digits(1)
        .numeric(true)
        .build();
    let factor_sender = sketch_board_sender.clone();
    let factor_handler = factor_spin.connect_value_changed(move |btn| {
        // Persist + broadcast happens centrally in App's
        // AnnotationFactorChanged handler so the welcome modal (if
        // open) gets the value pushed in too.
        let value = btn.value() as f32;
        let _ = factor_sender.send(SketchBoardInput::Output(
            SketchBoardOutput::AnnotationFactorChanged(value),
        ));
    });
    factor_spin.set_tooltip_text(Some(
        "Scales the size of every annotation (text, line width, arrow heads, …). \
         Set this to roughly match your display scale; values above 1 enlarge.",
    ));
    factor_row.append(&factor_spin);
    outer.append(&factor_row);

    // Hand App a clone of the spin + its signal id so the welcome
    // dialog's live updates can push values in here. Clear on close so
    // App stops trying to update a destroyed widget.
    *prefs_factor_spin_slot.borrow_mut() = Some((factor_spin.clone(), factor_handler));
    let slot_for_close = prefs_factor_spin_slot.clone();
    dialog.connect_close_request(move |_| {
        slot_for_close.borrow_mut().take();
        relm4::gtk::glib::Propagation::Proceed
    });

    let invert_scroll_check = gtk::CheckButton::builder()
        .label("Invert scrolling direction")
        .active(APP_CONFIG.read().invert_scrolling())
        .build();
    invert_scroll_check.connect_toggled(|btn| {
        let value = btn.is_active();
        crate::state::save_invert_scrolling(value);
        APP_CONFIG.write().set_invert_scrolling(value);
    });
    outer.append(&invert_scroll_check);

    let close_on_esc_check = gtk::CheckButton::builder()
        .label("Close window on Esc")
        .active(APP_CONFIG.read().close_on_esc())
        .build();
    close_on_esc_check.connect_toggled(|btn| {
        let value = btn.is_active();
        crate::state::save_close_on_esc(value);
        APP_CONFIG.write().set_close_on_esc(value);
    });
    outer.append(&close_on_esc_check);

    let hide_palette_check = gtk::CheckButton::builder()
        .label("Hide default palette colors")
        .active(APP_CONFIG.read().hide_default_palette())
        .build();
    hide_palette_check.connect_toggled(|btn| {
        let value = btn.is_active();
        crate::state::save_hide_default_palette(value);
        APP_CONFIG.write().set_hide_default_palette(value);
    });
    outer.append(&hide_palette_check);

    // When on, per-tool adjustments (size, fill, highlighter opacity,
    // brush smoothness) stick across tool switches and only re-seed
    // from saved defaults on a fresh app launch. Off (default) keeps
    // the original snap-back-on-tool-switch behavior.
    let sticky_defaults_check = gtk::CheckButton::builder()
        .label("Keep in-session tool adjustments across tool switches")
        .tooltip_text(
            "When off, switching tools snaps each tool back to its saved default.\n\
             When on, your in-session size / fill / opacity tweaks persist until \
             you close the app.",
        )
        .active(APP_CONFIG.read().sticky_session_defaults())
        .build();
    sticky_defaults_check.connect_toggled(|btn| {
        let value = btn.is_active();
        crate::state::save_sticky_session_defaults(value);
        APP_CONFIG.write().set_sticky_session_defaults(value);
    });
    outer.append(&sticky_defaults_check);

    let button_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .margin_top(8)
        .build();
    let cancel_btn = gtk::Button::builder().label("Cancel").build();
    let dialog_for_cancel = dialog.clone();
    cancel_btn.connect_clicked(move |_| dialog_for_cancel.close());
    button_row.append(&cancel_btn);

    let save_btn = gtk::Button::builder()
        .label("Save")
        .css_classes(["suggested-action"])
        .build();
    let dialog_for_save = dialog.clone();
    let working_for_save = working.clone();
    save_btn.connect_clicked(move |_| {
        let map = working_for_save.borrow().clone();
        crate::state::save_keybinds(&map);
        APP_CONFIG.write().set_keybinds(map);
        dialog_for_save.close();
    });
    button_row.append(&save_btn);
    outer.append(&button_row);

    let outer_scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .propagate_natural_width(true)
        .max_content_height(max_dialog_h)
        .child(&outer)
        .build();
    dialog.set_child(Some(&outer_scroller));
    dialog.present();
}
