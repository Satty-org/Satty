use crate::tools::{GroupableTool, Tools};
use crate::ui::toolbars::ToolsAction;
use relm4::actions::ActionablePlus;
use relm4::factory::{DynamicIndex, FactoryComponent};
use relm4::gtk::prelude::{BoxExt, ButtonExt, GestureExt, GestureSingleExt, PopoverExt, WidgetExt};
use relm4::gtk::{Align, Popover, ToggleButton};
use relm4::{FactorySender, RelmWidgetExt, gtk, view};

pub struct ToolGroupInit {
    pub group: Vec<GroupableTool>,
    pub initial_tool: Tools,
}

pub struct ToolGroupWidgets {
    button: ToggleButton,
}

#[derive(Debug, Clone)]
pub struct ToolGroupButton {
    group: Vec<GroupableTool>,
    current: usize,
    editing: bool,
    is_active: bool,
    popover: Popover,
}

#[derive(Debug, Clone)]
pub enum ToolGroupButtonInput {
    OpenPopover,
    SelectedToolChanged(Tools),
    SetEditing(bool),
}

impl ToolGroupButton {
    pub fn update_active_tool(&self, widgets: &ToolGroupWidgets) {
        if self.current < self.group.len()
            && let g = &self.group[self.current]
        {
            widgets.button.set_icon_name(&g.icon_name);
            let tooltip = if self.has_extra() {
                format!("{}\n\n{}", g.tooltip, "right-click for more tools")
            } else {
                g.tooltip.clone()
            };
            widgets.button.set_tooltip(&tooltip);
            ActionablePlus::set_action::<ToolsAction>(&widgets.button, g.tool);
        }
    }

    pub fn update_editing(&self, widgets: &ToolGroupWidgets) {
        if self.editing {
            widgets.button.add_css_class("editing");
        } else {
            widgets.button.remove_css_class("editing");
        }
    }

    pub fn has_extra(&self) -> bool {
        self.group.len() > 1
    }
}

impl FactoryComponent for ToolGroupButton {
    type Init = ToolGroupInit;
    type Input = ToolGroupButtonInput;
    type Output = ();
    type CommandOutput = ();
    type Root = relm4::gtk::Overlay;
    type Widgets = ToolGroupWidgets;
    type ParentWidget = relm4::gtk::Box;
    type Index = DynamicIndex;

    fn init_model(init: Self::Init, _index: &DynamicIndex, _sender: FactorySender<Self>) -> Self {
        let mut pos: usize = 0;
        let mut is_active = false;

        if let Some(p) = init.group.iter().position(|t| t.tool == init.initial_tool) {
            pos = p;
            is_active = true;
        }

        Self {
            group: init.group,
            current: pos,
            editing: false,
            is_active,
            popover: relm4::gtk::Popover::new(),
        }
    }

    fn init_root(&self) -> Self::Root {
        gtk::Overlay::new()
    }

    fn init_widgets(
        &mut self,
        _index: &DynamicIndex,
        root: Self::Root,
        _returned_widget: &relm4::gtk::Widget,
        sender: FactorySender<Self>,
    ) -> Self::Widgets {
        view! {
            #[local_ref]
            root -> relm4::gtk::Overlay {
                #[wrap(Some)]
                set_child: button = &ToggleButton {
                    set_focusable: false,
                    set_valign: Align::End,
                    set_halign: Align::Center,
                    add_controller = gtk::GestureClick {
                        set_button: 3,
                        connect_pressed[sender] => move |gesture, _, _, _| {
                            gesture.set_state(relm4::gtk::EventSequenceState::Claimed);
                            sender.input(ToolGroupButtonInput::OpenPopover);
                        }
                    }
                },
                add_overlay = &relm4::gtk::Image {
                    set_icon_name: Some("caret-down-right-filled"),
                    set_pixel_size: 8,
                    set_halign: Align::End,
                    set_valign: Align::End,
                    set_can_target: false,
                    set_visible: self.has_extra(),
                }
            },
        }

        let rows = gtk::Box::new(gtk::Orientation::Vertical, 2);
        for tool in &self.group {
            let button = ToggleButton::builder()
                .focusable(false)
                .icon_name(&tool.icon_name)
                .label(&tool.tooltip)
                .tooltip_text(&tool.tooltip)
                .build();
            let popover = self.popover.clone();
            button.connect_clicked(move |_| popover.popdown());
            ActionablePlus::set_action::<ToolsAction>(&button, tool.tool);
            rows.append(&button);
        }
        self.popover.set_child(Some(&rows));
        self.popover.set_position(relm4::gtk::PositionType::Bottom);
        self.popover.set_parent(&root);

        let widgets = ToolGroupWidgets { button };
        self.update_active_tool(&widgets);

        widgets
    }

    fn update(&mut self, message: Self::Input, _sender: FactorySender<Self>) {
        match message {
            ToolGroupButtonInput::OpenPopover => {
                if self.has_extra() {
                    self.popover.popup();
                }
            }
            ToolGroupButtonInput::SelectedToolChanged(tools) => {
                if let Some(i) = self.group.iter().position(|gt| gt.tool == tools) {
                    self.is_active = true;
                    self.current = i;
                } else {
                    self.is_active = false;
                    self.editing = false;
                }
            }
            ToolGroupButtonInput::SetEditing(editing) => {
                if self.is_active {
                    self.editing = editing;
                }
            }
        }
    }

    fn update_view(&self, widgets: &mut Self::Widgets, _sender: FactorySender<Self>) {
        self.update_active_tool(widgets);
        self.update_editing(widgets);
    }
}
