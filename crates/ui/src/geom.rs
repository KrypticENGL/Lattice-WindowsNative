//! Geometry in device-independent pixels (DIPs).

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PointF {
    pub x: f32,
    pub y: f32,
}

impl PointF {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SizeF {
    pub w: f32,
    pub h: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectF {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn contains(&self, p: PointF) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }

    /// Shrinks the rect by `d` on every side, never below zero size.
    pub fn inset(&self, d: f32) -> Self {
        Self::new(
            self.x + d,
            self.y + d,
            (self.w - 2.0 * d).max(0.0),
            (self.h - 2.0 * d).max(0.0),
        )
    }

    /// Splits off the top `h` DIPs: `(top, rest)`.
    pub fn split_top(&self, h: f32) -> (Self, Self) {
        let h = h.clamp(0.0, self.h.max(0.0));
        (
            Self::new(self.x, self.y, self.w, h),
            Self::new(self.x, self.y + h, self.w, self.h - h),
        )
    }

    /// Splits off the left `w` DIPs: `(left, rest)`.
    pub fn split_left(&self, w: f32) -> (Self, Self) {
        let w = w.clamp(0.0, self.w.max(0.0));
        (
            Self::new(self.x, self.y, w, self.h),
            Self::new(self.x + w, self.y, self.w - w, self.h),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_is_half_open() {
        let r = RectF::new(0.0, 0.0, 10.0, 10.0);
        assert!(r.contains(PointF::new(0.0, 0.0)));
        assert!(!r.contains(PointF::new(10.0, 5.0)));
    }

    #[test]
    fn splits_clamp_to_bounds() {
        let r = RectF::new(0.0, 0.0, 100.0, 50.0);
        let (top, rest) = r.split_top(80.0);
        assert_eq!(top.h, 50.0);
        assert_eq!(rest.h, 0.0);
        let (left, rest) = r.split_left(30.0);
        assert_eq!(left.w, 30.0);
        assert_eq!(rest.x, 30.0);
        assert_eq!(rest.w, 70.0);
    }
}
