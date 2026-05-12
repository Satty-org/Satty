//! Preferences dialog — keyboard-shortcut customization.
//!
//! Phase 1 (this commit): the dialog is a placeholder shell so the
//! gear button + Ctrl+, shortcut have somewhere to land. The full
//! shortcut recorder + persistence + double-press cycle come in
//! follow-up commits.

use relm4::gtk;
use relm4::gtk::prelude::*;

/// Open the Preferences dialog, parented (transient) to `root` so the
/// window manager treats it as a modal child of the main satty window.
///
/// `root` is the App's root `gtk::CenterBox` (or whichever widget the
/// view! macro mounts); we ascend to the toplevel `Window` to attach
/// the transient relationship correctly.
pub fn open<W: IsA<gtk::Widget>>(root: &W) {
    let toplevel = root
        .root()
        .and_then(|r| r.downcast::<gtk::Window>().ok());

    let dialog = gtk::Window::builder()
        .title("Preferences")
        .modal(true)
        .destroy_with_parent(true)
        .default_width(420)
        .default_height(360)
        .resizable(false)
        .build();
    if let Some(w) = &toplevel {
        dialog.set_transient_for(Some(w));
    }

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

    let placeholder = gtk::Label::builder()
        .label(
            "Per-tool shortcut customization is coming. \
             For now this dialog confirms the gear button + Ctrl+, wiring.",
        )
        .wrap(true)
        .xalign(0.0)
        .build();
    placeholder.add_css_class("dim-label");
    outer.append(&placeholder);

    let button_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .margin_top(8)
        .build();
    let close_btn = gtk::Button::builder().label("Close").build();
    let dialog_for_close = dialog.clone();
    close_btn.connect_clicked(move |_| dialog_for_close.close());
    button_row.append(&close_btn);
    outer.append(&button_row);

    dialog.set_child(Some(&outer));
    dialog.present();
}
