use std::collections::VecDeque;
use std::fmt::Display;
use std::sync::{LazyLock, Mutex, OnceLock};

use crate::app::{APP_BROKER, AppInput};
use crate::configuration::APP_CONFIG;
use crate::notification::{show_notification, show_notification_with_pixbuf};
use crate::ui::common_dialogs;
use chrono::{DateTime, Local};
use relm4::gtk;
use relm4::gtk::gdk_pixbuf::Pixbuf;
use relm4::gtk::gio;
use relm4::gtk::glib::MainLoop;
use relm4::gtk::prelude::{ApplicationExt, DialogExt, GtkApplicationExt, GtkWindowExt, WidgetExt};
pub use satty_cli::command_line::LogLevel;

pub static CONFIG_READ: OnceLock<()> = OnceLock::new();

const LOG_BUFFER_LENGTH: usize = 4096;

static LOG_BUFFER: LazyLock<Mutex<VecDeque<LogEntry>>> =
    LazyLock::new(|| Mutex::new(VecDeque::with_capacity(LOG_BUFFER_LENGTH)));

pub fn log_buffer_snapshot() -> Vec<LogEntry> {
    LOG_BUFFER
        .lock()
        .map(|buf| buf.iter().cloned().collect())
        .unwrap_or_default()
}

#[derive(Clone)]
pub struct LogEntry {
    pub timestamp: DateTime<Local>,
    pub level: LogLevel,
    pub message: String,
}

impl LogEntry {
    fn new_with_timestamp(msg: &str, level: LogLevel) -> LogEntry {
        LogEntry {
            timestamp: Local::now(),
            level,
            message: msg.to_owned(),
        }
    }

    pub(crate) fn needs_printing(&self, max_log_level: LogLevel) -> bool {
        self.level <= max_log_level
    }

    fn print_level_based(&self, max_log_level: LogLevel) {
        if self.needs_printing(max_log_level) {
            eprintln!("{}", self);
        }
    }
}

impl Display for LogEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} {}",
            self.timestamp.format("%Y-%m-%dT%H:%M:%S"),
            self.level.prefix(),
            self.message
        )
    }
}

pub(crate) trait Prefix {
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
    let log_entry = LogEntry::new_with_timestamp(msg, level);

    // avoid blocking before config is read, hopefully ;)
    let max_level = if !CONFIG_READ.get().is_some() {
        LogLevel::Warn
    } else {
        APP_CONFIG.read().log_level()
    };

    log_entry.print_level_based(max_level);

    if let Ok(mut buf) = LOG_BUFFER.lock() {
        if buf.len() >= LOG_BUFFER_LENGTH {
            buf.pop_front();
        }
        buf.push_back(log_entry);
    }
}

pub fn show_fatal_error_dialog(msg: &str) {
    let app = relm4::main_application();
    let message_dialog = common_dialogs::build_fatal_error_dialog(msg, app.active_window());

    message_dialog.connect_response(|d, _| {
        d.destroy();
        relm4::main_application().quit();
    });
    message_dialog.connect_response(|d, _| {
        d.destroy();
        relm4::main_application().quit();
    });
    message_dialog.show();
}

pub fn log_early_fatal_error(msg: &str, main_loop: &MainLoop, parent: gtk::Window) {
    log(msg, LogLevel::Fatal);
    let message_dialog = common_dialogs::build_fatal_error_dialog(msg, Some(parent));

    let ml_clone = main_loop.clone();
    message_dialog.connect_response(move |d, _| {
        d.destroy();
        ml_clone.quit();
    });

    let ml_clone = main_loop.clone();
    message_dialog.connect_response(move |d, _| {
        d.destroy();
        ml_clone.quit();
    });
    message_dialog.show();
}

pub fn log_with_severity(msg: &str, level: LogLevel, severity: LogSeverity) {
    log(msg, level);

    if severity == LogSeverity::Normal || CONFIG_READ.get().is_none() || !gtk::is_initialized() {
        return;
    }

    if level == LogLevel::Fatal {
        show_fatal_error_dialog(msg);
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
