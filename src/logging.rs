use relm4::gtk::gio::{FileIcon, Icon};
use relm4::gtk::gio::{Notification, prelude::ApplicationExt};
use std::sync::OnceLock;

use crate::configuration::APP_CONFIG;
use crate::{APP_BROKER, AppInput, TEMP_DIR};
use chrono::Local;
use relm4::gtk;
use relm4::gtk::gdk_pixbuf::{InterpType, Pixbuf};
use relm4::gtk::prelude::{Cast, WidgetExt};
use relm4::gtk::{
    ButtonsType, IconLookupFlags, IconTheme, MessageDialog, MessageType, TextDirection, gio,
};
pub use satty_cli::command_line::{LogLevel, NotificationThumbnail};
use tempfile::NamedTempFile;

pub static CONFIG_READ: OnceLock<()> = OnceLock::new();

trait Prefix {
    fn prefix(&self) -> &'static str;
}

impl Prefix for LogLevel {
    fn prefix(&self) -> &'static str {
        match self {
            LogLevel::Fatal => "💀",
            LogLevel::Error => "✖",
            LogLevel::Warn => "⚠",
            LogLevel::Info => "ℹ",
            LogLevel::Debug => "⚙",
            LogLevel::Trace => "…",
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogSeverity {
    #[default]
    Low,
    Normal,
    High,
    Urgent,
}

impl From<LogSeverity> for gio::NotificationPriority {
    fn from(val: LogSeverity) -> Self {
        match val {
            LogSeverity::Urgent => gio::NotificationPriority::Urgent,
            LogSeverity::High => gio::NotificationPriority::High,
            LogSeverity::Normal => gio::NotificationPriority::Normal,
            LogSeverity::Low => gio::NotificationPriority::Low, // not really used, but ok
        }
    }
}

pub fn log(msg: &str, level: LogLevel, severity: LogSeverity) {
    let can_read_config = CONFIG_READ.get().is_some();
    let has_gtk = gtk::is_initialized();

    // avoid blocking before config is read, hopefully ;)
    if !can_read_config || level <= APP_CONFIG.read().log_level() {
        let ts = Local::now().format("%Y-%m-%dT%H:%M:%S");
        let pfx = level.prefix();
        let output = format!("{ts} {pfx} {msg}");
        eprintln!("{output}");
    }

    if severity >= LogSeverity::High {
        if !can_read_config || !has_gtk {
            eprintln!("{msg}"); //TODO
        } else if APP_CONFIG.read().disable_notifications() {
            APP_BROKER.send(AppInput::ShowToast(msg.to_owned()));
        } else {
            show_notification(msg, None, severity);
        }
    }

    // if it's fatal, we shouldn't rely on toast or notification
    if has_gtk && level == LogLevel::Fatal {
        let message_dialog = MessageDialog::builder()
            .title("Fatal error")
            .message_type(MessageType::Error)
            .buttons(ButtonsType::Close)
            .text(msg)
            .modal(true)
            .build();
        message_dialog.show();
    }
}

pub fn log_result_with_pixbuf(msg: &str, pixbuf: Pixbuf) {
    //TODO partially duplicated
    let ts = Local::now().format("%Y-%m-%dT%H:%M:%S");
    let pfx = LogLevel::Info.prefix();
    let output = format!("{ts} {pfx} {msg}");
    eprintln!("{output}");

    if APP_CONFIG.read().disable_notifications() {
        APP_BROKER.send(AppInput::ShowToast(msg.to_owned()));
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

    show_notification(msg, icon, LogSeverity::Normal);
}

fn show_notification(msg: &str, icon: Option<Icon>, severity: LogSeverity) {
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
            notification.set_priority(severity.into());
        }
    }

    // send notification
    relm4::main_application().send_notification(None, &notification);
}
