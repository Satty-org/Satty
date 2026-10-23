use relm4::gtk::glib;
use relm4::gtk::prelude::{DialogExt, GtkWindowExt, WidgetExt};

#[path = "../ui/common_dialogs.rs"]
mod dialogs;

fn main() {
    let msg = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Unknown error".into());

    if relm4::gtk::init().is_err() {
        eprintln!("fatal: {msg}");
        std::process::exit(1);
    }

    let main_loop = glib::MainLoop::new(None, false);

    let message_dialog = dialogs::build_fatal_error_dialog(&msg, None);

    let ml = main_loop.clone();
    message_dialog.connect_response(move |d, _| {
        d.close();
        ml.quit();
    });

    let ml = main_loop.clone();
    message_dialog.connect_close_request(move |_| {
        ml.quit();
        glib::Propagation::Proceed
    });

    message_dialog.show();

    main_loop.run();
}
