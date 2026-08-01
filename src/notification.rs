use relm4::gtk::gio::{FileIcon, Icon};
use relm4::gtk::gio::{Notification, prelude::ApplicationExt};

use crate::TEMP_DIR;
use crate::configuration::APP_CONFIG;
use relm4::gtk::gdk_pixbuf::{InterpType, Pixbuf};
use relm4::gtk::prelude::Cast;
use relm4::gtk::{IconLookupFlags, IconTheme, TextDirection, gio};
use satty_cli::command_line::NotificationThumbnail;
use tempfile::NamedTempFile;

pub fn log_result(msg: &str, notify: bool) {
    eprintln!("{msg}");
    if notify && !APP_CONFIG.read().disable_notifications() {
        show_notification(msg, None);
    }
}

pub fn log_result_with_pixbuf(msg: &str, pixbuf: Pixbuf) {
    eprintln!("{msg}");

    if APP_CONFIG.read().disable_notifications() {
        return;
    }

    let notification_icon_kind = APP_CONFIG.read().notification_thumbnail();

    let pixbuf = match notification_icon_kind {
        NotificationThumbnail::AppIcon => None,
        _ => {
            let src_w = pixbuf.width();
            let src_h = pixbuf.height();

            if src_w == 0 || src_h == 0 {
                None
            } else {
                let scale = f64::min(96.0 / src_w as f64, 96.0 / src_h as f64);

                let new_w = ((src_w as f64) * scale).round().max(1.0) as i32;
                let new_h = ((src_h as f64) * scale).round().max(1.0) as i32;

                pixbuf.scale_simple(new_w, new_h, InterpType::Bilinear)
            }
        }
    };

    // we can't just use a tempfile here because we need the path for the FileIcon.
    // Also, we can't just use NamedTempFile with cleanup, because it gets dropped
    // at the end of this function, which can be too early for the notification daemon.
    let tempfile: Option<NamedTempFile> = match TEMP_DIR.read() {
        Ok(guard) => match &*guard {
            Some(d) if pixbuf.is_some() => {
                if let Ok(tf) = tempfile::Builder::new()
                    .disable_cleanup(true)
                    .suffix(".png")
                    .tempfile_in(d.path())
                {
                    Some(tf)
                } else {
                    eprintln!("Could not create temporary file");
                    None
                }
            }
            _ => None,
        },
        Err(e) => {
            eprintln!("Error acquiring read guard for temp directory: {}", e);
            None
        }
    };

    let icon = match tempfile.as_ref() {
        Some(f) => {
            // this unwrap is safe because tempfile would not be Some otherwise, see above
            if pixbuf.unwrap().savev(f, "png", &[]).is_ok() {
                let file = gio::File::for_path(f);
                Some(FileIcon::new(&file).upcast::<Icon>())
            } else {
                None
            }
        }
        _ => None,
    };

    show_notification(msg, icon);
}

fn show_notification(msg: &str, icon: Option<Icon>) {
    // construct
    let notification = Notification::new("Satty");
    notification.set_body(Some(msg));

    if let Some(i) = icon {
        notification.set_icon(&i);
    } else {
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
    }

    // send notification
    relm4::main_application().send_notification(None, &notification);
}
