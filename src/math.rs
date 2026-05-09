use std::{
    f32::consts::PI,
    fmt::Display,
    ops::{Add, AddAssign, Mul, Sub, SubAssign},
};

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

        let (a, b) = if new_angle.abs() < PI / 4.0
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

impl Display for Vec2D {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "({},{})", self.x, self.y)
    }
}

#[derive(Default, Debug, Copy, Clone, PartialEq)]
pub struct Rect {
    /// Top-left corner in canonical form (size always non-negative).
    pub pos: Vec2D,
    pub size: Vec2D,
}

impl Rect {
    pub fn new(pos: Vec2D, size: Vec2D) -> Self {
        let (pos, size) = rect_ensure_positive_size(pos, size);
        Self { pos, size }
    }

    pub fn from_corners(a: Vec2D, b: Vec2D) -> Self {
        let pos = Vec2D::new(a.x.min(b.x), a.y.min(b.y));
        let size = Vec2D::new((a.x - b.x).abs(), (a.y - b.y).abs());
        Self { pos, size }
    }

    pub fn from_tuple((pos, size): (Vec2D, Vec2D)) -> Self {
        Self::new(pos, size)
    }

    pub fn top_left(&self) -> Vec2D {
        self.pos
    }

    pub fn top_right(&self) -> Vec2D {
        Vec2D::new(self.pos.x + self.size.x, self.pos.y)
    }

    pub fn bottom_left(&self) -> Vec2D {
        Vec2D::new(self.pos.x, self.pos.y + self.size.y)
    }

    pub fn bottom_right(&self) -> Vec2D {
        self.pos + self.size
    }

    pub fn center(&self) -> Vec2D {
        self.pos + self.size * 0.5
    }

    pub fn contains(&self, p: Vec2D) -> bool {
        p.x >= self.pos.x
            && p.x <= self.pos.x + self.size.x
            && p.y >= self.pos.y
            && p.y <= self.pos.y + self.size.y
    }

    /// Expand the rectangle outward in all directions by `padding`.
    /// Useful for hit-test tolerance on thin strokes.
    pub fn inflated(&self, padding: f32) -> Rect {
        Rect {
            pos: Vec2D::new(self.pos.x - padding, self.pos.y - padding),
            size: Vec2D::new(self.size.x + 2.0 * padding, self.size.y + 2.0 * padding),
        }
    }

    pub fn translated(&self, delta: Vec2D) -> Rect {
        Rect {
            pos: self.pos + delta,
            size: self.size,
        }
    }

    pub fn union(&self, other: Rect) -> Rect {
        let pos = Vec2D::new(self.pos.x.min(other.pos.x), self.pos.y.min(other.pos.y));
        let br = Vec2D::new(
            (self.pos.x + self.size.x).max(other.pos.x + other.size.x),
            (self.pos.y + self.size.y).max(other.pos.y + other.size.y),
        );
        Rect {
            pos,
            size: br - pos,
        }
    }

    /// True if `self` overlaps `other` (either touches or shares any area).
    pub fn intersects(&self, other: Rect) -> bool {
        let a_br = self.bottom_right();
        let b_br = other.bottom_right();
        self.pos.x < b_br.x
            && other.pos.x < a_br.x
            && self.pos.y < b_br.y
            && other.pos.y < a_br.y
    }
}

/// Shortest distance from point `p` to the segment from `a` to `b`.
/// Used for hit-testing thin shapes (lines, arrow shafts).
pub fn point_to_segment_distance(p: Vec2D, a: Vec2D, b: Vec2D) -> f32 {
    let ab = b - a;
    let len2 = ab.norm2();
    if len2 < f32::EPSILON {
        return p.distance_to(&a);
    }
    let ap = p - a;
    let t = ((ap.x * ab.x + ap.y * ab.y) / len2).clamp(0.0, 1.0);
    let proj = Vec2D::new(a.x + ab.x * t, a.y + ab.y * t);
    p.distance_to(&proj)
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

    if pos.x < bounds.0.x {
        pos.x = bounds.0.x;
        size.x -= bounds.0.x - pos.x;
    }

    if pos.y < bounds.0.y {
        pos.y = bounds.0.y;
        size.y -= bounds.0.y - pos.y;
    }

    if pos.x + size.x > bounds.1.x {
        size.x = bounds.1.x - pos.x;
    }

    if pos.y + size.y > bounds.1.y {
        size.y = bounds.1.y - pos.y;
    }

    (pos, size)
}

pub fn rect_round(rect: (Vec2D, Vec2D)) -> (Vec2D, Vec2D) {
    let (mut pos, mut size) = rect;

    pos.x = pos.x.round();
    pos.y = pos.y.round();
    size.x = size.x.round();
    size.y = size.y.round();

    (pos, size)
}
