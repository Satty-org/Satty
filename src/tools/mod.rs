use std::{
    borrow::Cow,
    cell::RefCell,
    collections::HashMap,
    fmt::{Debug, Display},
    rc::Rc,
};

use anyhow::Result;
use femtovg::{Canvas, FontId, Paint, Path as FemtoPath, renderer::OpenGl};
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
    math::{Rect, Vec2D},
    sketch_board::{InputEvent, KeyEventMsg, MouseEventMsg, SketchBoardInput, TextEventMsg},
    style::Style,
};

use satty_cli::command_line;

mod arrow;
mod blur;
mod brush;
mod crop;
mod ellipse;
mod highlight;
mod line;
mod marker;
mod pointer;
mod rectangle;
mod text;

pub enum ToolEvent {
    Activated,
    Deactivated,
    Input(InputEvent),
    StyleChanged(Style),
}

pub trait Tool {
    fn handle_event(&mut self, event: ToolEvent) -> ToolUpdateResult {
        match event {
            ToolEvent::Activated => self.handle_activated(),
            ToolEvent::Deactivated => self.handle_deactivated(),
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

    fn set_im_context(&mut self, _context: Option<InputContext>) {}

    fn get_drawable(&self) -> Option<&dyn Drawable>;

    /// Optional overlay drawn on top of the in-progress drawable
    /// (e.g. selection handles). Computed fresh each frame so visuals stay in
    /// sync after undo/redo or external mutations.
    ///
    /// `selected` is the live drawable matching `selected_drawable()` from the
    /// renderer's stack, passed in to avoid the tool re-entering the renderer
    /// (which already holds a borrow during the render path).
    fn build_overlay(&self, _selected: Option<&dyn Drawable>) -> Option<Box<dyn Drawable>> {
        None
    }

    /// If the tool has a current selection, return its id. Used by the renderer
    /// to know which drawable is selected (for selection visuals when not dragging).
    fn selected_drawable(&self) -> Option<DrawableId> {
        self.selected_drawables().first().copied()
    }

    /// All currently-selected drawable ids. Default returns single selection
    /// (or empty); tools that support multi-selection override.
    fn selected_drawables(&self) -> Vec<DrawableId> {
        Vec::new()
    }

    /// If the tool is actively dragging an existing drawable, return its id.
    /// Used by the renderer to skip rendering the original (the tool's
    /// `get_drawable()` returns a moved copy during the drag).
    fn dragging_drawable_id(&self) -> Option<DrawableId> {
        None
    }

    fn get_tool_type(&self) -> Tools;

    fn set_sender(&mut self, sender: Sender<SketchBoardInput>);

    /// Inject a handle the tool can use to query the committed-drawable stack.
    /// Currently only the pointer tool needs this (for hit-testing and pulling
    /// a working copy of a selection).
    fn set_drawable_store(&mut self, _store: Rc<dyn DrawableStore>) {}

    /// Switch the arrow geometry (only meaningful for `ArrowTool`). Default
    /// no-op so the toolbar can broadcast without checking tool type.
    fn set_arrow_style(&mut self, _style: ArrowStyle) {}
}

/// Read-only view of the committed-drawable stack, exposed to tools that need
/// to do hit-testing or pull working copies of existing drawables.
pub trait DrawableStore {
    fn hit_test(&self, point: Vec2D, tolerance: f32) -> Option<DrawableId>;
    fn clone_drawable(&self, id: DrawableId) -> Option<Box<dyn Drawable>>;
    /// Drawable ids whose bounds overlap `rect`. Used for marquee / lasso
    /// selection.
    fn drawables_in_rect(&self, rect: Rect) -> Vec<DrawableId>;
    /// All committed drawable ids (back-to-front order). Used for Ctrl+A.
    fn all_drawable_ids(&self) -> Vec<DrawableId>;
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

    /// Axis-aligned bounding box in image coordinates. `None` means "not selectable"
    /// (e.g. an in-progress drawable still being drawn).
    fn bounds(&self) -> Option<Rect> {
        None
    }

    /// Whether `point` (image coordinates) hits this drawable.
    /// `tolerance` is extra picking slack in image-space pixels — sketch_board passes
    /// a value scaled to the current zoom.
    /// Default falls through to bounds-containment, which is correct for filled shapes.
    fn hit_test(&self, point: Vec2D, tolerance: f32) -> bool {
        self.bounds()
            .map(|b| b.inflated(tolerance).contains(point))
            .unwrap_or(false)
    }

    /// Translate the drawable by `delta` (image coordinates).
    /// Default is a no-op so non-movable drawables (e.g. crop overlays) don't need
    /// to implement it.
    fn translate(&mut self, _delta: Vec2D) {}

    /// Handles to expose for direct manipulation when this drawable is selected.
    /// Default is empty (move-only).
    fn handles(&self) -> Vec<Handle> {
        Vec::new()
    }

    /// Move a handle to `to` (image coordinates). The drawable updates itself
    /// according to the handle's semantics (e.g. arrow endpoint, rect corner).
    /// Default is a no-op.
    fn move_handle(&mut self, _handle: HandleId, _to: Vec2D) {}

    /// Apply a new style to the drawable (color, size, fill, …). Used when the
    /// user picks a different color/size in the toolbar while a drawable is
    /// selected, so the toolbar acts on the selection rather than only on
    /// future shapes. Default is a no-op for drawables that don't carry a
    /// mutable style.
    fn set_style(&mut self, _style: Style) {}

    /// Render a Selection "glow" — a wide, semi-transparent
    /// blue trace of the shape, drawn under the original. The default uses
    /// the axis-aligned `bounds()` (good enough for filled shapes); shapes
    /// like arrow/line/ellipse override to trace their actual outline.
    fn render_glow(
        &self,
        canvas: &mut Canvas<OpenGl>,
        _font: FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        let Some(b) = self.bounds() else {
            return Ok(());
        };
        canvas.save();
        let pad = 6.0;
        let mut path = FemtoPath::new();
        path.rounded_rect(
            b.pos.x - pad,
            b.pos.y - pad,
            b.size.x + pad * 2.0,
            b.size.y + pad * 2.0,
            6.0,
        );
        let mut paint = Paint::color(GLOW_COLOR);
        paint.set_line_width(GLOW_STROKE_WIDTH);
        canvas.stroke_path(&path, &paint);
        canvas.restore();
        Ok(())
    }
}

/// Selection accent (used for handles + glow + hover cursor halo).
pub const SELECTION_BLUE: femtovg::Color = femtovg::Color {
    r: 0.18,
    g: 0.53,
    b: 0.87,
    a: 1.0,
};
/// Semi-transparent variant for the glow trace.
pub const GLOW_COLOR: femtovg::Color = femtovg::Color {
    r: 0.18,
    g: 0.53,
    b: 0.87,
    a: 0.45,
};
pub const GLOW_STROKE_WIDTH: f32 = 8.0;
/// Wider glow for shapes whose drawable already strokes a fat outline at the
/// same path (currently only the Standard arrow's rounded-corner outline).
/// Without this, the outline would entirely cover an 8 px glow.
pub const GLOW_STROKE_WIDTH_WIDE: f32 = 14.0;

/// A handle exposed by a drawable for direct manipulation.
#[derive(Debug, Clone, Copy)]
pub struct Handle {
    pub id: HandleId,
    pub pos: Vec2D,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleId {
    /// Linear-shape endpoints (arrow, line).
    Start,
    End,
    /// Mid-shape control point (curved/double arrow Bezier control).
    Control,
    /// Bounding-box corners.
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    /// Bounding-box edge midpoints.
    Top,
    Right,
    Bottom,
    Left,
}

/// 8 standard bounding-box handles (4 corners, 4 edge midpoints).
/// Shared by rectangle / ellipse / blur / highlight-block.
pub fn bbox_handles(rect: Rect) -> Vec<Handle> {
    let tl = rect.top_left();
    let tr = rect.top_right();
    let bl = rect.bottom_left();
    let br = rect.bottom_right();
    let center = rect.center();
    vec![
        Handle {
            id: HandleId::TopLeft,
            pos: tl,
        },
        Handle {
            id: HandleId::TopRight,
            pos: tr,
        },
        Handle {
            id: HandleId::BottomLeft,
            pos: bl,
        },
        Handle {
            id: HandleId::BottomRight,
            pos: br,
        },
        Handle {
            id: HandleId::Top,
            pos: Vec2D::new(center.x, tl.y),
        },
        Handle {
            id: HandleId::Bottom,
            pos: Vec2D::new(center.x, br.y),
        },
        Handle {
            id: HandleId::Left,
            pos: Vec2D::new(tl.x, center.y),
        },
        Handle {
            id: HandleId::Right,
            pos: Vec2D::new(br.x, center.y),
        },
    ]
}

/// Resize a canonical bounding box given a handle being dragged to `to`.
/// Returns a canonicalized rect.
pub fn bbox_resize(rect: Rect, handle: HandleId, to: Vec2D) -> Rect {
    let tl = rect.top_left();
    let br = rect.bottom_right();
    let (new_tl, new_br) = match handle {
        HandleId::TopLeft => (to, br),
        HandleId::TopRight => (Vec2D::new(tl.x, to.y), Vec2D::new(to.x, br.y)),
        HandleId::BottomLeft => (Vec2D::new(to.x, tl.y), Vec2D::new(br.x, to.y)),
        HandleId::BottomRight => (tl, to),
        HandleId::Top => (Vec2D::new(tl.x, to.y), br),
        HandleId::Bottom => (tl, Vec2D::new(br.x, to.y)),
        HandleId::Left => (Vec2D::new(to.x, tl.y), br),
        HandleId::Right => (tl, Vec2D::new(to.x, br.y)),
        _ => return rect,
    };
    Rect::from_corners(new_tl, new_br)
}

/// Identifier for a committed drawable on the sketch stack.
/// Stable across moves, edits, and undo/redo cycles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DrawableId(pub u64);

/// A drawable that has been committed to the stack, paired with its stable ID.
#[derive(Debug)]
pub struct Stacked {
    pub id: DrawableId,
    pub drawable: Box<dyn Drawable>,
}

#[derive(Debug)]
pub enum ToolUpdateResult {
    Commit(Box<dyn Drawable>),
    /// Replace the existing drawable identified by `DrawableId` with the new one.
    /// Recorded as a single Modify undo action.
    ModifyDrawable(DrawableId, Box<dyn Drawable>),
    /// Replace many drawables atomically (Batch undo). Used for multi-select
    /// restyle.
    ModifyDrawables(Vec<(DrawableId, Box<dyn Drawable>)>),
    /// Remove the drawable from the stack. Recorded as a Remove undo action so
    /// it can be restored.
    DeleteDrawable(DrawableId),
    /// Remove a set of drawables atomically. Recorded as a single Batch undo
    /// action so one Ctrl+Z restores them all.
    DeleteDrawables(Vec<DrawableId>),
    Redraw,
    Unmodified,
    StopPropagation,
    RedrawAndStopPropagation,
}

/// A reversible change to the drawable stack. Stored on undo/redo stacks.
///
/// Same variant moves between stacks for `Modify`. `Add` and `Remove` are paired:
/// undoing an `Add` produces a `Remove` on the redo stack (and vice versa), since
/// the live drawable storage location differs between the two states.
#[derive(Debug)]
pub enum UndoAction {
    /// A drawable with this id was added; it currently lives in the stack.
    Add(DrawableId),
    /// A drawable was removed; this action holds it until restored.
    Remove {
        id: DrawableId,
        idx: usize,
        drawable: Box<dyn Drawable>,
    },
    /// A drawable was modified. `prev` is the state to restore on the next swap.
    /// (After a swap, `prev` becomes the *new* previous, so the same variant can
    /// move between undo/redo stacks symmetrically.)
    Modify {
        id: DrawableId,
        prev: Box<dyn Drawable>,
    },
    /// Group of actions applied/reversed atomically — single Ctrl+Z undoes
    /// the whole group. Used for multi-select operations like deleting a set
    /// of drawables at once.
    Batch(Vec<UndoAction>),
}

pub use arrow::{ArrowStyle, ArrowTool};
pub use blur::BlurTool;
pub use crop::CropTool;
pub use ellipse::EllipseTool;
pub use highlight::{HighlightTool, Highlighters};
pub use line::LineTool;
pub use rectangle::RectangleTool;
pub use text::TextTool;

use self::{brush::BrushTool, marker::MarkerTool, pointer::PointerTool};

// Re-export pointer-tool tunables that other modules (e.g. sketch_board's
// hover cursor) want to share.
pub use self::pointer::{HANDLE_HIT_RADIUS, HIT_TOLERANCE};

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

impl Tools {
    pub fn display_name(&self) -> &'static str {
        match self {
            Tools::Pointer => "Pointer",
            Tools::Crop => "Crop",
            Tools::Brush => "Brush",
            Tools::Line => "Line",
            Tools::Arrow => "Arrow",
            Tools::Rectangle => "Rectangle",
            Tools::Ellipse => "Ellipse",
            Tools::Text => "Text",
            Tools::Marker => "Numbered Marker",
            Tools::Blur => "Blur",
            Tools::Highlight => "Highlight",
        }
    }
}

// used for printing
impl Display for Tools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pointer => write!(f, "pointer"),
            Self::Crop => write!(f, "crop"),
            Self::Line => write!(f, "line"),
            Self::Arrow => write!(f, "arrow"),
            Self::Rectangle => write!(f, "rectangle"),
            Self::Ellipse => write!(f, "ellipse"),
            Self::Text => write!(f, "text"),
            Self::Marker => write!(f, "marker"),
            Self::Blur => write!(f, "blur"),
            Self::Highlight => write!(f, "highlight"),
            Self::Brush => write!(f, "brush"),
        }
    }
}

pub struct ToolsManager {
    tools: HashMap<Tools, Rc<RefCell<dyn Tool>>>,
    crop_tool: Rc<RefCell<CropTool>>,
}

impl ToolsManager {
    pub fn new() -> Self {
        let mut tools: HashMap<Tools, Rc<RefCell<dyn Tool>>> = HashMap::new();
        //tools.insert(Tools::Crop, Rc::new(RefCell::new(CropTool::default())));
        tools.insert(
            Tools::Pointer,
            Rc::new(RefCell::new(PointerTool::default())),
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
        tools.insert(Tools::Text, Rc::new(RefCell::new(TextTool::default())));
        tools.insert(Tools::Blur, Rc::new(RefCell::new(BlurTool::default())));
        tools.insert(
            Tools::Highlight,
            Rc::new(RefCell::new(HighlightTool::default())),
        );
        tools.insert(Tools::Marker, Rc::new(RefCell::new(MarkerTool::default())));
        tools.insert(Tools::Brush, Rc::new(RefCell::new(BrushTool::default())));

        let crop_tool = Rc::new(RefCell::new(CropTool::default()));
        Self { tools, crop_tool }
    }

    pub fn get(&self, tool: &Tools) -> Rc<RefCell<dyn Tool>> {
        match tool {
            Tools::Crop => self.crop_tool.clone(),
            _ => self
                .tools
                .get(tool)
                .unwrap_or_else(|| {
                    panic!("Did you add the requested too {tool:#?} to the tools HashMap?")
                })
                .clone(),
        }
    }

    pub fn get_crop_tool(&self) -> Rc<RefCell<CropTool>> {
        self.crop_tool.clone()
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
