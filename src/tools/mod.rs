use std::fmt;
use std::str::FromStr;
use std::{
    borrow::Cow,
    cell::{OnceCell, RefCell},
    collections::HashMap,
    fmt::Debug,
    rc::Rc,
};

use anyhow::Result;
use femtovg::{Canvas, FontId, renderer::OpenGl};
use relm4::gtk::gdk_pixbuf::{
    glib::{Variant, VariantTy},
    prelude::{StaticVariantType, ToVariant},
};

use relm4::gtk::glib::variant::FromVariant;
use relm4::{
    Sender,
    gtk::{self, IMMulticontext},
};
use serde_derive::Deserialize;

use crate::{
    math::{Vec2D, ensure_bounding_box},
    sketch_board::{InputEvent, KeyEventMsg, MouseEventMsg, SketchBoardInput, TextEventMsg},
    style::Style,
};

use satty_cli::command_line;

mod arrow;
mod blur;
mod brush;
mod crop;
mod drag_box;
mod ellipse;
mod highlight;
mod line;
mod marker;
mod pointer;
mod rectangle;
mod text;

pub const HIT_BORDER_TOLERANCE: f32 = 7.0;

pub enum ToolEvent {
    Input(InputEvent),
    StyleChanged(Style),
}

#[derive(Debug, PartialEq, Eq)]
// The rendering mode of a drawable. This is used to determine drawables
// which should be taken out of the stack order to draw them earlier or later.
pub enum RenderingMode {
    Default,          // Render in stack order
    Blur,             // Rendered below everything else, but above the background
    Crop,             // Rendered above everything else, but below the SelectionOverlay
    SelectionOverlay, // Render above everything else
}

pub trait Tool {
    fn handle_event(&mut self, event: ToolEvent) -> ToolUpdateResult {
        match event {
            ToolEvent::Input(e) => self.handle_input_event(e),
            ToolEvent::StyleChanged(s) => self.handle_style_event(s),
        }
    }

    fn handle_activated(&mut self) -> ToolUpdateResult {
        ToolUpdateResult::Unmodified
    }

    fn handle_deactivated(&mut self) -> ToolUpdateResult {
        ToolUpdateResult::Unmodified
    }

    fn handle_input_event(&mut self, event: InputEvent) -> ToolUpdateResult {
        match event {
            InputEvent::Mouse(e) => self.handle_mouse_event(e),
            InputEvent::Key(e) => self.handle_key_event(e),
            InputEvent::KeyRelease(e) => self.handle_key_release_event(e),
            InputEvent::Text(e) => self.handle_text_event(e),
        }
    }

    fn handle_text_event(&mut self, event: TextEventMsg) -> ToolUpdateResult {
        let _ = event;
        ToolUpdateResult::Unmodified
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        let _ = event;
        ToolUpdateResult::Unmodified
    }

    fn handle_key_event(&mut self, event: KeyEventMsg) -> ToolUpdateResult {
        let _ = event;
        ToolUpdateResult::Unmodified
    }

    fn handle_key_release_event(&mut self, event: KeyEventMsg) -> ToolUpdateResult {
        let _ = event;
        ToolUpdateResult::Unmodified
    }

    fn handle_style_event(&mut self, style: Style) -> ToolUpdateResult {
        let _ = style;
        ToolUpdateResult::Unmodified
    }

    fn active(&self) -> bool {
        false
    }

    fn input_enabled(&self) -> bool;

    fn set_input_enabled(&mut self, value: bool);

    fn handle_undo(&mut self) -> ToolUpdateResult {
        ToolUpdateResult::Unmodified
    }

    fn handle_redo(&mut self) -> ToolUpdateResult {
        ToolUpdateResult::Unmodified
    }

    fn handle_reset(&mut self) {
        // override if your tool needs to reset a internal state, e.g. the next marker number for the marker tool
    }

    fn set_im_context(&mut self, _context: Option<InputContext>) {}

    fn get_drawable(&self) -> Option<&dyn Drawable>;

    fn get_tool_type(&self) -> Tools;

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>);
}

#[derive(Clone, Debug)]
pub struct GroupableTool {
    pub tool: Tools,
    pub icon_name: String,
    pub tooltip: Option<String>, // should be fine as long as there is no runtime shortcut editing
}

#[derive(Clone, Debug)]
pub struct InputContext {
    pub im_context: IMMulticontext,
    pub widget: gtk::Widget,
}

// the clone method below has been adapted from: https://stackoverflow.com/questions/30353462/how-to-clone-a-struct-storing-a-boxed-trait-object
// it feels "strange" and especially the fact that drawable has to derive from DrawableClone feels "wrong".
pub trait DrawableClone {
    fn clone_box(&self) -> Box<dyn Drawable>;
}

impl<T> DrawableClone for T
where
    T: 'static + Drawable + Clone,
{
    fn clone_box(&self) -> Box<dyn Drawable> {
        Box::new(self.clone())
    }
}

pub trait Drawable: DrawableClone + Debug {
    fn draw(&self, canvas: &mut Canvas<OpenGl>, font: FontId, bounds: (Vec2D, Vec2D))
    -> Result<()>;
    fn handle_undo(&mut self) {}
    fn handle_redo(&mut self) {}
    fn get_rendering_mode(&self) -> RenderingMode {
        RenderingMode::Default
    }
    fn bounds_only_valid_after_redraw(&self) -> bool {
        false
    }
    fn bounds(&self) -> Option<(Vec2D, Vec2D)> {
        None
    }
    fn hit_test(&self, pos: Vec2D, tolerance: f32) -> bool {
        let _ = (pos, tolerance);
        false
    }
    fn translate(&mut self, delta: Vec2D) {
        let _ = delta;
    }
    fn resize_bounds(&mut self, tl: Vec2D, br: Vec2D) {
        let _ = (tl, br);
    }
    // Returns position, text content and style if this drawable is an editable text, for
    // re-opening it in the text tool. Returns None for all other drawable types.
    fn edit_info(&self) -> Option<(Vec2D, String, crate::style::Style)> {
        None
    }

    fn get_style(&self) -> Option<&Style> {
        None
    }

    fn get_style_mut(&mut self) -> Option<&mut Style> {
        None
    }
}

pub fn hit_test_rectangle(
    pos: Vec2D,
    top_left: Vec2D,
    size: Option<Vec2D>,
    tolerance: f32,
    filled: bool,
) -> bool {
    let Some(size) = size else {
        return false;
    };

    // ensure a valid bounding box - dragging br to the left/up of tl is possible
    // and then the hit test should still work as expected
    let (tl, br) = ensure_bounding_box(top_left, top_left + size);

    if pos.x < tl.x - tolerance
        || pos.x > br.x + tolerance
        || pos.y < tl.y - tolerance
        || pos.y > br.y + tolerance
    {
        return false;
    }

    // Allow hit also inside
    if filled {
        return true;
    }

    let tl_inner = tl + tolerance;
    let br_inner = br - tolerance;

    pos.x < tl_inner.x || pos.x > br_inner.x || pos.y < tl_inner.y || pos.y > br_inner.y
}

#[derive(Debug)]
pub enum ToolUpdateResult {
    Commit(Box<dyn Drawable>),
    ReplaceDrawable(usize, Box<dyn Drawable>),
    Redraw,
    Unmodified,
    StopPropagation,
    RedrawAndStopPropagation,
}

pub use arrow::ArrowTool;
pub use blur::BlurTool;
pub use crop::CropTool;
pub use ellipse::EllipseTool;
pub use highlight::{HighlightTool, Highlighters};
pub use line::LineTool;
pub use pointer::PointerTool;
pub use rectangle::RectangleTool;
pub use text::TextTool;

use self::{brush::BrushTool, marker::MarkerTool};

thread_local! {
    static CROP_TOOL_SINGLETON: OnceCell<Rc<RefCell<CropTool>>> = const { OnceCell::new() };
}

fn shared_crop_tool() -> Rc<RefCell<CropTool>> {
    CROP_TOOL_SINGLETON.with(|cell| {
        cell.get_or_init(|| Rc::new(RefCell::new(CropTool::default())))
            .clone()
    })
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tools {
    Pointer = 0,
    Crop = 1,
    Line = 2,
    Arrow = 3,
    Rectangle = 4,
    Ellipse = 5,
    Text = 6,
    Marker = 7,
    Blur = 8,
    Highlight = 9,
    Brush = 10,
}

impl fmt::Display for Tools {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Tools::Pointer => "Pointer",
            Tools::Crop => "Crop",
            Tools::Brush => "Brush",
            Tools::Line => "Line",
            Tools::Arrow => "Arrow",
            Tools::Rectangle => "Rectangle",
            Tools::Ellipse => "Ellipse",
            Tools::Text => "Text",
            Tools::Marker => "Marker",
            Tools::Blur => "Blur",
            Tools::Highlight => "Highlight",
        };
        write!(f, "{}", name)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ParseCommandError;

impl FromStr for Tools {
    type Err = ParseCommandError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower_name = s.to_lowercase();
        match lower_name.as_str() {
            "pointer" => Ok(Self::Pointer),
            "crop" => Ok(Self::Crop),
            "line" => Ok(Self::Line),
            "arrow" => Ok(Self::Arrow),
            "rectangle" => Ok(Self::Rectangle),
            "ellipse" => Ok(Self::Ellipse),
            "text" => Ok(Self::Text),
            "marker" => Ok(Self::Marker),
            "blur" => Ok(Self::Blur),
            "highlight" => Ok(Self::Highlight),
            "brush" => Ok(Self::Brush),
            _ => Err(ParseCommandError),
        }
    }
}

pub struct ToolsManager {
    tools: HashMap<Tools, Rc<RefCell<dyn Tool>>>,
    crop_tool: Rc<RefCell<CropTool>>,
    pointer_tool: Rc<RefCell<PointerTool>>,
    text_tool: Rc<RefCell<TextTool>>,
}

impl ToolsManager {
    pub fn new() -> Self {
        let mut tools: HashMap<Tools, Rc<RefCell<dyn Tool>>> = HashMap::new();
        //tools.insert(Tools::Crop, Rc::new(RefCell::new(CropTool::default())));
        tools.insert(Tools::Line, Rc::new(RefCell::new(LineTool::default())));
        tools.insert(Tools::Arrow, Rc::new(RefCell::new(ArrowTool::default())));
        tools.insert(
            Tools::Rectangle,
            Rc::new(RefCell::new(RectangleTool::default())),
        );
        tools.insert(
            Tools::Ellipse,
            Rc::new(RefCell::new(EllipseTool::default())),
        );
        let text_tool = Rc::new(RefCell::new(TextTool::default()));
        tools.insert(Tools::Text, text_tool.clone());
        tools.insert(Tools::Blur, Rc::new(RefCell::new(BlurTool::default())));
        tools.insert(
            Tools::Highlight,
            Rc::new(RefCell::new(HighlightTool::default())),
        );
        tools.insert(Tools::Marker, Rc::new(RefCell::new(MarkerTool::default())));
        tools.insert(Tools::Brush, Rc::new(RefCell::new(BrushTool::default())));

        let crop_tool = shared_crop_tool();
        let text_tool = Rc::new(RefCell::new(TextTool::default()));
        let pointer_tool = Rc::new(RefCell::new(PointerTool::default()));
        Self {
            tools,
            text_tool,
            crop_tool,
            pointer_tool,
        }
    }

    pub fn get(&self, tool: &Tools) -> Rc<RefCell<dyn Tool>> {
        match tool {
            Tools::Crop => self.crop_tool.clone(),
            Tools::Text => self.text_tool.clone(),
            Tools::Pointer => self.pointer_tool.clone(),
            _ => self
                .tools
                .get(tool)
                .unwrap_or_else(|| {
                    panic!("Did you add the requested to {tool:#?} to the tools HashMap?")
                })
                .clone(),
        }
    }

    pub fn get_pointer_tool(&self) -> Rc<RefCell<PointerTool>> {
        self.pointer_tool.clone()
    }

    pub fn get_text_tool(&self) -> Rc<RefCell<TextTool>> {
        self.text_tool.clone()
    }
}

impl StaticVariantType for Tools {
    fn static_variant_type() -> Cow<'static, VariantTy> {
        Cow::Borrowed(VariantTy::UINT32)
    }
}

impl ToVariant for Tools {
    fn to_variant(&self) -> Variant {
        Variant::from(*self as u32)
    }
}

impl FromVariant for Tools {
    fn from_variant(variant: &Variant) -> Option<Self> {
        variant.get::<u32>().and_then(|v| match v {
            0 => Some(Tools::Pointer),
            1 => Some(Tools::Crop),
            2 => Some(Tools::Line),
            3 => Some(Tools::Arrow),
            4 => Some(Tools::Rectangle),
            5 => Some(Tools::Ellipse),
            6 => Some(Tools::Text),
            7 => Some(Tools::Marker),
            8 => Some(Tools::Blur),
            9 => Some(Tools::Highlight),
            10 => Some(Tools::Brush),
            _ => None,
        })
    }
}

impl From<command_line::Tools> for Tools {
    fn from(tool: command_line::Tools) -> Self {
        match tool {
            command_line::Tools::Pointer => Self::Pointer,
            command_line::Tools::Crop => Self::Crop,
            command_line::Tools::Line => Self::Line,
            command_line::Tools::Arrow => Self::Arrow,
            command_line::Tools::Rectangle => Self::Rectangle,
            command_line::Tools::Ellipse => Self::Ellipse,
            command_line::Tools::Text => Self::Text,
            command_line::Tools::Marker => Self::Marker,
            command_line::Tools::Blur => Self::Blur,
            command_line::Tools::Highlight => Self::Highlight,
            command_line::Tools::Brush => Self::Brush,
        }
    }
}
