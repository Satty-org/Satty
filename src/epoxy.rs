use libloading::Library;
use std::ffi::c_void;
use std::sync::OnceLock;
use std::{panic, ptr};

static EPOXY: OnceLock<Library> = OnceLock::new();

pub fn load_epoxy() -> &'static Library {
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

        // if this fails, we can't start anyway so ok for now.
        library.unwrap()
    })
}

pub fn get_proc_addr(name: &str) -> *const c_void {
    let library = match panic::catch_unwind(load_epoxy) {
        Ok(library) => library,
        Err(_) => {
            eprintln!("failed to load epoxy");
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
