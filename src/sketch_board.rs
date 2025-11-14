use anyhow::anyhow;

use femtovg::imgref::Img;
use femtovg::rgb::{ComponentBytes, RGBA};
use gdk_pixbuf::glib::Bytes;
use gdk_pixbuf::Pixbuf;
use keycode::{KeyMap, KeyMappingId};
use std::cell::RefCell;
use std::io::Write;
use std::panic;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::{fs, io};

use gtk::prelude::*;

use relm4::gtk::gdk::{DisplayManager, Key, ModifierType, Texture};
use relm4::{gtk, Component, ComponentParts, ComponentSender, RelmWidgetExt};

use crate::configuration::{Action, APP_CONFIG};
use crate::femtovg_area::FemtoVGArea;
use crate::ime::pango_adapter::spans_from_pango_attrs;
use crate::math::Vec2D;
use crate::notification::log_result;
use crate::style::Style;
use crate::tools::{Tool, ToolEvent, ToolUpdateResult, Tools, ToolsManager};
use crate::ui::toolbars::ToolbarEvent;

type RenderedImage = Img<Vec<RGBA<u8>>>;

#[derive(Debug, Clone)]
pub enum SketchBoardInput {
    InputEvent(InputEvent),
    ToolbarEvent(ToolbarEvent),
    RenderResult(RenderedImage, Vec<Action>),
    CommitEvent(TextEventMsg),
}

#[derive(Debug, Clone)]
pub enum SketchBoardOutput {
    ToggleToolbarsDisplay,
    ToolSwitchShortcut(Tools),
}

#[derive(Debug, Clone)]
pub enum InputEvent {
    Mouse(MouseEventMsg),
    Key(KeyEventMsg),
    KeyRelease(KeyEventMsg),
    Text(TextEventMsg),
}

// from https://flatuicolors.com/palette/au

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
pub enum MouseButton {
    Primary,
    Secondary,
    Middle,
}

#[derive(Debug, Clone, Copy)]
pub struct KeyEventMsg {
    pub key: Key,
    pub code: u32,
    pub modifier: ModifierType,
}
#[derive(Debug, Clone)]
pub enum TextEventMsg {
    Commit(String),
    Preedit {
        text: String,
        cursor_chars: Option<usize>,
        spans: Vec<crate::ime::preedit::PreeditSpan>,
    },
    PreeditEnd,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MouseEventType {
    BeginDrag,
    EndDrag,
    UpdateDrag,
    Click,
    Scroll,
    PointerPos,
    //Motion(Vec2D),
}

#[derive(Debug, Clone, Copy)]
pub struct MouseEventMsg {
    pub type_: MouseEventType,
    pub button: MouseButton,
    pub modifier: ModifierType,
    pub pos: Vec2D,
}

impl SketchBoardInput {
    pub fn new_mouse_event(
        event_type: MouseEventType,
        button: u32,
        modifier: ModifierType,
        pos: Vec2D,
    ) -> SketchBoardInput {
        SketchBoardInput::InputEvent(InputEvent::Mouse(MouseEventMsg {
            type_: event_type,
            button: button.into(),
            modifier,
            pos,
        }))
    }
    pub fn new_key_event(event: KeyEventMsg) -> SketchBoardInput {
        SketchBoardInput::InputEvent(InputEvent::Key(event))
    }

    pub fn new_key_release_event(event: KeyEventMsg) -> SketchBoardInput {
        SketchBoardInput::InputEvent(InputEvent::KeyRelease(event))
    }

    pub fn new_text_event(event: TextEventMsg) -> SketchBoardInput {
        SketchBoardInput::InputEvent(InputEvent::Text(event))
    }

    pub fn new_commit_event(event: TextEventMsg) -> SketchBoardInput {
        SketchBoardInput::CommitEvent(event)
    }

    pub fn new_scroll_event(delta_y: f64) -> SketchBoardInput {
        SketchBoardInput::InputEvent(InputEvent::Mouse(MouseEventMsg {
            type_: MouseEventType::Scroll,
            button: MouseButton::Middle,
            modifier: ModifierType::empty(),
            pos: Vec2D::new(0.0, delta_y as f32),
        }))
    }
}

impl From<u32> for MouseButton {
    fn from(value: u32) -> Self {
        match value {
            gtk::gdk::BUTTON_PRIMARY => MouseButton::Primary,
            gtk::gdk::BUTTON_MIDDLE => MouseButton::Middle,
            gtk::gdk::BUTTON_SECONDARY => MouseButton::Secondary,
            _ => MouseButton::Primary,
        }
    }
}

impl InputEvent {
    fn handle_event_mouse_input(&mut self, renderer: &FemtoVGArea) -> Option<ToolUpdateResult> {
        if let InputEvent::Mouse(me) = self {
            match me.type_ {
                MouseEventType::Click => {
                    if me.button == MouseButton::Secondary {
                        renderer.request_render(&APP_CONFIG.read().actions_on_right_click());
                        None
                    } else {
                        me.pos = renderer.abs_canvas_to_image_coordinates(me.pos);
                        None
                    }
                }
                MouseEventType::BeginDrag => {
                    me.pos = renderer.abs_canvas_to_image_coordinates(me.pos);
                    None
                }
                MouseEventType::EndDrag | MouseEventType::UpdateDrag => {
                    if me.button == MouseButton::Middle {
                        renderer.set_drag_offset(me.pos);
                        renderer.set_is_drag(true);

                        if me.type_ == MouseEventType::EndDrag {
                            renderer.store_last_offset();
                            renderer.set_is_drag(false);
                        }
                        renderer.request_render(&APP_CONFIG.read().actions_on_right_click());
                    } else {
                        me.pos = renderer.rel_canvas_to_image_coordinates(me.pos);
                    }
                    None
                }
                MouseEventType::Scroll => {
                    let factor = APP_CONFIG.read().zoom_factor();
                    match me.pos.y {
                        v if v < 0.0 => renderer.set_zoom_scale(factor),
                        v if v > 0.0 => renderer.set_zoom_scale(1f32 / factor),
                        _ => {}
                    }
                    renderer.request_render(&APP_CONFIG.read().actions_on_right_click());
                    None
                }
                MouseEventType::PointerPos => {
                    renderer.set_pointer_offset(me.pos);
                    None
                }
            }
        } else {
            None
        }
    }
}

pub struct SketchBoard {
    renderer: FemtoVGArea,
    active_tool: Rc<RefCell<dyn Tool>>,
    tools: ToolsManager,
    style: Style,
    im_context: gtk::IMMulticontext,
}

impl SketchBoard {
    fn refresh_screen(&mut self) {
        self.renderer.queue_render();
    }

    fn image_to_pixbuf(image: RenderedImage) -> Pixbuf {
        let (buf, w, h) = image.into_contiguous_buf();

        Pixbuf::from_bytes(
            &Bytes::from(buf.as_bytes()),
            gdk_pixbuf::Colorspace::Rgb,
            true,
            8,
            w as i32,
            h as i32,
            w as i32 * 4,
        )
    }

    fn deactivate_active_tool(&mut self) -> bool {
        if self.active_tool.borrow().active() {
            if let ToolUpdateResult::Commit(result) =
                self.active_tool.borrow_mut().handle_deactivated()
            {
                self.renderer.commit(result);
                return true;
            }
        }
        false
    }

    fn handle_action(&mut self, actions: &[Action]) -> ToolUpdateResult {
        let rv = if self.deactivate_active_tool() {
            ToolUpdateResult::Redraw
        } else {
            ToolUpdateResult::Unmodified
        };
        self.renderer.request_render(actions);
        rv
    }

    fn handle_render_result(&self, image: RenderedImage, actions: Vec<Action>) {
        let needs_pixbuf = actions.iter().any(|action| {
            matches!(
                action,
                Action::SaveToClipboard | Action::SaveToFile | Action::SaveToFileAs
            )
        });

        let pix_buf = if needs_pixbuf {
            Some(Self::image_to_pixbuf(image))
        } else {
            None
        };

        for action in actions {
            match action {
                Action::SaveToClipboard => {
                    if let Some(ref pix_buf) = pix_buf {
                        self.handle_copy_clipboard(pix_buf);
                    }
                }
                Action::SaveToFile => {
                    if let Some(ref pix_buf) = pix_buf {
                        self.handle_save(pix_buf);
                    }
                }
                Action::SaveToFileAs => {
                    if let Some(ref pix_buf) = pix_buf {
                        self.handle_save_as(pix_buf);
                    }
                }
                _ => (),
            }

            if APP_CONFIG.read().early_exit() || action == Action::Exit {
                self.handle_exit();
                return;
            }
        }
    }

    fn handle_exit(&self) {
        relm4::main_application().quit();
    }

    fn handle_save(&self, image: &Pixbuf) {
        let mut output_filename = match APP_CONFIG.read().output_filename() {
            None => {
                println!("No Output filename specified!");
                return;
            }
            Some(o) => o.clone(),
        };

        // run the output filename by "chrono date format"
        let delayed_format = chrono::Local::now().format(&output_filename);
        let result = panic::catch_unwind(|| {
            delayed_format.to_string();
        });

        if result.is_err() {
            println!(
                "Warning: Could not format filename {output_filename} due to chrono format error, falling back to literal filename."
            );
        } else {
            output_filename = format!("{delayed_format}");
        }

        // TODO: we could support more data types
        if output_filename != "-" && !output_filename.ends_with(".png") {
            log_result(
                "The only supported format is png, but the filename does not end in png",
                !APP_CONFIG.read().disable_notifications(),
            );
            return;
        }

        if let Some(tilde_stripped) =
            output_filename.strip_prefix(&format!("~{}", std::path::MAIN_SEPARATOR_STR))
        {
            if let Some(h) = std::env::home_dir() {
                let mut p = h;
                p.push(tilde_stripped);
                output_filename = p.to_string_lossy().into_owned();
            } else {
                log_result(
                    "~ found but could not determine homedir",
                    !APP_CONFIG.read().disable_notifications(),
                );
                return;
            }
        }

        let data = match image.save_to_bufferv("png", &Vec::new()) {
            Ok(d) => d,
            Err(e) => {
                println!("Error serializing image: {e}");
                return;
            }
        };

        if output_filename == "-" {
            // "-" means stdout
            let stdout = io::stdout();
            let mut handle = stdout.lock();
            if let Err(e) = handle.write_all(&data) {
                eprintln!("Error writing image to stdout: {e}");
            }
            return;
        }
        match fs::write(&output_filename, data) {
            Err(e) => log_result(
                &format!("Error while saving file: {e}"),
                !APP_CONFIG.read().disable_notifications(),
            ),
            Ok(_) => log_result(
                &format!("File saved to '{}'.", &output_filename),
                !APP_CONFIG.read().disable_notifications(),
            ),
        };
    }

    fn handle_save_as(&self, image: &Pixbuf) {
        let data = match image.save_to_bufferv("png", &Vec::new()) {
            Ok(d) => d,
            Err(e) => {
                println!("Error serializing image: {e}");
                return;
            }
        };

        let root = self.renderer.toplevel_window();

        relm4::spawn_local(async move {
            let builder = gtk::FileChooserDialog::builder()
                .modal(false)
                .title("Save Image As")
                .action(gtk::FileChooserAction::Save);

            let dialog = match root {
                Some(w) => builder.transient_for(&w),
                None => builder,
            }
            .build();

            dialog.add_buttons(&[
                ("Cancel", gtk::ResponseType::Cancel),
                ("Save", gtk::ResponseType::Accept),
            ]);

            dialog.connect_response(move |dialog, response| {
                if response == gtk::ResponseType::Accept {
                    if let Some(file) = dialog.file() {
                        let output_filename = match file.path() {
                            Some(path) => path.to_string_lossy().into_owned(),
                            None => return,
                        };

                        match fs::write(&output_filename, &data) {
                            Err(e) => log_result(
                                &format!("Error while saving file: {e}"),
                                !APP_CONFIG.read().disable_notifications(),
                            ),
                            Ok(_) => log_result(
                                &format!("File saved to '{}'.", &output_filename),
                                !APP_CONFIG.read().disable_notifications(),
                            ),
                        };
                    }
                }
                dialog.close();
            });

            dialog.show();
        });
    }

    fn save_to_clipboard(&self, texture: &impl IsA<Texture>) -> anyhow::Result<()> {
        let display = DisplayManager::get()
            .default_display()
            .ok_or(anyhow!("Cannot open default display for clipboard."))?;
        display.clipboard().set_texture(texture);

        Ok(())
    }

    fn save_to_external_process(
        &self,
        texture: &impl IsA<Texture>,
        command: &str,
    ) -> anyhow::Result<()> {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()?;

        let child_stdin = child.stdin.as_mut().unwrap();
        child_stdin.write_all(texture.save_to_png_bytes().as_ref())?;

        if !child.wait()?.success() {
            return Err(anyhow!("Writing to process '{command}' failed."));
        }

        Ok(())
    }

    fn handle_copy_clipboard(&self, image: &Pixbuf) {
        let texture = Texture::for_pixbuf(image);

        let result = if let Some(command) = APP_CONFIG.read().copy_command() {
            self.save_to_external_process(&texture, command)
        } else {
            self.save_to_clipboard(&texture)
        };

        match result {
            Err(e) => println!("Error saving {e}"),
            Ok(()) => {
                log_result(
                    "Copied to clipboard.",
                    !APP_CONFIG.read().disable_notifications(),
                );

                // TODO: rethink order and messaging patterns
                if APP_CONFIG.read().save_after_copy() {
                    self.handle_save(image);
                };
            }
        }
    }

    fn handle_undo(&mut self) -> ToolUpdateResult {
        if self.active_tool.borrow().active() {
            self.active_tool.borrow_mut().handle_undo()
        } else if self.renderer.undo() {
            ToolUpdateResult::Redraw
        } else {
            ToolUpdateResult::Unmodified
        }
    }

    fn handle_redo(&mut self) -> ToolUpdateResult {
        if self.active_tool.borrow().active() {
            self.active_tool.borrow_mut().handle_redo()
        } else if self.renderer.redo() {
            ToolUpdateResult::Redraw
        } else {
            ToolUpdateResult::Unmodified
        }
    }

    fn handle_reset(&mut self) -> ToolUpdateResult {
        // can't use lazy || here
        if self.deactivate_active_tool() | self.renderer.reset() {
            ToolUpdateResult::Redraw
        } else {
            ToolUpdateResult::Unmodified
        }
    }

    fn handle_resize(&mut self) -> ToolUpdateResult {
        self.renderer.reset_size(0.);
        self.renderer
            .request_render(&APP_CONFIG.read().actions_on_right_click());
        ToolUpdateResult::Unmodified
    }

    fn handle_original_scale(&mut self) -> ToolUpdateResult {
        self.renderer.reset_size(1.);
        self.renderer
            .request_render(&APP_CONFIG.read().actions_on_right_click());
        ToolUpdateResult::Unmodified
    }

    // Toolbars = Tools Toolbar + Style Toolbar
    fn handle_toggle_toolbars_display(
        &mut self,
        sender: ComponentSender<Self>,
    ) -> ToolUpdateResult {
        sender
            .output_sender()
            .emit(SketchBoardOutput::ToggleToolbarsDisplay);
        ToolUpdateResult::Unmodified
    }

    fn handle_toolbar_event(&mut self, toolbar_event: ToolbarEvent) -> ToolUpdateResult {
        match toolbar_event {
            ToolbarEvent::ToolSelected(tool) => {
                // deactivate old tool and save drawable, if any
                let old_tool = self.active_tool.clone();
                let mut deactivate_result =
                    old_tool.borrow_mut().handle_event(ToolEvent::Deactivated);

                old_tool.borrow_mut().set_im_context(None);

                if let ToolUpdateResult::Commit(d) = deactivate_result {
                    self.renderer.commit(d);
                    // we handle commit directly and "downgrade" to a simple redraw result
                    deactivate_result = ToolUpdateResult::Redraw;
                }

                // change active tool
                self.active_tool = self.tools.get(&tool);
                self.renderer.set_active_tool(self.active_tool.clone());
                let widget_ref: gtk::Widget = self.renderer.clone().upcast();
                self.active_tool
                    .borrow_mut()
                    .set_im_context(Some(crate::tools::InputContext {
                        im_context: self.im_context.clone(),
                        widget: widget_ref,
                    }));

                // send style event
                self.active_tool
                    .borrow_mut()
                    .handle_event(ToolEvent::StyleChanged(self.style));

                // send activated event
                let activate_result = self
                    .active_tool
                    .borrow_mut()
                    .handle_event(ToolEvent::Activated);

                match activate_result {
                    ToolUpdateResult::Unmodified => deactivate_result,
                    _ => activate_result,
                }
            }
            ToolbarEvent::ColorSelected(color) => {
                self.style.color = color;
                self.active_tool
                    .borrow_mut()
                    .handle_event(ToolEvent::StyleChanged(self.style))
            }
            ToolbarEvent::SizeSelected(size) => {
                self.style.size = size;
                self.active_tool
                    .borrow_mut()
                    .handle_event(ToolEvent::StyleChanged(self.style))
            }
            ToolbarEvent::SaveFile => self.handle_action(&[Action::SaveToFile]),
            ToolbarEvent::CopyClipboard => self.handle_action(&[Action::SaveToClipboard]),
            ToolbarEvent::Undo => self.handle_undo(),
            ToolbarEvent::Redo => self.handle_redo(),
            ToolbarEvent::Reset => self.handle_reset(),
            ToolbarEvent::ToggleFill => {
                self.style.fill = !self.style.fill;
                self.active_tool
                    .borrow_mut()
                    .handle_event(ToolEvent::StyleChanged(self.style))
            }
            ToolbarEvent::AnnotationSizeChanged(value) => {
                self.style.annotation_size_factor = value;
                self.active_tool
                    .borrow_mut()
                    .handle_event(ToolEvent::StyleChanged(self.style))
            }
            ToolbarEvent::SaveFileAs => self.handle_action(&[Action::SaveToFileAs]),
            ToolbarEvent::Resize => self.handle_resize(),
            ToolbarEvent::OriginalScale => self.handle_original_scale(),
        }
    }

    fn handle_text_commit(
        &self,
        event: TextEventMsg,
        sender: ComponentSender<Self>,
    ) -> ToolUpdateResult {
        match event {
            TextEventMsg::Commit(txt) => {
                // NOTE:
                // If there's an IMContext binded to the controller, single letter-key events will
                // always go through it first, denying a bypass, so the only way we can do single-key
                // bindings is to act upon the IMMulticontext's commit event itself.
                // NOTE:
                // Here we're basically bypassing the IMMulticontext. If the text tool is active
                // and wants text inputs, we're interested in the single-letter keypress as a text character.
                // If not, we parse it as a shortcut event.
                if self.active_tool_type() == Tools::Text
                    && self.active_tool.borrow().input_enabled()
                {
                    sender.input(SketchBoardInput::new_text_event(TextEventMsg::Commit(
                        txt.to_string(),
                    )));
                } else if let Some(tool) = txt
                    .chars()
                    .next()
                    .and_then(|char| APP_CONFIG.read().keybinds().get_tool(char))
                {
                    sender.input(SketchBoardInput::ToolbarEvent(ToolbarEvent::ToolSelected(
                        tool,
                    )));
                    sender
                        .output_sender()
                        .emit(SketchBoardOutput::ToolSwitchShortcut(tool));
                }
            }
            TextEventMsg::Preedit {
                text,
                cursor_chars,
                spans,
            } => {
                if self.active_tool_type() == Tools::Text
                    && self.active_tool.borrow().input_enabled()
                {
                    sender.input(SketchBoardInput::new_text_event(TextEventMsg::Preedit {
                        text,
                        cursor_chars,
                        spans,
                    }));
                }
            }
            TextEventMsg::PreeditEnd => {
                if self.active_tool_type() == Tools::Text
                    && self.active_tool.borrow().input_enabled()
                {
                    sender.input(SketchBoardInput::new_text_event(TextEventMsg::PreeditEnd));
                }
            }
        }
        ToolUpdateResult::Unmodified
    }

    pub fn active_tool_type(&self) -> Tools {
        self.active_tool.borrow().get_tool_type()
    }
}

#[relm4::component(pub)]
impl Component for SketchBoard {
    type CommandOutput = ();
    type Input = SketchBoardInput;
    type Output = SketchBoardOutput;
    type Init = Pixbuf;

    view! {
        gtk::Box {
            #[local_ref]
            area -> FemtoVGArea {
                set_vexpand: true,
                set_hexpand: true,
                set_can_focus: true,
                set_focusable: true,
                grab_focus: (),

                add_controller = gtk::GestureDrag {
                        set_button: 0,
                        connect_drag_begin[sender] => move |controller, x, y| {
                            sender.input(SketchBoardInput::new_mouse_event(
                                MouseEventType::BeginDrag,
                                controller.current_button(),
                                controller.current_event_state(),
                                Vec2D::new(x as f32, y as f32)));

                        },
                        connect_drag_update[sender] => move |controller, x, y| {
                            sender.input(SketchBoardInput::new_mouse_event(
                                MouseEventType::UpdateDrag,
                                controller.current_button(),
                                controller.current_event_state(),
                                Vec2D::new(x as f32, y as f32)));
                        },
                        connect_drag_end[sender] => move |controller, x, y| {
                            sender.input(SketchBoardInput::new_mouse_event(
                                MouseEventType::EndDrag,
                                controller.current_button(),
                                controller.current_event_state(),
                                Vec2D::new(x as f32, y as f32)
                            ));
                        }
                },

                add_controller = gtk::GestureClick {
                    set_button: 0,
                    connect_pressed[sender] => move |controller, _, x, y| {
                        sender.input(SketchBoardInput::new_mouse_event(
                            MouseEventType::Click,
                            controller.current_button(),
                            controller.current_event_state(),
                            Vec2D::new(x as f32, y as f32)));
                    }
                },

                add_controller = gtk::EventControllerScroll{
                    set_flags: gtk::EventControllerScrollFlags::VERTICAL,
                    connect_scroll[sender] => move |_, _, dy| {
                        sender.input(SketchBoardInput::new_scroll_event(dy));
                        glib::Propagation::Stop
                    },
                },

                add_controller = gtk::EventControllerKey {
                    connect_key_pressed[sender] => move |controller, key, code, modifier | {
                        if let Some(im_context) = controller.im_context() {
                            im_context.focus_in();
                            if !im_context.filter_keypress(controller.current_event().unwrap()) {
                                sender.input(SketchBoardInput::new_key_event(KeyEventMsg::new(key, code, modifier)));
                            }
                        } else {
                            sender.input(SketchBoardInput::new_key_event(KeyEventMsg::new(key, code, modifier)));
                        }
                        glib::Propagation::Stop
                    },

                    connect_key_released[sender] => move |controller, key, code, modifier | {
                        if let Some(im_context) = controller.im_context() {
                            im_context.focus_in();
                            if !im_context.filter_keypress(controller.current_event().unwrap()) {
                                sender.input(SketchBoardInput::new_key_release_event(KeyEventMsg::new(key, code, modifier)));
                            }
                        } else {
                            sender.input(SketchBoardInput::new_key_release_event(KeyEventMsg::new(key, code, modifier)));
                        }
                    },
                    set_im_context: Some(&model.im_context),
                },

                add_controller = gtk::EventControllerMotion {
                    connect_motion[sender] => move |controller, x, y| {
                        sender.input(SketchBoardInput::new_mouse_event(
                            MouseEventType::PointerPos,
                            0,
                            controller.current_event_state(),
                            Vec2D::new(x as f32, y as f32)
                        ));
                    }
                }
            }
        },
    }

    fn update(&mut self, msg: SketchBoardInput, sender: ComponentSender<Self>, _root: &Self::Root) {
        // handle resize ourselves, pass everything else to tool
        let result = match msg {
            SketchBoardInput::InputEvent(mut ie) => {
                if let InputEvent::Key(ke) = ie {
                    if ke.is_one_of(Key::z, KeyMappingId::UsZ)
                        && ke.modifier == ModifierType::CONTROL_MASK
                    {
                        self.handle_undo()
                    } else if ke.is_one_of(Key::y, KeyMappingId::UsY)
                        && ke.modifier == ModifierType::CONTROL_MASK
                    {
                        self.handle_redo()
                    } else if ke.is_one_of(Key::t, KeyMappingId::UsT)
                        && ke.modifier == ModifierType::CONTROL_MASK
                    {
                        self.handle_toggle_toolbars_display(sender)
                    } else if ke.is_one_of(Key::s, KeyMappingId::UsS)
                        && ke.modifier == ModifierType::CONTROL_MASK
                    {
                        self.renderer.request_render(&[Action::SaveToFile]);
                        ToolUpdateResult::Unmodified
                    } else if ke.is_one_of(Key::s, KeyMappingId::UsS)
                        && ke.modifier == (ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK)
                    {
                        self.renderer.request_render(&[Action::SaveToFileAs]);
                        ToolUpdateResult::Unmodified
                    } else if ke.is_one_of(Key::c, KeyMappingId::UsC)
                        && ke.modifier == ModifierType::CONTROL_MASK
                    {
                        self.renderer.request_render(&[Action::SaveToClipboard]);
                        ToolUpdateResult::Unmodified
                    } else if (ke.is_one_of(Key::leftarrow, KeyMappingId::ArrowLeft)
                        || ke.is_one_of(Key::rightarrow, KeyMappingId::ArrowRight)
                        || ke.is_one_of(Key::uparrow, KeyMappingId::ArrowUp)
                        || ke.is_one_of(Key::downarrow, KeyMappingId::ArrowDown))
                        && ke.modifier == ModifierType::CONTROL_MASK
                    {
                        let pan_step_size = APP_CONFIG.read().pan_step_size();
                        match ke.key {
                            Key::Left => self
                                .renderer
                                .set_drag_offset(Vec2D::new(-pan_step_size, 0.)),
                            Key::Right => {
                                self.renderer.set_drag_offset(Vec2D::new(pan_step_size, 0.))
                            }
                            Key::Up => self
                                .renderer
                                .set_drag_offset(Vec2D::new(0., -pan_step_size)),
                            Key::Down => {
                                self.renderer.set_drag_offset(Vec2D::new(0., pan_step_size))
                            }
                            _ => { /* unreachable */ }
                        }

                        self.renderer.store_last_offset();
                        self.renderer
                            .request_render(&APP_CONFIG.read().actions_on_right_click());
                        ToolUpdateResult::Unmodified
                    } else if ke.modifier.is_empty()
                        && (ke.key == Key::Escape
                            || ke.key == Key::Return
                            || ke.key == Key::KP_Enter)
                    {
                        // First, let the tool handle the event. If the tool does nothing, we can do our thing (otherwise require a second keyboard press)
                        // Relying on ToolUpdateResult::Unmodified is probably not a good idea, but it's the only way at the moment. See discussion in #144
                        let result: ToolUpdateResult = self
                            .active_tool
                            .borrow_mut()
                            .handle_event(ToolEvent::Input(ie));
                        if let ToolUpdateResult::Unmodified = result {
                            let actions = if ke.key == Key::Escape {
                                APP_CONFIG.read().actions_on_escape()
                            } else {
                                APP_CONFIG.read().actions_on_enter()
                            };
                            self.renderer.request_render(&actions);
                        };
                        result
                    } else {
                        self.active_tool
                            .borrow_mut()
                            .handle_event(ToolEvent::Input(ie))
                    }
                } else {
                    ie.handle_event_mouse_input(&self.renderer);
                    self.active_tool
                        .borrow_mut()
                        .handle_event(ToolEvent::Input(ie))
                }
            }
            SketchBoardInput::ToolbarEvent(toolbar_event) => {
                self.handle_toolbar_event(toolbar_event)
            }
            SketchBoardInput::RenderResult(img, action) => {
                self.handle_render_result(img, action);
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::CommitEvent(txt) => {
                self.handle_text_commit(txt, sender);
                ToolUpdateResult::Unmodified
            }
        };

        //println!("Event={:?} Result={:?}", msg, result);
        match result {
            ToolUpdateResult::Commit(drawable) => {
                self.renderer.commit(drawable);
                self.refresh_screen();
            }
            ToolUpdateResult::Unmodified => (),
            ToolUpdateResult::Redraw => self.refresh_screen(),
        };
    }

    fn init(
        image: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let config = APP_CONFIG.read();
        let tools = ToolsManager::new();

        let im_context = gtk::IMMulticontext::new();

        let mut model = Self {
            renderer: FemtoVGArea::default(),
            active_tool: tools.get(&config.initial_tool()),
            style: Style::default(),
            tools,
            im_context,
        };

        let area = &mut model.renderer;
        area.init(
            sender.input_sender().clone(),
            model.tools.get_crop_tool(),
            model.active_tool.clone(),
            image,
        );

        let widgets = view_output!();

        model.im_context.set_client_widget(Some(&model.renderer));
        model.im_context.set_use_preedit(true);

        if let Ok(module) = std::env::var("GTK_IM_MODULE") {
            if module.eq_ignore_ascii_case("fcitx") || module.eq_ignore_ascii_case("fcitx5") {
                model.im_context.set_context_id(Some("fcitx"));
            }
        }

        {
            let sender = sender.input_sender().clone();
            model.im_context.connect_commit(move |_cx, txt| {
                sender.emit(SketchBoardInput::new_commit_event(TextEventMsg::Commit(
                    txt.to_string(),
                )));
            });
        }

        {
            let sender = sender.input_sender().clone();
            model.im_context.connect_preedit_changed(move |cx| {
                let (text, attrs, cursor) = cx.preedit_string();
                let cursor_chars = if cursor >= 0 {
                    Some(cursor as usize)
                } else {
                    None
                };
                let spans = spans_from_pango_attrs(text.as_str(), Some(attrs));
                sender.emit(SketchBoardInput::new_commit_event(TextEventMsg::Preedit {
                    text: text.to_string(),
                    cursor_chars,
                    spans,
                }));
            });
        }

        {
            let sender = sender.input_sender().clone();
            model.im_context.connect_preedit_end(move |_cx| {
                sender.emit(SketchBoardInput::new_commit_event(TextEventMsg::PreeditEnd));
            });
        }

        let focus_controller = gtk::EventControllerFocus::new();
        {
            let im_context = model.im_context.clone();
            focus_controller.connect_enter(move |_| {
                im_context.focus_in();
            });
        }
        {
            let im_context = model.im_context.clone();
            focus_controller.connect_leave(move |_| {
                im_context.focus_out();
            });
        }
        model.renderer.add_controller(focus_controller);

        let widget_ref: gtk::Widget = model.renderer.clone().upcast();
        model
            .active_tool
            .borrow_mut()
            .set_im_context(Some(crate::tools::InputContext {
                im_context: model.im_context.clone(),
                widget: widget_ref,
            }));

        ComponentParts { model, widgets }
    }
}

impl KeyEventMsg {
    pub fn new(key: Key, code: u32, modifier: ModifierType) -> Self {
        Self {
            key,
            code,
            modifier,
        }
    }

    /// Matches one of providen keys. The modifier is not considered.
    /// And the key has more priority over keycode.
    fn is_one_of(&self, key: Key, code: KeyMappingId) -> bool {
        // INFO: on linux the keycode from gtk4 is evdev keycode, so need to match by him if need
        // to use layout-independent shortcuts. And notice that there is substraction by 8, it's
        // because of x11 compatibility in which the keycodes are in range [8,255]. So need shift
        // them to get correct evdev keycode.
        let keymap = KeyMap::from(code);
        self.key == key || self.code as u16 - 8 == keymap.evdev
    }
}
