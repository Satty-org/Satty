use relm4::gtk::gio::FileIcon;
use relm4::gtk::gio::{Notification, prelude::ApplicationExt};

use relm4::gtk::{IconLookupFlags, IconTheme, TextDirection};

pub fn log_result(msg: &str, notify: bool) {
    eprintln!("{msg}");
    if notify {
        show_notification(msg);
    }
}

fn show_notification(msg: &str) {
    // construct
    let notification = Notification::new("Satty");
    notification.set_body(Some(msg));

    // lookup sattys icon
    let theme = IconTheme::default();
    if theme.has_icon("satty")
        && let Some(icon_file) = theme
            .lookup_icon(
                "satty",
                &[],
                96,
                1,
                TextDirection::Ltr,
                IconLookupFlags::empty(),
            )
            .file()
    {
        notification.set_icon(&FileIcon::new(&icon_file));
    }

    // send notification
    relm4::main_application().send_notification(None, &notification);
}
