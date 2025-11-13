use anyhow::Result;
use femtovg::{FontId, Path};
use relm4::gtk::gdk::{Key, ModifierType};

use crate::{
    math::{Angle, Vec2D},
    sketch_board::{MouseButton, MouseEventMsg, MouseEventType},
    style::Style,
};

use super::{Drawable, DrawableClone, Tool, ToolUpdateResult, Tools};

#[derive(Clone, Copy, Debug)]
pub struct Arrow {
    start: Vec2D,
    end: Option<Vec2D>,
    style: Style,
}

#[derive(Default)]
pub struct ArrowTool {
    arrow: Option<Arrow>,
    style: Style,
    input_enabled: bool,
}

impl Tool for ArrowTool {
    fn input_enabled(&self) -> bool {
        self.input_enabled
    }

    fn set_input_enabled(&mut self, value: bool) {
        self.input_enabled = value;
    }

    fn get_tool_type(&self) -> super::Tools {
        Tools::Arrow
    }

    fn handle_mouse_event(&mut self, event: MouseEventMsg) -> ToolUpdateResult {
        match event.type_ {
            MouseEventType::BeginDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                // start new
                self.arrow = Some(Arrow {
                    start: event.pos,
                    end: None,
                    style: self.style,
                });

                ToolUpdateResult::Redraw
            }
            MouseEventType::EndDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                if let Some(a) = &mut self.arrow {
                    if event.pos == Vec2D::zero() {
                        self.arrow = None;

                        ToolUpdateResult::Redraw
                    } else {
                        if event.modifier.intersects(ModifierType::SHIFT_MASK) {
                            a.end = Some(a.start + event.pos.snapped_vector_15deg());
                        } else {
                            a.end = Some(a.start + event.pos);
                        }
                        let result = a.clone_box();
                        self.arrow = None;

                        ToolUpdateResult::Commit(result)
                    }
                } else {
                    ToolUpdateResult::Unmodified
                }
            }
            MouseEventType::UpdateDrag => {
                if event.button == MouseButton::Middle {
                    return ToolUpdateResult::Unmodified;
                }

                if let Some(a) = &mut self.arrow {
                    if event.pos == Vec2D::zero() {
                        return ToolUpdateResult::Unmodified;
                    }
                    if event.modifier.intersects(ModifierType::SHIFT_MASK) {
                        a.end = Some(a.start + event.pos.snapped_vector_15deg());
                    } else {
                        a.end = Some(a.start + event.pos);
                    }

                    ToolUpdateResult::Redraw
                } else {
                    ToolUpdateResult::Unmodified
                }
            }
            _ => ToolUpdateResult::Unmodified,
        }
    }

    fn handle_key_event(&mut self, event: crate::sketch_board::KeyEventMsg) -> ToolUpdateResult {
        if event.key == Key::Escape && self.arrow.is_some() {
            self.arrow = None;
            ToolUpdateResult::Redraw
        } else {
            ToolUpdateResult::Unmodified
        }
    }

    fn get_drawable(&self) -> Option<&dyn Drawable> {
        match &self.arrow {
            Some(d) => Some(d),
            None => None,
        }
    }

    fn handle_style_event(&mut self, style: Style) -> ToolUpdateResult {
        self.style = style;
        ToolUpdateResult::Unmodified
    }
}

impl Drawable for Arrow {
    fn draw(
        &self,
        canvas: &mut femtovg::Canvas<femtovg::renderer::OpenGl>,
        _font: FontId,
        _bounds: (Vec2D, Vec2D),
    ) -> Result<()> {
        let end = match self.end {
            Some(e) => e,
            None => return Ok(()), // exit if no end
        };

        // Fat arrow:
        //          C
        //  E       #
        //    ######G###
        //  A ######D##### B
        //    ##########
        //  F       #
        //
        //
        // Thin arrow:
        //          C
        //           \
        //  A -------- B
        //           /
        //
        // A: start
        // B: end
        // C: head side
        // D: midpoint
        // E: tail side
        // F: tail side
        // G: the cross-section of C - D on the tail side.
        // Head: the point of the head at the end of the arrow (2, 3, 4).
        // Tail: the line from the start to the midpoint (1 - 4).
        // Side: the sloped side of the arrow head (3 - 2).
        // Midpoint: where the tail ends and the head begins (4).
        // Arrow length: the distance from the start to the end (1 - 2).
        // Head angle: the angle of the head point at end (2).
        // Tail width: the distance from tail side to tail side (5 - 6).

        let arrow_offset = end - self.start;
        let arrow_length = arrow_offset.norm();
        let arrow_direction = arrow_offset * (1.0 / arrow_length);

        // We rotate the canvas so that we can draw the arrow on the x-axis.
        // start will be at (0,0)
        // end will be at (length, 0)
        canvas.save();
        canvas.translate(self.start.x, self.start.y);
        canvas.rotate(arrow_direction.angle().radians);

        // The width of the tail (double distance from start to head side)
        let tail_width = self
            .style
            .size
            .to_arrow_tail_width(self.style.annotation_size_factor);
        // The length of the (sloped) side of the arrow head (distance from end to head side).
        let head_side_length = self
            .style
            .size
            .to_arrow_head_length(self.style.annotation_size_factor);
        // The offset of the midpoint is the distance the midpoint moves toward the end of the arrow.
        // A offset of 0 will place the midpoint right below the head side.
        // A negative value will result in a diamond head.
        // A positive value will result in a sharper head.
        let midpoint_offset = head_side_length * 0.1;

        let head_angle = Angle::from_degrees(60.0); // The angle of the point of the arrow head.

        let tail_half_width = tail_width / 2.0;
        let head_half_angle = head_angle * 0.5;
        let head_left =
            Vec2D::new(arrow_length, 0.0) - Vec2D::from_angle(head_half_angle) * head_side_length;
        let midpoint_x = head_left.x + midpoint_offset;

        if self.style.fill {
            // Draw a 'fat' arrow.
            let mut path = Path::new();
            path.move_to(midpoint_x, tail_half_width); // G
            path.line_to(head_left.x, -head_left.y); // C
            path.line_to(arrow_length, 0.0); // B
            path.line_to(head_left.x, head_left.y); // C (mirrored)
            path.line_to(midpoint_x, -tail_half_width); // G (mirrored)
            if midpoint_x > 0.0 {
                // If the midpoint is placed _before_ the start, there is only a head and no tail.
                // We can skip the beginning of the tail.
                path.line_to(0.0, -tail_half_width); // F
                path.line_to(0.0, tail_half_width); // E
            }
            path.close();

            canvas.fill_path(&path, &self.style.into());
        } else {
            // Draw a 'thin' arrow head.
            let mut path = Path::new();
            path.move_to(head_left.x, -head_left.y); // C
            path.line_to(arrow_length, 0.0); // B
            path.line_to(head_left.x, head_left.y); // C (mirrored)

            path.move_to(0.0, 0.0); // A
            path.line_to(arrow_length, 0.0); // B

            canvas.stroke_path(&path, &self.style.into());
        }

        canvas.restore();
        Ok(())
    }
}
