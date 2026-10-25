use crate::logging::{LogEntry, log_buffer_snapshot};
use relm4::gtk::prelude::{
    BoxExt, ButtonExt, GtkApplicationExt, GtkWindowExt, OrientableExt, TextBufferExt, TextViewExt,
    WidgetExt,
};
use relm4::{
    Component, ComponentController, ComponentParts, ComponentSender, Controller, RelmWidgetExt, gtk,
};
use satty_cli::command_line::LogLevel;
use std::cell::RefCell;

pub struct LogDialog {
    entries: Vec<LogEntry>,
    max_displayed_level: LogLevel,
}

const LEVELS: [&str; 6] = ["Fatal", "Error", "Warn", "Info", "Debug", "Trace"];

impl LogDialog {
    fn get_log_text(&self) -> String {
        let mut text = String::new();
        for entry in &self.entries {
            if entry.needs_printing(self.max_displayed_level) {
                text.push_str(&format!("{}\n", entry));
            }
        }
        text
    }
}

#[derive(Debug)]
pub enum LogDialogInput {
    ToggleLevel(u32),
    Refresh,
}

#[relm4::component(pub)]
impl Component for LogDialog {
    type Init = ();
    type Input = LogDialogInput;
    type Output = ();
    type CommandOutput = ();

    view! {
        #[root]
        gtk::Window {
            set_title: Some("Satty log"),
            set_default_width: 800,
            set_default_height: 600,

            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_spacing: 6,
                set_margin_all: 6,

                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 6,

                    gtk::DropDown::from_strings(&LEVELS) {
                        set_selected: model.max_displayed_level.index(),
                        connect_selected_notify[sender] => move |dd| {
                            sender.input(LogDialogInput::ToggleLevel(dd.selected()))
                        }
                    },

                    gtk::Button {
                        set_label: "Refresh",
                        connect_clicked => LogDialogInput::Refresh
                    }
                },

                gtk::ScrolledWindow {
                    set_vexpand: true,
                    set_hexpand: true,

                    #[name(log_view)]
                    gtk::TextView {
                        set_editable: false,
                        set_cursor_visible: false,
                        set_monospace: true,
                        set_wrap_mode: gtk::WrapMode::WordChar,

                        #[wrap(Some)]
                        set_buffer = &gtk::TextBuffer {
                            #[watch]
                            set_text: &model.get_log_text(),
                        }
                    }
                },
            }
        }
    }

    fn init(
        _: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = LogDialog {
            entries: log_buffer_snapshot(),
            max_displayed_level: LogLevel::Warn,
        };

        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: Self::Input,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match message {
            LogDialogInput::Refresh => {
                self.entries = log_buffer_snapshot();
            }
            LogDialogInput::ToggleLevel(level) => {
                if let Some(l) = LogLevel::from_index(level) {
                    self.max_displayed_level = l;
                }
            }
        }
        self.update_view(widgets, sender);
    }
}

pub fn show_log_window() {
    thread_local! {
        static DIALOG: RefCell<Option<Controller<LogDialog>>> = const { RefCell::new(None) };

    }
    DIALOG.with(|cell| {
        let mut slot = cell.borrow_mut();
        let controller = slot.get_or_insert_with(|| {
            let controller = LogDialog::builder().launch(()).detach();
            if let Some(active) = relm4::main_application().active_window() {
                controller.widget().set_transient_for(Some(&active));
            }
            controller
        });
        controller.sender().send(LogDialogInput::Refresh).ok();
        controller.widget().present();
    })
}
