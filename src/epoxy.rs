use crate::logging::{LogSeverity, log_with_severity};
use anyhow::{Error, anyhow};
use libloading::Library;
use satty_cli::command_line::LogLevel;
use std::ffi::c_void;
use std::ptr;
use std::sync::OnceLock;

static EPOXY: OnceLock<Result<Library, Error>> = OnceLock::new();

pub fn load_epoxy() -> &'static Result<Library, Error> {
    // TODO: rework with get_or_try_init once
    // https://github.com/rust-lang/rust/issues/109737
    // becomes stable
    EPOXY.get_or_init(|| {
        #[cfg(target_os = "macos")]
        let filename = "libepoxy.0.dylib";
        #[cfg(all(unix, not(target_os = "macos")))]
        let filename = "libepoxy.so.0";
        #[cfg(windows)]
        let filename = "libepoxy-0.dll";

        let library = unsafe { Library::new(filename) };

        #[cfg(windows)]
        let library = library.or_else(|_| unsafe { Library::new("epoxy-0.dll") });

        match library {
            Ok(library) => Ok(library),
            Err(e) => Err(anyhow!("failed to load {}: {}", filename, e)),
        }
    })
}

pub fn get_proc_addr(name: &str) -> *const c_void {
    let library = match load_epoxy() {
        Ok(library) => library,
        Err(e) => {
            log_with_severity(&format!("{e}"), LogLevel::Error, LogSeverity::Urgent);
            return ptr::null();
        }
    };

    let symbol = format!("epoxy_{name}");

    unsafe {
        library
            .get::<*const c_void>(&symbol)
            .map(|sym| {
                let entry = sym.try_as_raw_ptr().unwrap() as *const *const c_void;
                *entry
            })
            .unwrap_or(ptr::null())
    }
}
