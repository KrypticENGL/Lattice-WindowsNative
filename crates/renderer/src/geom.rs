//! Plain 2-D geometry: points in world units or DIPs, axis-aligned rects,
//! and the uniform-scale transform the camera produces.

use std::ops::{Add, Mul, Sub};

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };

    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub fn length_sq(self) -> f32 {
        self.x * self.x + self.y * self.y
    }

    pub fn length(self) -> f32 {
        self.length_sq().sqrt()
    }

    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

impl Add for Vec2 {
    type Output = Self;
    fn add(self, o: Self) -> Self {
        Self::new(self.x + o.x, self.y + o.y)
    }
}

impl Sub for Vec2 {
    type Output = Self;
    fn sub(self, o: Self) -> Self {
        Self::new(self.x - o.x, self.y - o.y)
    }
}

impl Mul<f32> for Vec2 {
    type Output = Self;
    fn mul(self, k: f32) -> Self {
        Self::new(self.x * k, self.y * k)
    }
}

/// Axis-aligned rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn center(&self) -> Vec2 {
        Vec2::new(self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    pub fn contains(&self, p: Vec2) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }

    pub fn inflate(&self, d: f32) -> Self {
        Self::new(self.x - d, self.y - d, self.w + 2.0 * d, self.h + 2.0 * d)
    }

    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }

    /// Whether the segment's bounding box overlaps this rect (a cheap,
    /// conservative visibility test).
    pub fn overlaps_segment(&self, a: Vec2, b: Vec2) -> bool {
        a.x.max(b.x) >= self.x
            && a.x.min(b.x) <= self.right()
            && a.y.max(b.y) >= self.y
            && a.y.min(b.y) <= self.bottom()
    }

    /// Smallest rect holding every point, or `None` for no (finite) points.
    pub fn bounding(points: &[Vec2]) -> Option<Self> {
        let mut it = points.iter().filter(|p| p.is_finite());
        let first = it.next()?;
        let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.x, first.y);
        for p in it {
            x0 = x0.min(p.x);
            y0 = y0.min(p.y);
            x1 = x1.max(p.x);
            y1 = y1.max(p.y);
        }
        Some(Self::new(x0, y0, x1 - x0, y1 - y0))
    }
}

/// `screen = world * scale + offset`, in DIPs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine {
    pub scale: f32,
    pub offset: Vec2,
}

impl Affine {
    pub const IDENTITY: Self = Self {
        scale: 1.0,
        offset: Vec2::ZERO,
    };

    pub fn apply(&self, p: Vec2) -> Vec2 {
        p * self.scale + self.offset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounding_skips_non_finite() {
        let pts = [
            Vec2::new(1.0, 2.0),
            Vec2::new(f32::NAN, 0.0),
            Vec2::new(-3.0, 5.0),
        ];
        assert_eq!(Rect::bounding(&pts), Some(Rect::new(-3.0, 2.0, 4.0, 3.0)));
        assert_eq!(Rect::bounding(&[]), None);
    }

    #[test]
    fn segment_overlap_is_bbox_based() {
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert!(r.overlaps_segment(Vec2::new(-5.0, 5.0), Vec2::new(15.0, 5.0)));
        assert!(!r.overlaps_segment(Vec2::new(-5.0, -5.0), Vec2::new(-1.0, 20.0)));
    }
}
