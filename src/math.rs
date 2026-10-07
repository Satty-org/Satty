use std::{
    f32::consts::PI,
    fmt::Display,
    ops::{Add, AddAssign, Div, Mul, Sub, SubAssign},
};

pub fn get_closest_aspect_ratio(aspect_ratio: f32, aspect_ratios: &[(f32, f32)]) -> (f32, f32) {
    if aspect_ratios.is_empty() {
        return (1.0, 1.0); // default aspect ratio if list is empty
    }
    let closest_ar = aspect_ratios
        .iter()
        .min_by(|a, b| {
            let da = (aspect_ratio - a.0 / a.1)
                .abs()
                .min((aspect_ratio - a.1 / a.0).abs());
            let db = (aspect_ratio - b.0 / b.1)
                .abs()
                .min((aspect_ratio - b.1 / b.0).abs());
            da.partial_cmp(&db).unwrap() // safe because there is a min
        })
        .unwrap(); // safe because list is not empty 
    if (aspect_ratio - closest_ar.0 / closest_ar.1).abs()
        <= (aspect_ratio - closest_ar.1 / closest_ar.0).abs()
    {
        *closest_ar
    } else {
        (closest_ar.1, closest_ar.0)
    }
}

#[derive(Default, Debug, Copy, Clone, PartialEq)]
pub struct Vec2D {
    pub x: f32,
    pub y: f32,
}

#[derive(Default, Debug, Copy, Clone, PartialEq)]
pub struct Angle {
    pub radians: f32,
}
impl Angle {
    pub fn from_radians(radians: f32) -> Self {
        Self { radians }
    }

    pub fn from_degrees(degrees: f32) -> Self {
        Self {
            radians: degrees * PI / 180.0,
        }
    }

    pub fn cos(&self) -> f32 {
        self.radians.cos()
    }

    pub fn sin(&self) -> f32 {
        self.radians.sin()
    }
}

impl Mul<f32> for Angle {
    type Output = Angle;

    fn mul(self, rhs: f32) -> Self::Output {
        Angle::from_radians(self.radians * rhs)
    }
}

impl Vec2D {
    pub fn zero() -> Self {
        Self { x: 0.0, y: 0.0 }
    }

    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub fn norm(&self) -> f32 {
        (self.x * self.x + self.y * self.y).sqrt()
    }

    pub fn norm2(&self) -> f32 {
        self.x * self.x + self.y * self.y
    }

    pub fn area(&self) -> f32 {
        self.x * self.y
    }

    pub fn abs(&self) -> Self {
        Self {
            x: self.x.abs(),
            y: self.y.abs(),
        }
    }

    pub fn round(&self) -> Self {
        Self {
            x: self.x.round(),
            y: self.y.round(),
        }
    }

    pub fn min(self, other: Self) -> Self {
        Self {
            x: self.x.min(other.x),
            y: self.y.min(other.y),
        }
    }

    pub fn max(self, other: Self) -> Self {
        Self {
            x: self.x.max(other.x),
            y: self.y.max(other.y),
        }
    }

    /**
     * Get the angle of the vector.
     * Angle of 0 is the positive x-axis.
     * Angle of PI/2 is the positive y-axis.
     */
    pub fn angle(&self) -> Angle {
        Angle::from_radians(self.y.atan2(self.x))
    }

    /**
     * Create a vector from an angle.
     * Angle of 0 is the positive x-axis.
     * Angle of PI/2 is the positive y-axis.
     */
    pub fn from_angle(angle: Angle) -> Vec2D {
        Vec2D::new(angle.cos(), angle.sin())
    }

    pub fn snapped_vector_15deg(&self) -> Vec2D {
        let current_angle = (self.y / self.x).atan();
        let current_norm2 = self.norm2();
        let new_angle = (current_angle / 0.261_799_4).round() * 0.261_799_4;

        let (mut a, mut b) = if new_angle.abs() < PI / 4.0
        // 45°
        {
            let b = (current_norm2 / ((PI / 2.0 - new_angle).tan().powi(2) + 1.0)).sqrt();
            let a = (current_norm2 - b * b).sqrt();
            (a, b)
        } else {
            let a = (current_norm2 / (new_angle.tan().powi(2) + 1.0)).sqrt();
            let b = (current_norm2 - a * a).sqrt();
            (a, b)
        };

        // round to pixels to avoid small inaccuracies
        (a, b) = (a.round(), b.round());

        if self.x >= 0.0 && self.y >= 0.0 {
            Vec2D::new(a, b)
        } else if self.x < 0.0 && self.y >= 0.0 {
            Vec2D::new(-a, b)
        } else if self.x >= 0.0 && self.y < 0.0 {
            Vec2D::new(a, -b)
        } else {
            Vec2D::new(-a, -b)
        }
    }

    pub fn is_zero(&self) -> bool {
        self.x.abs() < f32::EPSILON && self.y.abs() < f32::EPSILON
    }

    pub fn distance_to(&self, other: &Vec2D) -> f32 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        (dx * dx + dy * dy).sqrt()
    }

    pub fn distance_to_segment(&self, a: Vec2D, b: Vec2D) -> f32 {
        let ab = b - a;
        if ab.is_zero() {
            return self.distance_to(&a);
        }
        let ap = *self - a;
        let factor = (ap * ab / ab.norm2()).clamp(0.0, 1.0);
        let projected_point = a + ab * factor;
        self.distance_to(&projected_point)
    }
}

impl Add for Vec2D {
    type Output = Vec2D;

    fn add(self, rhs: Self) -> Self::Output {
        Self::Output {
            x: self.x + rhs.x,
            y: self.y + rhs.y,
        }
    }
}

impl Add<f32> for Vec2D {
    type Output = Vec2D;

    fn add(self, rhs: f32) -> Self::Output {
        Self::Output {
            x: self.x + rhs,
            y: self.y + rhs,
        }
    }
}

impl AddAssign for Vec2D {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs
    }
}

impl Sub for Vec2D {
    type Output = Vec2D;

    fn sub(self, rhs: Self) -> Self::Output {
        Self::Output {
            x: self.x - rhs.x,
            y: self.y - rhs.y,
        }
    }
}

impl Sub<f32> for Vec2D {
    type Output = Vec2D;

    fn sub(self, rhs: f32) -> Self::Output {
        Self::Output {
            x: self.x - rhs,
            y: self.y - rhs,
        }
    }
}

impl SubAssign for Vec2D {
    fn sub_assign(&mut self, rhs: Self) {
        *self = *self - rhs;
    }
}

impl Mul<f32> for Vec2D {
    type Output = Vec2D;

    fn mul(self, rhs: f32) -> Self::Output {
        Vec2D::new(self.x * rhs, self.y * rhs)
    }
}

impl Mul<Vec2D> for Vec2D {
    type Output = f32;

    fn mul(self, rhs: Vec2D) -> Self::Output {
        self.x * rhs.x + self.y * rhs.y
    }
}

impl Div<f32> for Vec2D {
    type Output = Vec2D;

    fn div(self, rhs: f32) -> Self::Output {
        Vec2D::new(self.x / rhs, self.y / rhs)
    }
}

impl Div<Vec2D> for Vec2D {
    type Output = Vec2D;

    fn div(self, rhs: Vec2D) -> Self::Output {
        Vec2D::new(self.x / rhs.x, self.y / rhs.y)
    }
}

impl Display for Vec2D {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "({},{})", self.x, self.y)
    }
}

pub fn rect_ensure_positive_size(pos: Vec2D, size: Vec2D) -> (Vec2D, Vec2D) {
    let (pos_x, size_x) = if size.x > 0.0 {
        (pos.x, size.x)
    } else {
        ((pos.x + size.x), size.x.abs())
    };

    let (pos_y, size_y) = if size.y > 0.0 {
        (pos.y, size.y)
    } else {
        ((pos.y + size.y), size.y.abs())
    };

    (Vec2D::new(pos_x, pos_y), Vec2D::new(size_x, size_y))
}

pub fn rect_ensure_in_bounds(rect: (Vec2D, Vec2D), bounds: (Vec2D, Vec2D)) -> (Vec2D, Vec2D) {
    let (mut pos, mut size) = rect;

    // outside of bounds entirely
    if pos.x + size.x < bounds.0.x
        || pos.y + size.y < bounds.0.y
        || pos.x > bounds.1.x
        || pos.y > bounds.1.y
    {
        return (Vec2D::zero(), Vec2D::zero());
    }

    // The part sticking out has to be measured before pos is moved onto the
    // bound, otherwise the difference is always zero and size stays untouched.
    if pos.x < bounds.0.x {
        size.x = (size.x - (bounds.0.x - pos.x)).max(0.0);
        pos.x = bounds.0.x;
    }

    if pos.y < bounds.0.y {
        size.y = (size.y - (bounds.0.y - pos.y)).max(0.0);
        pos.y = bounds.0.y;
    }

    if pos.x + size.x > bounds.1.x {
        size.x = bounds.1.x - pos.x;
    }

    if pos.y + size.y > bounds.1.y {
        size.y = bounds.1.y - pos.y;
    }

    (pos, size)
}

// Return the bounding box tl, br for two points
pub fn ensure_bounding_box(a: Vec2D, b: Vec2D) -> (Vec2D, Vec2D) {
    (a.min(b), a.max(b))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentResizeTarget {
    Low,
    High,
    // Edge-center handles, named after the axis they move along.
    MiddleX,
    MiddleY,
}

// We use this for line and arrow when doing a resize operation with the pointer tool.
// The bounds of the bounding box are not meaningful for horizontal or vertical lines.
// Maps the segment `start`-`end` into resized bounds `(tl, br)`, preserving its direction.
// With `keep_aspect` (Shift/Ctrl are the same), it switches the movement direction on middle
// handle for vertical/horizontal ones and does the 15°-angle-snap on end points.
pub fn resize_segment(
    segment: (Vec2D, Vec2D),
    bounding_box: (Vec2D, Vec2D),
    delta: Vec2D,
    keep_aspect: bool,
    target: Option<SegmentResizeTarget>,
) -> (Vec2D, Vec2D) {
    let new_segment = resize_segment_to_bounds(segment, bounding_box, target);
    if keep_aspect {
        move_or_snap_segment(segment, new_segment, delta, target)
    } else {
        new_segment
    }
}

// Do the actual resizing of the segment.
fn resize_segment_to_bounds(
    segment: (Vec2D, Vec2D),
    bounding_box: (Vec2D, Vec2D),
    target: Option<SegmentResizeTarget>,
) -> (Vec2D, Vec2D) {
    let (start, end) = segment;
    let (tl, br) = bounding_box;
    let (otl, obr) = ensure_bounding_box(start, end);
    let start_is_left = start.x <= end.x;
    let start_is_top = start.y <= end.y;
    let mapped_start = Vec2D::new(
        if start_is_left { tl.x } else { br.x },
        if start_is_top { tl.y } else { br.y },
    );
    let mapped_end = Vec2D::new(
        if start_is_left { br.x } else { tl.x },
        if start_is_top { br.y } else { tl.y },
    );

    let flat_x = otl.x == obr.x;
    let flat_y = otl.y == obr.y;
    if flat_x == flat_y {
        return (mapped_start, mapped_end);
    }

    let (flat_lo_changed, flat_hi_changed, span_lo_changed, span_hi_changed) = if flat_x {
        (tl.x != otl.x, br.x != obr.x, tl.y != otl.y, br.y != obr.y)
    } else {
        (tl.y != otl.y, br.y != obr.y, tl.x != otl.x, br.x != obr.x)
    };
    if flat_lo_changed == flat_hi_changed {
        return (mapped_start, mapped_end);
    }
    let flat_value = match (flat_x, flat_lo_changed) {
        (true, true) => tl.x,
        (true, false) => br.x,
        (false, true) => tl.y,
        (false, false) => br.y,
    };

    let start_is_low = if flat_x { start_is_top } else { start_is_left };
    let (move_start, move_end) = match target {
        Some(SegmentResizeTarget::Low) => (start_is_low, !start_is_low),
        Some(SegmentResizeTarget::High) => (!start_is_low, start_is_low),
        Some(SegmentResizeTarget::MiddleX | SegmentResizeTarget::MiddleY) => (true, true),
        None => match (span_lo_changed, span_hi_changed) {
            (true, false) => (start_is_low, !start_is_low),
            (false, true) => (!start_is_low, start_is_low),
            _ => (true, true),
        },
    };

    let apply_flat_value = |point: Vec2D| {
        if flat_x {
            Vec2D::new(flat_value, point.y)
        } else {
            Vec2D::new(point.x, flat_value)
        }
    };
    (
        if move_start {
            apply_flat_value(mapped_start)
        } else {
            start
        },
        if move_end {
            apply_flat_value(mapped_end)
        } else {
            end
        },
    )
}

// Keep-aspect handling: the dragged end follows the pointer but its direction relative to
// the fixed end is snapped to 15° steps. A segment dragged by an edge-center handle is moved
// along or perpendicular to its own direction instead.
// The resize handles' own aspect math is not meaningful for segments, so `new_segment` is only
// used to tell which end is dragged.
fn move_or_snap_segment(
    segment: (Vec2D, Vec2D),
    new_segment: (Vec2D, Vec2D),
    delta: Vec2D,
    target: Option<SegmentResizeTarget>,
) -> (Vec2D, Vec2D) {
    let (start, end) = segment;
    let start_moved = new_segment.0.distance_to(&start) > f32::EPSILON;
    let end_moved = new_segment.1.distance_to(&end) > f32::EPSILON;

    let flat = (start.x == end.x) != (start.y == end.y);
    let start_is_low = if start.x == end.x {
        start.y < end.y
    } else {
        start.x < end.x
    };

    // `None` means: move the whole segment.
    let drag_start = match target {
        Some(SegmentResizeTarget::MiddleX | SegmentResizeTarget::MiddleY) => None,
        Some(SegmentResizeTarget::Low) => Some(start_is_low),
        Some(SegmentResizeTarget::High) => Some(!start_is_low),
        None if flat => (start_moved != end_moved).then_some(start_moved),
        None if start_moved || end_moved => {
            Some(new_segment.0.distance_to(&start) >= new_segment.1.distance_to(&end))
        }
        None => return segment,
    };

    let Some(drag_start) = drag_start else {
        let dir = end - start;
        let norm = dir.norm();
        if norm <= f32::EPSILON {
            return segment;
        }
        let u = dir / norm;
        // The handle whose axis is more perpendicular to the segment moves it along its
        // direction, the other one moves it perpendicular to it.
        let handle_is_y = target == Some(SegmentResizeTarget::MiddleY);
        let along = handle_is_y == (dir.x.abs() >= dir.y.abs());
        let axis = if along { u } else { Vec2D::new(-u.y, u.x) };
        let shift = axis * (delta.x * axis.x + delta.y * axis.y);
        return (start + shift, end + shift);
    };

    let (fixed, dragged) = if drag_start {
        (end, start)
    } else {
        (start, end)
    };
    let v = dragged + delta - fixed;
    if v.is_zero() {
        return segment;
    }
    let moved = fixed + v.snapped_vector_15deg();
    if drag_start {
        (moved, end)
    } else {
        (start, moved)
    }
}

// The part of a crop rectangle that is actually inside the image: what a save
// writes out, and therefore what the dimension readout has to be derived from.
pub fn crop_rect_in_bounds(rect: (Vec2D, Vec2D), bounds: (Vec2D, Vec2D)) -> (Vec2D, Vec2D) {
    let (pos, size) = rect_ensure_in_bounds(rect_round(rect), bounds);
    (pos, size.max(Vec2D::zero()))
}

pub fn rect_round(rect: (Vec2D, Vec2D)) -> (Vec2D, Vec2D) {
    let (pos, size) = rect;
    (pos.round(), size.round())
}

#[cfg(test)]
mod tests {
    use super::{Vec2D, crop_rect_in_bounds, get_closest_aspect_ratio, rect_ensure_in_bounds};

    #[test]
    fn closest_aspect_ratio_supports_reverse_orientation() {
        assert_eq!(
            get_closest_aspect_ratio(9.0 / 16.0, &[(16.0, 9.0)]),
            (9.0, 16.0)
        );
    }

    fn bounds() -> (Vec2D, Vec2D) {
        (Vec2D::zero(), Vec2D::new(200.0, 100.0))
    }

    #[test]
    fn rect_ensure_in_bounds_clips_both_axes_when_overlapping_top_left() {
        let rect = (Vec2D::new(-40.0, -30.0), Vec2D::new(100.0, 80.0));
        assert_eq!(
            rect_ensure_in_bounds(rect, bounds()),
            (Vec2D::zero(), Vec2D::new(60.0, 50.0))
        );
    }

    #[test]
    fn rect_ensure_in_bounds_removes_width_when_rect_is_left_of_bounds() {
        let rect = (Vec2D::new(-300.0, 10.0), Vec2D::new(100.0, 20.0));
        let (_, size) = rect_ensure_in_bounds(rect, bounds());
        assert_eq!(size.x, 0.0);
    }

    #[test]
    fn rect_ensure_in_bounds_removes_height_when_rect_is_above_bounds() {
        let rect = (Vec2D::new(10.0, -300.0), Vec2D::new(20.0, 100.0));
        let (_, size) = rect_ensure_in_bounds(rect, bounds());
        assert_eq!(size.y, 0.0);
    }

    #[test]
    fn rect_ensure_in_bounds_clips_rect_straddling_all_four_edges() {
        let rect = (Vec2D::new(-50.0, -50.0), Vec2D::new(400.0, 300.0));
        assert_eq!(
            rect_ensure_in_bounds(rect, bounds()),
            (Vec2D::zero(), Vec2D::new(200.0, 100.0))
        );
    }

    #[test]
    fn rect_ensure_in_bounds_leaves_rect_inside_bounds_unchanged() {
        let rect = (Vec2D::new(10.0, 20.0), Vec2D::new(30.0, 40.0));
        assert_eq!(rect_ensure_in_bounds(rect, bounds()), rect);
    }

    #[test]
    fn crop_rect_in_bounds_reports_the_part_inside_the_image() {
        let rect = (Vec2D::new(150.0, 10.0), Vec2D::new(100.0, 20.0));
        assert_eq!(
            crop_rect_in_bounds(rect, bounds()),
            (Vec2D::new(150.0, 10.0), Vec2D::new(50.0, 20.0))
        );
    }

    #[test]
    fn crop_rect_in_bounds_rounds_before_clipping_like_the_render_target_does() {
        // Clipping first would leave 9.5 and read as 10, but the render target
        // is built from the rounded rect and comes out 9 wide.
        let rect = (Vec2D::new(190.5, 10.0), Vec2D::new(9.9, 20.0));
        assert_eq!(
            crop_rect_in_bounds(rect, bounds()),
            (Vec2D::new(191.0, 10.0), Vec2D::new(9.0, 20.0))
        );
    }

    #[test]
    fn crop_rect_in_bounds_reports_zero_for_a_crop_dragged_past_the_right_edge() {
        let rect = (Vec2D::new(250.0, 10.0), Vec2D::new(100.0, 20.0));
        let (_, size) = crop_rect_in_bounds(rect, bounds());
        assert_eq!(size.x, 0.0);
    }

    #[test]
    fn crop_rect_in_bounds_reports_zero_for_a_crop_dragged_past_the_bottom_edge() {
        let rect = (Vec2D::new(10.0, 150.0), Vec2D::new(20.0, 100.0));
        let (_, size) = crop_rect_in_bounds(rect, bounds());
        assert_eq!(size.y, 0.0);
    }
}
