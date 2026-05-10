use relm4::{
    gtk::{self, prelude::*},
    ComponentParts, ComponentSender, SimpleComponent,
};

use crate::sketch_board::ZoomCommand;

/// Compact zoom dropdown that lives in the lower-left of the canvas.
///
/// Acts as both a *display* (label tracks the renderer's current
/// `scale_factor` via `SetCurrentZoom` from the parent) and a *control*
/// (the popover emits `ZoomCommand`s back through `ZoomIndicatorOutput`).
pub struct ZoomIndicator {
    /// Current effective scale factor (1.0 = 100%, 0.5 = 50%, etc.).
    /// Updated externally whenever the renderer reports a new scale.
    current_scale: f32,
}

#[derive(Debug, Clone, Copy)]
pub enum ZoomIndicatorInput {
    SetCurrentZoom(f32),
    Emit(ZoomCommand),
}

#[derive(Debug, Clone, Copy)]
pub enum ZoomIndicatorOutput {
    Command(ZoomCommand),
}

#[relm4::component(pub)]
impl SimpleComponent for ZoomIndicator {
    type Init = f32;
    type Input = ZoomIndicatorInput;
    type Output = ZoomIndicatorOutput;

    view! {
        #[name = "menu_button"]
        gtk::MenuButton {
            add_css_class: "zoom-indicator",
            add_css_class: "flat",
            set_focusable: false,
            set_halign: gtk::Align::Start,
            set_valign: gtk::Align::Center,
            set_margin_start: 8,
            set_margin_top: 4,
            set_margin_bottom: 4,

            #[watch]
            set_label: &format_zoom(model.current_scale),
        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = ZoomIndicator {
            current_scale: init,
        };
        let widgets = view_output!();

        // Build the popover ourselves so the rows can carry custom labels
        // and shortcuts; gio::Menu doesn't give us enough control over
        // styling.
        let popover = gtk::Popover::builder()
            .has_arrow(false)
            .position(gtk::PositionType::Top)
            .build();
        popover.add_css_class("zoom-indicator-popover");
        let list = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .build();

        let zoom_in = make_row("Zoom In", Some("Ctrl  ="));
        let zoom_out = make_row("Zoom Out", Some("Ctrl  −"));
        let fit = make_row("Fit Canvas", Some("Ctrl  1"));
        let p50 = make_row("50%", None);
        let p100 = make_row("100%", Some("Ctrl  0"));
        let p200 = make_row("200%", None);

        list.append(&zoom_in);
        list.append(&zoom_out);
        list.append(&separator());
        list.append(&fit);
        list.append(&separator());
        list.append(&p50);
        list.append(&p100);
        list.append(&p200);

        popover.set_child(Some(&list));
        widgets.menu_button.set_popover(Some(&popover));

        // Wire each row to send its command and dismiss the popover.
        wire_row(&zoom_in, &popover, sender.clone(), ZoomCommand::In);
        wire_row(&zoom_out, &popover, sender.clone(), ZoomCommand::Out);
        wire_row(&fit, &popover, sender.clone(), ZoomCommand::FitCanvas);
        wire_row(&p50, &popover, sender.clone(), ZoomCommand::Abs(0.5));
        wire_row(&p100, &popover, sender.clone(), ZoomCommand::Abs(1.0));
        wire_row(&p200, &popover, sender.clone(), ZoomCommand::Abs(2.0));

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            ZoomIndicatorInput::SetCurrentZoom(scale) => self.current_scale = scale,
            ZoomIndicatorInput::Emit(cmd) => {
                let _ = sender.output(ZoomIndicatorOutput::Command(cmd));
            }
        }
    }
}

fn format_zoom(scale: f32) -> String {
    let pct = (scale * 100.0).round() as i32;
    format!("{pct}%")
}

fn make_row(label: &str, shortcut: Option<&str>) -> gtk::Button {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(16)
        .hexpand(true)
        .build();
    let lbl = gtk::Label::builder()
        .label(label)
        .halign(gtk::Align::Start)
        .hexpand(true)
        .build();
    row.append(&lbl);
    if let Some(s) = shortcut {
        let s_lbl = gtk::Label::builder()
            .label(s)
            .halign(gtk::Align::End)
            .build();
        s_lbl.add_css_class("dim-label");
        row.append(&s_lbl);
    }
    let button = gtk::Button::builder()
        .child(&row)
        .focusable(false)
        .build();
    button.add_css_class("flat");
    button.add_css_class("zoom-indicator-row");
    button
}

fn separator() -> gtk::Separator {
    gtk::Separator::new(gtk::Orientation::Horizontal)
}

fn wire_row(
    row: &gtk::Button,
    popover: &gtk::Popover,
    sender: ComponentSender<ZoomIndicator>,
    cmd: ZoomCommand,
) {
    let popover = popover.clone();
    row.connect_clicked(move |_| {
        sender.input(ZoomIndicatorInput::Emit(cmd));
        popover.popdown();
    });
}
