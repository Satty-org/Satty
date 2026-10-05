use std::cell::Cell;
use std::rc::Rc;

use crate::tools::{GroupableTool, Tools};
use crate::ui::toolbars::ToolsAction;
use relm4::actions::ActionablePlus;
use relm4::factory::{DynamicIndex, FactoryComponent};
use relm4::gtk::prelude::{
    BoxExt, ButtonExt, EventControllerExt, GestureExt, PopoverExt, WidgetExt,
};
use relm4::gtk::{Align, Popover, ToggleButton};
use relm4::{FactorySender, RelmWidgetExt, view};

thread_local! {
    static ACTIVE_TOOL_POPOVER: std::cell::RefCell<Option<Popover>> =
        const { std::cell::RefCell::new(None) };
}

pub fn close_active_tool_popover_if_outside(target: Option<&relm4::gtk::Widget>) {
    ACTIVE_TOOL_POPOVER.with(|active| {
        let mut active = active.borrow_mut();
        if let Some(popover) = active.as_ref()
            && !target.is_some_and(|widget| widget.is_ancestor(popover))
        {
            popover.popdown();
            *active = None;
        }
    });
}

fn update_menu_indicator_hover(
    indicator: &relm4::gtk::Image,
    button: &ToggleButton,
    x: f64,
    y: f64,
) {
    let over_indicator =
        x >= f64::from(button.width()) - 12.0 && y >= f64::from(button.height()) - 12.0;
    if over_indicator {
        indicator.add_css_class("grouped-tools-menu-indicator-hover");
    } else {
        indicator.remove_css_class("grouped-tools-menu-indicator-hover");
    }
}

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
            if let Some(tt) = &g.tooltip {
                let tooltip = if self.has_extra() {
                    format!(
                        "{}\n\n{}",
                        tt, "click long or click the triangle or right-click for more tools"
                    )
                } else {
                    tt.clone()
                };
                widgets.button.set_tooltip(&tooltip);
            }
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
            popover: relm4::gtk::Popover::builder().autohide(false).build(),
        }
    }

    fn init_root(&self) -> Self::Root {
        relm4::gtk::Overlay::new()
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
                },
            },
        }

        if self.has_extra() {
            let menu_indicator = relm4::gtk::Image::builder()
                .icon_name("caret-down-right-filled")
                .pixel_size(8)
                .halign(Align::End)
                .valign(Align::End)
                .can_target(false)
                .build();
            root.add_overlay(&menu_indicator);

            let motion_controller = relm4::gtk::EventControllerMotion::new();
            let enter_indicator = menu_indicator.clone();
            let enter_button = button.clone();
            motion_controller.connect_enter(move |_, x, y| {
                update_menu_indicator_hover(&enter_indicator, &enter_button, x, y);
            });
            let motion_indicator = menu_indicator.clone();
            let motion_button = button.clone();
            motion_controller.connect_motion(move |_, x, y| {
                update_menu_indicator_hover(&motion_indicator, &motion_button, x, y);
            });
            let leave_indicator = menu_indicator.clone();
            motion_controller.connect_leave(move |_| {
                leave_indicator.remove_css_class("grouped-tools-menu-indicator-hover");
            });
            button.add_controller(motion_controller);

            let triangle_click_controller = relm4::gtk::GestureClick::builder().button(1).build();
            triangle_click_controller.set_propagation_phase(relm4::gtk::PropagationPhase::Capture);
            let triangle_pressed = Rc::new(Cell::new(false));
            let pressed_on_press = Rc::clone(&triangle_pressed);
            let button_for_hit_test = button.clone();
            triangle_click_controller.connect_pressed(move |gesture, _, x, y| {
                let over_indicator = x >= f64::from(button_for_hit_test.width()) - 12.0
                    && y >= f64::from(button_for_hit_test.height()) - 12.0;
                pressed_on_press.set(over_indicator);
                if over_indicator {
                    gesture.set_state(relm4::gtk::EventSequenceState::Claimed);
                }
            });
            let pressed_on_release = Rc::clone(&triangle_pressed);
            let triangle_click_sender = sender.clone();
            triangle_click_controller.connect_released(move |_, _, _, _| {
                if pressed_on_release.replace(false) {
                    triangle_click_sender.input(ToolGroupButtonInput::OpenPopover);
                }
            });
            button.add_controller(triangle_click_controller);

            let long_press = relm4::gtk::GestureLongPress::new();
            long_press.set_delay_factor(0.6);
            long_press.set_propagation_phase(relm4::gtk::PropagationPhase::Capture);
            let long_press_sender = sender.clone();
            long_press.connect_pressed(move |gesture, _, _| {
                gesture.set_state(relm4::gtk::EventSequenceState::Claimed);
                long_press_sender.input(ToolGroupButtonInput::OpenPopover);
            });
            button.add_controller(long_press);

            let right_click_controller = relm4::gtk::GestureClick::builder().button(3).build();
            right_click_controller.set_propagation_phase(relm4::gtk::PropagationPhase::Capture);
            right_click_controller.connect_pressed(move |gesture, _, _, _| {
                gesture.set_state(relm4::gtk::EventSequenceState::Claimed);
            });
            right_click_controller.connect_released(move |_, _, _, _| {
                sender.input(ToolGroupButtonInput::OpenPopover);
            });
            root.add_controller(right_click_controller);

            let rows = relm4::gtk::Box::new(relm4::gtk::Orientation::Vertical, 2);
            for tool in &self.group {
                let button = ToggleButton::builder().focusable(false).build();
                let inner_box = relm4::gtk::Box::new(relm4::gtk::Orientation::Horizontal, 2);
                let icon = relm4::gtk::Image::from_icon_name(&tool.icon_name);
                let label = relm4::gtk::Label::new(Some(&format!("{}", tool.tool)));
                inner_box.append(&icon);
                inner_box.append(&label);
                button.set_child(Some(&inner_box));
                if let Some(tooltip) = &tool.tooltip {
                    button.set_tooltip(tooltip);
                }
                let popover = self.popover.clone();
                button.connect_clicked(move |_| popover.popdown());
                ActionablePlus::set_action::<ToolsAction>(&button, tool.tool);
                rows.append(&button);
            }
            self.popover.set_child(Some(&rows));
            self.popover.set_position(relm4::gtk::PositionType::Bottom);
            self.popover.set_parent(&root);
        }

        let widgets = ToolGroupWidgets { button };
        self.update_active_tool(&widgets);

        widgets
    }

    fn update(&mut self, message: Self::Input, _sender: FactorySender<Self>) {
        match message {
            ToolGroupButtonInput::OpenPopover => {
                if self.has_extra() {
                    ACTIVE_TOOL_POPOVER.with(|active| {
                        let mut active = active.borrow_mut();
                        if let Some(popover) = active.as_ref() {
                            popover.popdown();
                        }
                        self.popover.popup();
                        *active = Some(self.popover.clone());
                    });
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
        if self.has_extra() {
            self.update_active_tool(widgets);
        }
        self.update_editing(widgets);
    }
}
