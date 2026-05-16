//! First-run welcome dialog. Forces the user to commit a value to the
//! persisted `annotation_size_factor` before they can use the app, so
//! Satty doesn't silently render mismatched annotations on a HiDPI
//! display. The dialog explains what the factor controls, reports the
//! detected display scale (from Hyprland when available), pre-fills the
//! `SpinButton` with that scale, and exposes only a Save action — close
//! requests are re-routed into Save so the user always exits with a
//! committed value.

use std::cell::Cell;
use std::rc::Rc;

use relm4::gtk::prelude::*;
use relm4::{Component, ComponentParts, ComponentSender, RelmWidgetExt, gtk};

#[derive(Debug)]
pub struct WelcomeDialog {
    /// Value currently shown in the SpinButton — what we'll persist on
    /// Save.
    value: f32,
    /// The detected display scale, kept around so "Use detected" can
    /// snap back to it after manual editing.
    detected_scale: f32,
    /// Display-scale source string ("Hyprland" or "default") for the
    /// hint label.
    source: &'static str,
    /// Set true once the user has clicked Save (or used Enter / X /
    /// WM close). The `close-request` signal handler short-circuits
    /// to `Propagation::Proceed` when this is set, breaking the
    /// "Save → root.close() → close-request → Save → …" recursion
    /// that the always-route-close-to-Save design otherwise creates.
    /// `Rc<Cell>` because the close-request closure needs its own
    /// reference (closures can't borrow `&self.saving` across the
    /// signal boundary).
    saving: Rc<Cell<bool>>,
}

#[derive(Debug)]
pub struct WelcomeDialogInit {
    /// Best guess for the user's scale. Falls back to 1.0 when no
    /// signal is available (non-Hyprland or hyprctl missing).
    pub detected_scale: f32,
    /// Whether `detected_scale` came from a real probe or just the
    /// fallback. Drives the wording on the hint label.
    pub detected: bool,
}

#[derive(Debug)]
pub enum WelcomeDialogInput {
    ValueChanged(f32),
    /// Push a new value in from outside (e.g. the user just edited
    /// the matching SpinButton in the Preferences dialog). Updates
    /// `self.value` so the watched `set_value` binding re-renders;
    /// `#[block_signal(value_changed)]` on the view! suppresses the
    /// re-emission so this doesn't loop back out as `ValueChanged`.
    SetValue(f32),
    UseDetected,
    UseDefault,
    Save,
}

#[derive(Debug)]
pub enum WelcomeDialogOutput {
    /// User clicked Save. Carries the value that should be persisted
    /// and applied as the live annotation factor.
    Saved(f32),
    /// User edited the spin without (yet) saving. Emitted on every
    /// change so the Preferences dialog can mirror the value live —
    /// the welcome modal isn't isolated from APP_CONFIG, and we don't
    /// want the two surfaces to drift while both are open.
    ValueChanged(f32),
}

#[relm4::component(pub)]
impl Component for WelcomeDialog {
    type Init = WelcomeDialogInit;
    type Input = WelcomeDialogInput;
    type Output = WelcomeDialogOutput;
    type CommandOutput = ();

    view! {
        gtk::Window {
            set_modal: true,
            set_resizable: false,
            set_title: Some("Welcome to Tensaku"),
            set_default_width: 460,

            connect_close_request[sender, saving_for_close] => move |_| {
                // The Save handler eventually calls `root.close()` to
                // dismiss the dialog, which re-emits close-request.
                // Without this guard, we'd route that re-emission
                // back into Save and infinite-loop. Once `saving` is
                // flipped on by the first Save, every subsequent
                // close-request just lets the window go.
                if saving_for_close.get() {
                    return relm4::gtk::glib::Propagation::Proceed;
                }
                // Re-route the close (X / WM close) into a Save so the
                // user always exits with a committed value.
                sender.input(WelcomeDialogInput::Save);
                relm4::gtk::glib::Propagation::Stop
            },

            #[wrap(Some)]
            set_child = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_spacing: 12,
                set_margin_all: 18,

                gtk::Label {
                    set_wrap: true,
                    set_xalign: 0.0,
                    set_use_markup: true,
                    set_label: "<b>Pick a default annotation size factor</b>",
                },

                gtk::Label {
                    set_wrap: true,
                    set_xalign: 0.0,
                    set_label: "Tensaku sizes annotations (text, line width, arrow heads, blur radius, …) in image-space pixels, not relative to the screenshot's dimensions. To compensate for high-DPI screenshots, set a factor that matches your display scale.",
                },

                gtk::Label {
                    set_wrap: true,
                    set_xalign: 0.0,
                    set_use_markup: true,
                    #[watch]
                    set_label: &format!(
                        "Detected display scale: <b>{:.2}×</b> ({source}). \
                         The field below is pre-filled to match — adjust if \
                         you want bigger or smaller annotations.",
                        model.detected_scale,
                        source = model.source,
                    ),
                },

                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 6,

                    gtk::Label { set_label: "Factor:" },

                    #[name = "spin"]
                    gtk::SpinButton {
                        set_editable: true,
                        set_numeric: true,
                        // Range / step / digits all match the
                        // Preferences spin so values round-trip cleanly
                        // between the two surfaces — a 0.05 step here
                        // would let the welcome modal land on values
                        // (e.g. 1.45) that the 1-digit prefs spin can't
                        // represent, breaking the live-sync invariant.
                        set_adjustment: &gtk::Adjustment::new(1.0, 0.10, 10.0, 0.10, 0.50, 0.0),
                        set_climb_rate: 0.1,
                        set_digits: 1,
                        set_hexpand: true,

                        #[watch]
                        #[block_signal(value_changed)]
                        set_value: model.value.into(),

                        connect_value_changed[sender] => move |btn| {
                            sender.input(WelcomeDialogInput::ValueChanged(btn.value() as f32));
                        } @value_changed,
                    },

                    gtk::Button {
                        set_label: "Use detected",
                        set_tooltip_text: Some("Reset to the detected display scale"),
                        connect_clicked[sender] => move |_| {
                            sender.input(WelcomeDialogInput::UseDetected);
                        },
                    },

                    gtk::Button {
                        set_label: "1.00×",
                        set_tooltip_text: Some("Reset to the unscaled default"),
                        connect_clicked[sender] => move |_| {
                            sender.input(WelcomeDialogInput::UseDefault);
                        },
                    },
                },

                gtk::Label {
                    set_wrap: true,
                    set_xalign: 0.0,
                    set_use_markup: true,
                    set_label: "<i>You need to save a value to continue. This dialog won't reappear once a factor is persisted — change it later in Preferences (Ctrl+,) or via Alt+scroll on the canvas.</i>",
                },

                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_halign: gtk::Align::End,
                    set_spacing: 6,

                    gtk::Button {
                        set_label: "Save and continue",
                        add_css_class: "suggested-action",
                        connect_clicked[sender] => move |_| {
                            sender.input(WelcomeDialogInput::Save);
                        },
                    },
                },
            },
        }
    }

    fn init(
        init: WelcomeDialogInit,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let WelcomeDialogInit {
            detected_scale,
            detected,
        } = init;
        let saving = Rc::new(Cell::new(false));
        // Cloned for the close-request closure captured by the view!
        // macro below — keeps the close handler in sync with `Save`.
        let saving_for_close = saving.clone();
        let model = WelcomeDialog {
            value: detected_scale,
            detected_scale,
            source: if detected { "Hyprland" } else { "default" },
            saving,
        };
        let widgets = view_output!();

        // Enter saves so the user can confirm with the keyboard. The
        // controller runs in Capture phase so it fires before the
        // SpinButton's own activation behavior.
        let key_controller = gtk::EventControllerKey::builder()
            .propagation_phase(gtk::PropagationPhase::Capture)
            .build();
        let sender_clone = sender.clone();
        key_controller.connect_key_pressed(move |_, keyval, _, _| {
            use gtk::gdk::Key;
            if keyval == Key::Return || keyval == Key::KP_Enter {
                sender_clone.input(WelcomeDialogInput::Save);
                relm4::gtk::glib::Propagation::Stop
            } else {
                relm4::gtk::glib::Propagation::Proceed
            }
        });
        root.add_controller(key_controller);

        // GTK4 Windows default to hidden — without this, the dialog
        // would be built but never shown. This dialog is a one-shot
        // launched once at startup, so showing in init is the right
        // place (no separate Show message needed).
        root.present();

        ComponentParts { model, widgets }
    }

    fn update(
        &mut self,
        message: WelcomeDialogInput,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            WelcomeDialogInput::ValueChanged(v) => {
                self.value = v;
                // Notify the outside world so the Preferences dialog
                // (if open) can mirror this value live. The watched
                // `set_value` binding in the view won't fire because
                // `self.value` already matches what the spin reports.
                let _ = sender
                    .output_sender()
                    .send(WelcomeDialogOutput::ValueChanged(v));
            }
            WelcomeDialogInput::SetValue(v) => {
                // Don't re-emit ValueChanged here — we got pushed FROM
                // the outside, echoing back would loop. The view's
                // `#[block_signal(value_changed)]` on `set_value`
                // covers the spin-button signal layer.
                self.value = v;
            }
            WelcomeDialogInput::UseDetected => {
                self.value = self.detected_scale;
                let _ = sender
                    .output_sender()
                    .send(WelcomeDialogOutput::ValueChanged(self.value));
            }
            WelcomeDialogInput::UseDefault => {
                self.value = 1.0;
                let _ = sender
                    .output_sender()
                    .send(WelcomeDialogOutput::ValueChanged(self.value));
            }
            WelcomeDialogInput::Save => {
                if self.saving.get() {
                    // Already saved (re-entry from close-request after
                    // a previous Save). Nothing to do — the close is
                    // already in flight.
                    return;
                }
                self.saving.set(true);
                let _ = sender
                    .output_sender()
                    .send(WelcomeDialogOutput::Saved(self.value));
                root.close();
            }
        }
    }
}
