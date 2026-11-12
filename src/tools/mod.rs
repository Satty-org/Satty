use std::any::Any;
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
use femtovg::imgref::ImgVec;
use femtovg::rgb::RGBA8;
use femtovg::{Canvas, FontId, RenderTarget, renderer::OpenGl};
use relm4::gtk::gdk_pixbuf::{
    Pixbuf,
    glib::{Variant, VariantTy},
    prelude::{StaticVariantType, ToVariant},
};

use relm4::gtk::gdk::ModifierType;
use relm4::gtk::glib::variant::FromVariant;
use relm4::{
    Sender,
    gtk::{self, IMMulticontext},
};

use crate::math::rect_ensure_positive_size;
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
mod image;
mod line;
mod marker;
mod pixelate;
mod pointer;
mod rectangle;
pub mod spotlight;
mod text;

pub const HIT_BORDER_TOLERANCE: f32 = 7.0;

pub enum ToolEvent {
    Input(InputEvent),
    StyleChanged(Style),
    // the background size, and where the image goes; without a placement it
    // is centered on the screenshot
    ImageSelected(Pixbuf, Vec2D, Option<ImagePlacement>),
}

#[derive(Debug, PartialEq, Eq)]
// The rendering mode of a drawable. This is used to determine drawables
// which should be taken out of the stack order to draw them earlier or later.
pub enum RenderingMode {
    // Rendered below everything else, but above the background
    BlurOrPixelate,
    SpotlightBlur,
    SpotlightHighlight,
    // Render in stack order
    Default,
    // Rendered above everything else, but below the SelectionOverlay
    Crop,
    // Render above everything else
    SelectionOverlay,
}

pub trait Tool {
    fn handle_event(&mut self, event: ToolEvent) -> ToolUpdateResult {
        match event {
            ToolEvent::Input(e) => self.handle_input_event(e),
            ToolEvent::StyleChanged(s) => self.handle_style_event(s),
            ToolEvent::ImageSelected(i, b, p) => self.handle_image_selected(i, b, p),
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

    fn handle_image_selected(
        &mut self,
        pixbuf: Pixbuf,
        background_size: Vec2D,
        placement: Option<ImagePlacement>,
    ) -> ToolUpdateResult {
        let _ = (pixbuf, background_size, placement);
        ToolUpdateResult::Unmodified
    }

    fn active(&self) -> bool {
        false
    }

    fn input_enabled(&self) -> bool;

    fn set_input_enabled(&mut self, value: bool);

    fn set_image_size(&mut self, _size: Vec2D) {}

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

pub trait AsAny {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl<T: Any> AsAny for T {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SelectionType {
    BoundingBox,
    BoundingBoxAllHandles,
    MiddleHandles,
    Line,
    Rectangle,
    Ellipse,
}

impl<T> DrawableClone for T
where
    T: 'static + Drawable + Clone,
{
    fn clone_box(&self) -> Box<dyn Drawable> {
        Box::new(self.clone())
    }
}

pub struct SpotlightData {
    pub tl: Vec2D,
    pub br: Vec2D,
    pub filled: bool,
    pub editing: bool,
    pub centered: bool,
}

pub trait Drawable: DrawableClone + Debug + AsAny {
    fn draw_baselayer(
        &self,
        canvas: &mut Canvas<OpenGl>,
        image: &ImgVec<RGBA8>,
        background_image_id: femtovg::ImageId,
        font: FontId,
        bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        let _ = image;
        let _ = background_image_id;
        self.draw(canvas, font, bounds)
    }
    fn draw_spotlight(
        &self,
        canvas: &mut Canvas<OpenGl>,
        bounds: (Vec2D, Vec2D),
        boxes: &Vec<SpotlightData>,
        spotlight_preview: bool,
        background_image_id: femtovg::ImageId,
        output_target: RenderTarget,
    ) {
        let _ = (
            canvas,
            bounds,
            boxes,
            spotlight_preview,
            background_image_id,
            output_target,
        );
    }
    fn draw(&self, canvas: &mut Canvas<OpenGl>, font: FontId, bounds: (Vec2D, Vec2D))
    -> Result<()>;
    fn handle_undo(&mut self) {}
    fn handle_redo(&mut self) {}
    fn get_rendering_mode(&self) -> RenderingMode {
        RenderingMode::Default
    }
    fn is_renderable(&self) -> bool {
        true
    }
    fn anchors_only_valid_after_redraw(&self) -> bool {
        false
    }
    /// The two points that define the shape and that `resize` takes back.
    /// For line like shapes, these are the start and end points of the line.
    /// For others, these are the corners of the box the shape was drawn in,
    /// not necessarily the extent of what is drawn.
    fn anchors(&self) -> Option<(Vec2D, Vec2D)> {
        None
    }
    /// The box `(tl, br)` around everything the drawable draws, except its
    /// shadow. By default the box around the anchors.
    fn get_bounding_box(&self) -> Option<(Vec2D, Vec2D)> {
        self.anchors().map(|(a, b)| ensure_bounding_box(a, b))
    }
    fn get_selection_type(&self) -> SelectionType {
        SelectionType::Rectangle
    }
    fn hit_test(&self, pos: Vec2D, tolerance: f32) -> bool {
        let _ = (pos, tolerance);
        false
    }
    fn translate(&mut self, delta: Vec2D) {
        let _ = delta;
    }
    /// Reshapes the drawable to the anchors `tl` and `br`, the inverse of `anchors`.
    /// For line like shapes, this sets the start and end points of the line.
    /// For others, this sets the corners of the box the shape is drawn in.
    fn resize(&mut self, tl: Vec2D, br: Vec2D, keep_aspect: bool) {
        let _ = (tl, br, keep_aspect);
    }
    fn get_style(&self) -> Option<&Style> {
        None
    }
    fn set_style(&mut self, style: Style) {
        let _ = style;
    }
    fn get_centered(&self) -> bool {
        let _ = self;
        false
    }
    fn set_centered(&mut self, centered: bool) {
        let _ = centered;
    }
    fn set_editing(&mut self, editing: bool) {
        let _ = editing;
    }
}

pub fn hit_test_rectangle(
    pos: Vec2D,
    top_left: Vec2D,
    size: Vec2D,
    tolerance: f32,
    filled: bool,
) -> bool {
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

pub fn hit_test_line(pos: Vec2D, start_point: Vec2D, points: &[Vec2D], tolerance: f32) -> bool {
    let mut last: Vec2D = start_point;
    for p in points.iter() {
        let next = start_point + *p;
        if pos.distance_to_segment(last, next) <= tolerance {
            return true;
        }
        last = next;
    }
    false
}

/// Returns the box around `a` and `b`, grown by `margin` on all sides.
pub fn bounding_box_with_margin(a: Vec2D, b: Vec2D, margin: f32) -> (Vec2D, Vec2D) {
    let (tl, br) = ensure_bounding_box(a, b);
    (tl - margin, br + margin)
}

pub fn calculate_line_bounds(start_point: Vec2D, points: &[Vec2D]) -> (Vec2D, Vec2D) {
    let mut tl = start_point;
    let mut br = start_point;
    for p in points.iter() {
        let abs = start_point + *p;
        tl = tl.min(abs);
        br = br.max(abs);
    }
    (tl, br)
}

fn resize_line_bounds(tl: Vec2D, br: Vec2D, start_point: &mut Vec2D, points: &mut [Vec2D]) {
    // Raw corners crossing each other means the shape is mirrored on that axis
    let flip_x = br.x < tl.x;
    let flip_y = br.y < tl.y;
    let (tl, br) = ensure_bounding_box(tl, br);

    let (current_tl, current_br) = calculate_line_bounds(*start_point, points);
    let current_size = current_br - current_tl;
    let new_size = br - tl;

    let axis_scale = |new: f32, current: f32, flip: bool| {
        let s = if current.abs() > f32::EPSILON {
            new / current
        } else {
            1.0
        };
        if flip { -s } else { s }
    };
    let scale = Vec2D::new(
        axis_scale(new_size.x, current_size.x, flip_x),
        axis_scale(new_size.y, current_size.y, flip_y),
    );

    // Points are relative to the start point, so they only need scaling
    for p in points.iter_mut() {
        *p *= scale;
    }
    // A mirrored axis grows from the opposite edge
    let origin = Vec2D::new(
        if flip_x { br.x } else { tl.x },
        if flip_y { br.y } else { tl.y },
    );
    *start_point = origin + (*start_point - current_tl) * scale;
}

/// How a line or a segment is kept straight while a modifier is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Straight {
    /// Shift: in steps of 15°
    Angle15,
    /// Ctrl: only horizontal
    Horizontal,
}

impl Straight {
    pub fn from_modifier(modifier: ModifierType) -> Option<Self> {
        if modifier.contains(ModifierType::CONTROL_MASK) {
            Some(Straight::Horizontal)
        } else if modifier.contains(ModifierType::SHIFT_MASK) {
            Some(Straight::Angle15)
        } else {
            None
        }
    }

    pub fn snap(self, v: Vec2D) -> Vec2D {
        match self {
            Straight::Angle15 => v.snapped_vector_15deg(),
            Straight::Horizontal => Vec2D::new(v.x, 0.0),
        }
    }

    /// `v` kept straight as the modifiers in `modifier` ask for, else unchanged.
    pub fn apply(modifier: ModifierType, v: Vec2D) -> Vec2D {
        Self::from_modifier(modifier).map_or(v, |straight| straight.snap(v))
    }
}

/// Hit test for the ellipse with the bounding box `top_left`, `size`.
pub fn hit_test_ellipse(
    pos: Vec2D,
    top_left: Vec2D,
    size: Vec2D,
    tolerance: f32,
    filled: bool,
) -> bool {
    let (top_left, size) = rect_ensure_positive_size(top_left, size);
    let (center, radii) = (top_left + size / 2.0, size / 2.0);

    let d = (pos - center) / (radii + tolerance);
    if d.norm2() > 1.0 {
        return false;
    }

    if filled {
        return true;
    }

    let inner_d = (pos - center) / (radii - tolerance);
    inner_d.norm2() > 1.0
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

use self::{brush::BrushTool, marker::MarkerTool};
use crate::tools::pixelate::PixelateMode;
pub use arrow::ArrowTool;
pub use blur::BlurTool;
pub use crop::CropTool;
pub use ellipse::EllipseTool;
pub use highlight::{HighlightTool, Highlighters};
pub use image::{ImagePlacement, ImageTool};
pub use line::LineTool;
pub use pixelate::PixelateTool;
pub use pointer::PointerTool;
pub use rectangle::RectangleTool;
pub use text::{Text, TextTool};

thread_local! {
    static CROP_TOOL_SINGLETON: OnceCell<Rc<RefCell<CropTool>>> = const { OnceCell::new() };
}

fn shared_crop_tool() -> Rc<RefCell<CropTool>> {
    CROP_TOOL_SINGLETON.with(|cell| {
        cell.get_or_init(|| Rc::new(RefCell::new(CropTool::default())))
            .clone()
    })
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Hash)]
pub enum Tools {
    Pointer,
    Crop,
    Pen,
    Highlighter,
    Brush,
    Line,
    Arrow,
    Rectangle,
    Ellipse,
    Text,
    Marker,
    FringePixelate,
    Pixelate,
    Fringe,
    Blur,
    Blackout,
    BlockHighlighter,
    HighlightSpotlight,
    Lensblur,
    Image,
}

impl fmt::Display for Tools {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Tools::Pointer => "Pointer",
            Tools::Crop => "Crop",
            Tools::Pen => "Pen",
            Tools::Highlighter => "Highlighter",
            Tools::Brush => "Brush",
            Tools::Line => "Line",
            Tools::Arrow => "Arrow",
            Tools::Rectangle => "Rectangle",
            Tools::Ellipse => "Ellipse",
            Tools::Text => "Text",
            Tools::Marker => "Marker",
            Tools::FringePixelate => "Fringe Pixelate",
            Tools::Pixelate => "Pixelate",
            Tools::Fringe => "Fringe Inpaint",
            Tools::Blur => "Blur",
            Tools::Blackout => "Blackout",
            Tools::Lensblur => "Lensblur",
            Tools::BlockHighlighter => "Block Highlighter",
            Tools::HighlightSpotlight => "Spotlight",
            Tools::Image => "Image",
        };
        write!(f, "{}", name)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ParseCommandError;

/// The names of the tools in the config, the keybinds and on the command line.
const TOOL_NAMES: [(&str, Tools); 20] = [
    ("pointer", Tools::Pointer),
    ("crop", Tools::Crop),
    ("pen", Tools::Pen),
    ("highlighter", Tools::Highlighter),
    ("brush", Tools::Brush),
    ("line", Tools::Line),
    ("arrow", Tools::Arrow),
    ("rectangle", Tools::Rectangle),
    ("ellipse", Tools::Ellipse),
    ("text", Tools::Text),
    ("marker", Tools::Marker),
    ("fringe-pixelate", Tools::FringePixelate),
    ("pixelate", Tools::Pixelate),
    ("fringe", Tools::Fringe),
    ("blur", Tools::Blur),
    ("blackout", Tools::Blackout),
    ("highlight", Tools::BlockHighlighter),
    ("spotlight", Tools::HighlightSpotlight),
    ("lensblur", Tools::Lensblur),
    ("image", Tools::Image),
];

impl FromStr for Tools {
    type Err = ParseCommandError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower_name = s.to_lowercase();
        TOOL_NAMES
            .iter()
            .find(|(name, _)| *name == lower_name)
            .map(|(_, tool)| *tool)
            .ok_or(ParseCommandError)
    }
}

// With the same names as the keybinds, e.g. "highlight", not the variant names.
impl<'de> serde::Deserialize<'de> for Tools {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = <String as serde::Deserialize>::deserialize(deserializer)?;
        name.parse().map_err(|_| {
            let names: Vec<&str> = TOOL_NAMES.iter().map(|(name, _)| *name).collect();
            serde::de::Error::custom(format!(
                "unknown tool `{name}`, expected one of {}",
                names.join(", ")
            ))
        })
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
        tools.insert(
            Tools::Pen,
            Rc::new(RefCell::new(BrushTool::with_mode(brush::BrushMode::Pen))),
        );
        tools.insert(
            Tools::Highlighter,
            Rc::new(RefCell::new(BrushTool::with_mode(
                brush::BrushMode::Highlighter,
            ))),
        );
        tools.insert(
            Tools::Brush,
            Rc::new(RefCell::new(BrushTool::with_mode(brush::BrushMode::Brush))),
        );
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
        tools.insert(Tools::Marker, Rc::new(RefCell::new(MarkerTool::default())));
        tools.insert(
            Tools::FringePixelate,
            Rc::new(RefCell::new(PixelateTool::with_mode(
                PixelateMode::FringePixelate,
            ))),
        );
        tools.insert(
            Tools::Pixelate,
            Rc::new(RefCell::new(PixelateTool::with_mode(
                PixelateMode::Pixelate,
            ))),
        );
        tools.insert(
            Tools::Fringe,
            Rc::new(RefCell::new(PixelateTool::with_mode(PixelateMode::Fringe))),
        );
        tools.insert(Tools::Blur, Rc::new(RefCell::new(BlurTool::default())));
        tools.insert(
            Tools::Blackout,
            Rc::new(RefCell::new(HighlightTool::with_mode(
                Highlighters::Blackout,
            ))),
        );
        tools.insert(
            Tools::Lensblur,
            Rc::new(RefCell::new(BlurTool::spotlight())),
        );
        tools.insert(
            Tools::BlockHighlighter,
            Rc::new(RefCell::new(HighlightTool::with_mode(Highlighters::Block))),
        );
        tools.insert(
            Tools::HighlightSpotlight,
            Rc::new(RefCell::new(HighlightTool::with_mode(
                Highlighters::Spotlight,
            ))),
        );
        tools.insert(Tools::Image, Rc::new(RefCell::new(ImageTool::default())));

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
            Tools::Pointer => self.pointer_tool.clone(),
            Tools::Crop => self.crop_tool.clone(),
            Tools::Text => self.text_tool.clone(),
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
            v if v == Tools::Pointer as u32 => Some(Tools::Pointer),
            v if v == Tools::Crop as u32 => Some(Tools::Crop),
            v if v == Tools::Pen as u32 => Some(Tools::Pen),
            v if v == Tools::Highlighter as u32 => Some(Tools::Highlighter),
            v if v == Tools::Brush as u32 => Some(Tools::Brush),
            v if v == Tools::Line as u32 => Some(Tools::Line),
            v if v == Tools::Arrow as u32 => Some(Tools::Arrow),
            v if v == Tools::Rectangle as u32 => Some(Tools::Rectangle),
            v if v == Tools::Ellipse as u32 => Some(Tools::Ellipse),
            v if v == Tools::Text as u32 => Some(Tools::Text),
            v if v == Tools::Marker as u32 => Some(Tools::Marker),
            v if v == Tools::FringePixelate as u32 => Some(Tools::FringePixelate),
            v if v == Tools::Pixelate as u32 => Some(Tools::Pixelate),
            v if v == Tools::Fringe as u32 => Some(Tools::Fringe),
            v if v == Tools::Blur as u32 => Some(Tools::Blur),
            v if v == Tools::Blackout as u32 => Some(Tools::Blackout),
            v if v == Tools::BlockHighlighter as u32 => Some(Tools::BlockHighlighter),
            v if v == Tools::HighlightSpotlight as u32 => Some(Tools::HighlightSpotlight),
            v if v == Tools::Lensblur as u32 => Some(Tools::Lensblur),
            v if v == Tools::Image as u32 => Some(Tools::Image),
            _ => None,
        })
    }
}

impl From<command_line::Tools> for Tools {
    fn from(tool: command_line::Tools) -> Self {
        match tool {
            command_line::Tools::Pointer => Self::Pointer,
            command_line::Tools::Crop => Self::Crop,
            command_line::Tools::Pen => Self::Pen,
            command_line::Tools::Highlighter => Self::Highlighter,
            command_line::Tools::Brush => Self::Brush,
            command_line::Tools::Line => Self::Line,
            command_line::Tools::Arrow => Self::Arrow,
            command_line::Tools::Rectangle => Self::Rectangle,
            command_line::Tools::Ellipse => Self::Ellipse,
            command_line::Tools::Text => Self::Text,
            command_line::Tools::Marker => Self::Marker,
            command_line::Tools::FringePixelate => Self::FringePixelate,
            command_line::Tools::Pixelate => Self::Pixelate,
            command_line::Tools::Fringe => Self::Fringe,
            command_line::Tools::Blur => Self::Blur,
            command_line::Tools::Blackout => Self::Blackout,
            command_line::Tools::Highlight => Self::BlockHighlighter,
            command_line::Tools::Spotlight => Self::HighlightSpotlight,
            command_line::Tools::Lensblur => Self::Lensblur,
            command_line::Tools::Image => Self::Image,
        }
    }
}
