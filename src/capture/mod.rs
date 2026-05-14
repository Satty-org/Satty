use anyhow::Result;
use relm4::gtk::gdk_pixbuf::Pixbuf;

pub mod wlr_screencopy;

#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

pub fn capture_output() -> Result<Pixbuf> {
    wlr_screencopy::capture(None)
}

pub fn capture_region(rect: Rect) -> Result<Pixbuf> {
    wlr_screencopy::capture(Some(rect))
}
