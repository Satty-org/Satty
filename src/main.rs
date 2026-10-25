use configuration::{APP_CONFIG, Configuration};
use std::io::Write;
use std::path::Path;
use std::process::exit;
use std::sync::{LazyLock, RwLock};
use std::time::SystemTime;
use std::{fs, thread};

use relm4::gtk::{self};

use anyhow::Result;
use satty_cli::command_line::LogLevel;
use tempfile::TempDir;

use crate::logging::{CONFIG_READ, LogSeverity, log, log_early_fatal_error, log_with_severity};

mod app;
mod configuration;
mod epoxy;
mod femtovg_area;
mod icons;
mod image_loading;
mod ime;
mod keybindings;
mod logging;
mod math;
mod notification;
mod sketch_board;
mod style;
mod tools;
mod ui;

pub static START_TIME: LazyLock<chrono::DateTime<chrono::Local>> =
    LazyLock::new(chrono::Local::now);

pub static TEMP_DIR: LazyLock<RwLock<Option<TempDir>>> =
    LazyLock::new(|| match tempfile::TempDir::new() {
        Ok(dir) => RwLock::new(Some(dir)),
        Err(e) => {
            log_with_severity(
                &format!("Failed to create temporary directory: {e}"),
                LogLevel::Error,
                LogSeverity::High,
            );
            RwLock::new(None)
        }
    });

macro_rules! generate_profile_output {
    ($e: expr) => {
        log(
            &format!(
                "{:5} ms time elapsed: {}",
                (chrono::Local::now() - *START_TIME).num_milliseconds(),
                $e
            ),
            LogLevel::Debug,
        );
    };
}
pub(crate) use generate_profile_output;

fn cleanup() -> Result<()> {
    let _ = std::io::stdout().lock().flush();
    let _ = std::io::stderr().lock().flush();

    match TEMP_DIR.write() {
        Ok(mut temp_dir) => {
            // take is sufficient to have the dir deleted when it's dropped.
            // But that would hide any errors, so use explicit close instead.
            if let Some(dir) = temp_dir.take() {
                let grace_period = APP_CONFIG.read().notification_grace_period();
                // get the last modified file from the temp directory, with its timestamp
                // we can determine if we sent a notification recently.
                // this works so long as we only have notification thumbs there.
                if let Some(mtime) = newest_mtime(dir.path())
                    && let Ok(elapsed) = SystemTime::now().duration_since(mtime)
                    && elapsed < grace_period
                {
                    let sleep_time = grace_period - elapsed;
                    log(
                        &format!(
                            "last notification sent {:?} ago, sleeping additional {:?}.",
                            elapsed, sleep_time
                        ),
                        LogLevel::Debug,
                    );
                    thread::sleep(sleep_time);
                }
                dir.close()?;
            }
            Ok(())
        }
        Err(e) => Err(anyhow::anyhow!("{}", e)),
    }
}

fn newest_mtime(dir: &Path) -> Option<SystemTime> {
    fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.metadata().ok())
        .filter_map(|meta| meta.modified().ok())
        .max()
}

fn main() -> Result<()> {
    let _ = *START_TIME;
    // populate the APP_CONFIG from commandline and
    // config file. this might exit, if an error occurred.
    Configuration::load();
    _ = CONFIG_READ.set(());
    if APP_CONFIG.read().man() {
        print!(include_str!(concat!(env!("OUT_DIR"), "/satty.1")));
        exit(0);
    }
    if APP_CONFIG.read().license() {
        print!(include_str!("../LICENSE"));
        exit(0);
    }

    log(
        &format!(
            "startup timestamp was {}",
            START_TIME.format("%s.%f %Y-%m-%d %H:%M:%S")
        ),
        LogLevel::Debug,
    );
    generate_profile_output!("configuration loaded");

    let mut fatal_error_message: Option<String> = None;

    if let Err(e) = epoxy::load_epoxy() {
        fatal_error_message = Some(format!("Error: {}", e));
    }
    generate_profile_output!("loaded gl");

    let pixbuf = if fatal_error_message.is_none() {
        match image_loading::load_image() {
            Ok(image) => Some(image),
            Err(e) => {
                fatal_error_message = Some(format!("Error: {}", e));
                None
            }
        }
    } else {
        None
    };

    // run the application, funny enough, this does not have anything result worthy.
    let r = if let Some(pb) = pixbuf {
        app::run_satty_app(pb);
        Ok(())
    } else if let Some(msg) = fatal_error_message {
        // not much else we can do if it isn't
        if gtk::init().is_ok() {
            let main_loop = relm4::gtk::glib::MainLoop::new(None, false);
            let parent = gtk::Window::new();
            log_early_fatal_error(&msg, &main_loop, parent);
            main_loop.run();
        }
        Err(anyhow::anyhow!("errors occurred"))
    } else {
        Ok(())
    };

    if let Err(e) = cleanup() {
        log(&format!("{}", e), LogLevel::Error);
        if r.is_ok() {
            return Err(anyhow::anyhow!("errors occurred"));
        }
    }
    r
}
