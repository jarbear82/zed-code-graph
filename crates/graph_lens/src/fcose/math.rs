use std::ops::{Add, AddAssign, Mul, Neg, Sub, SubAssign};

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vector2 {
    pub x: f64,
    pub y: f64,
}

impl Vector2 {
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };

    #[inline]
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    #[inline]
    pub fn length_sq(self) -> f64 {
        self.x * self.x + self.y * self.y
    }

    #[inline]
    pub fn length(self) -> f64 {
        self.length_sq().sqrt()
    }

    #[inline]
    pub fn normalize(self) -> Self {
        let l = self.length();
        if l > 1e-10 {
            Self::new(self.x / l, self.y / l)
        } else {
            Self::ZERO
        }
    }

    /// Coordinate by axis index: 0 = x, 1 = y.
    #[inline]
    pub fn coord(self, axis: usize) -> f64 {
        if axis == 0 { self.x } else { self.y }
    }

    #[inline]
    pub fn set_coord(&mut self, axis: usize, v: f64) {
        if axis == 0 { self.x = v; } else { self.y = v; }
    }
}

impl Add for Vector2 {
    type Output = Self;
    fn add(self, r: Self) -> Self { Self::new(self.x + r.x, self.y + r.y) }
}
impl Sub for Vector2 {
    type Output = Self;
    fn sub(self, r: Self) -> Self { Self::new(self.x - r.x, self.y - r.y) }
}
impl Neg for Vector2 {
    type Output = Self;
    fn neg(self) -> Self { Self::new(-self.x, -self.y) }
}
impl Mul<f64> for Vector2 {
    type Output = Self;
    fn mul(self, s: f64) -> Self { Self::new(self.x * s, self.y * s) }
}
impl AddAssign for Vector2 {
    fn add_assign(&mut self, r: Self) { self.x += r.x; self.y += r.y; }
}
impl SubAssign for Vector2 {
    fn sub_assign(&mut self, r: Self) { self.x -= r.x; self.y -= r.y; }
}
