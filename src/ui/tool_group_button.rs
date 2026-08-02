use crate::tools::{GroupableTool, Tools};
use crate::ui::toolbars::ToolsAction;
use relm4::actions::ActionablePlus;
use relm4::factory::{DynamicIndex, FactoryComponent};
use relm4::gtk::prelude::{ButtonExt, WidgetExt};
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
            widgets.button.set_tooltip(&g.tooltip);
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
            is_active: is_active,
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
        _sender: FactorySender<Self>,
    ) -> Self::Widgets {
        view! {
            #[local_ref]
            root -> relm4::gtk::Overlay {
                #[wrap(Some)]
                set_child: button = &ToggleButton {
                    set_focusable: false,
                    set_valign: Align::End,
                    set_halign: Align::Center,
                }
            },
        }

        let widgets = ToolGroupWidgets { button };
        self.update_active_tool(&widgets);

        widgets
    }

    fn update(&mut self, message: Self::Input, _sender: FactorySender<Self>) {
        match message {
            ToolGroupButtonInput::OpenPopover => {}
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
        self.update_active_tool(&widgets);
        self.update_editing(&widgets);
    }
}
