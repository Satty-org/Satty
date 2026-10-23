#[cfg(feature = "fatal-error-dialog")]
use std::process::Command;
use std::sync::OnceLock;

use crate::configuration::APP_CONFIG;
use crate::notification::{show_notification, show_notification_with_pixbuf};
use crate::{APP_BROKER, AppInput, ui};
use chrono::Local;
use relm4::gtk;
use relm4::gtk::gdk_pixbuf::Pixbuf;
use relm4::gtk::gio;
use relm4::gtk::prelude::{DialogExt, GtkApplicationExt, GtkWindowExt, WidgetExt};
pub use satty_cli::command_line::LogLevel;

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
        }
    }
}

pub fn log(msg: &str, level: LogLevel) {
    // avoid blocking before config is read, hopefully ;)
    if !CONFIG_READ.get().is_some() || level <= APP_CONFIG.read().log_level() {
        let ts = Local::now().format("%Y-%m-%dT%H:%M:%S");
        let pfx = level.prefix();
        let output = format!("{ts} {pfx} {msg}");
        eprintln!("{output}");
    }
}

#[cfg(feature = "fatal-error-dialog")]
fn spawn_fatal_dialog(msg: &str) {
    let helper = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("satty-fatal-error-dialog")));
    if let Some(helper) = helper {
        let _ = Command::new(helper).arg(msg).status();
    }
}

pub fn log_with_severity(msg: &str, level: LogLevel, severity: LogSeverity) {
    log(msg, level);

    if severity == LogSeverity::Normal {
        return;
    }

    let can_read_config = CONFIG_READ.get().is_some();
    let has_gtk = gtk::is_initialized();

    if !can_read_config || !has_gtk {
        #[cfg(feature = "fatal-error-dialog")]
        spawn_fatal_dialog(msg);
    } else if level == LogLevel::Fatal {
        // if it's fatal, we shouldn't rely on toast or notification
        let app = relm4::main_application();
        let message_dialog = ui::common_dialogs::build_fatal_error_dialog(msg, app.active_window());
        message_dialog.connect_response(|d, _| {
            d.destroy();
        });
        message_dialog.show();
    } else if APP_CONFIG.read().disable_notifications() {
        APP_BROKER.send(AppInput::ShowToast(msg.to_owned()));
    } else {
        show_notification(msg, None, severity.into());
    }
}

pub fn log_result_with_pixbuf(msg: &str, pixbuf: Pixbuf) {
    log(msg, LogLevel::Info);

    if APP_CONFIG.read().disable_notifications() {
        APP_BROKER.send(AppInput::ShowToast(msg.to_owned()));
    } else {
        show_notification_with_pixbuf(msg, pixbuf);
    }
}
