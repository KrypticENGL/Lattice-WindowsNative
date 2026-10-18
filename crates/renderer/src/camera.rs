//! Pan and zoom: one uniform-scale world-to-screen transform.

use crate::geom::{Affine, Rect, Vec2};

pub const MIN_ZOOM: f32 = 0.01;
pub const MAX_ZOOM: f32 = 8.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    zoom: f32,
    /// World point shown at the viewport centre.
    center: Vec2,
    viewport: Rect,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            center: Vec2::ZERO,
            viewport: Rect::default(),
        }
    }
}

impl Camera {
    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    pub fn viewport(&self) -> Rect {
        self.viewport
    }

    /// Keeps the same world point centred when the viewport changes.
    pub fn set_viewport(&mut self, viewport: Rect) {
        self.viewport = viewport;
    }

    pub fn transform(&self) -> Affine {
        Affine {
            scale: self.zoom,
            offset: self.viewport.center() - self.center * self.zoom,
        }
    }

    pub fn to_screen(&self, world: Vec2) -> Vec2 {
        self.transform().apply(world)
    }

    pub fn to_world(&self, screen: Vec2) -> Vec2 {
        (screen - self.viewport.center()) * (1.0 / self.zoom) + self.center
    }

    /// The world rect currently on screen.
    pub fn visible_world(&self) -> Rect {
        let tl = self.to_world(Vec2::new(self.viewport.x, self.viewport.y));
        Rect::new(
            tl.x,
            tl.y,
            self.viewport.w / self.zoom,
            self.viewport.h / self.zoom,
        )
    }

    /// Moves the view by a screen-space delta (content follows the mouse).
    pub fn pan(&mut self, screen_delta: Vec2) {
        self.center = self.center - screen_delta * (1.0 / self.zoom);
    }

    /// Multiplies the zoom by `factor`, keeping the world point under
    /// `screen` fixed.
    pub fn zoom_at(&mut self, screen: Vec2, factor: f32) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let anchor = self.to_world(screen);
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        // Solve to_world(screen) == anchor for the new centre.
        self.center = anchor - (screen - self.viewport.center()) * (1.0 / self.zoom);
    }

    /// Shows `world_center` at the viewport centre with zoom `zoom`.
    pub fn look_at(&mut self, world_center: Vec2, zoom: f32) {
        self.zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        self.center = world_center;
    }

    /// The camera `t` of the way from `a` to `b`. Zoom moves evenly in
    /// scale, so a big zoom change does not rush at one end.
    pub fn lerp(a: Self, b: Self, t: f32) -> Self {
        if t >= 1.0 {
            return b;
        }
        Self {
            zoom: a.zoom * (b.zoom / a.zoom).powf(t),
            center: a.center + (b.center - a.center) * t,
            viewport: b.viewport,
        }
    }

    /// Frames `bounds` with `margin` DIPs on each side, zooming in no
    /// further than `max_zoom`.
    pub fn fit(&mut self, bounds: Rect, margin: f32, max_zoom: f32) {
        let w = (self.viewport.w - 2.0 * margin).max(1.0);
        let h = (self.viewport.h - 2.0 * margin).max(1.0);
        let zx = w / bounds.w.max(1.0);
        let zy = h / bounds.h.max(1.0);
        self.zoom = zx.min(zy).clamp(MIN_ZOOM, max_zoom.min(MAX_ZOOM));
        self.center = bounds.center();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam() -> Camera {
        let mut c = Camera::default();
        c.set_viewport(Rect::new(100.0, 50.0, 800.0, 600.0));
        c
    }

    fn close(a: Vec2, b: Vec2) -> bool {
        (a - b).length() < 1e-3
    }

    #[test]
    fn round_trip() {
        let mut c = cam();
        c.zoom_at(Vec2::new(300.0, 200.0), 2.5);
        c.pan(Vec2::new(-40.0, 13.0));
        for p in [Vec2::new(0.0, 0.0), Vec2::new(-120.5, 77.0)] {
            assert!(close(c.to_world(c.to_screen(p)), p));
        }
    }

    #[test]
    fn zoom_keeps_point_under_cursor() {
        let mut c = cam();
        let cursor = Vec2::new(250.0, 420.0);
        let before = c.to_world(cursor);
        c.zoom_at(cursor, 3.0);
        assert!(close(c.to_world(cursor), before));
        c.zoom_at(cursor, 1e-9);
        assert_eq!(c.zoom(), MIN_ZOOM);
        assert!(close(c.to_world(cursor), before));
    }

    #[test]
    fn pan_follows_mouse() {
        let mut c = cam();
        let p = Vec2::new(10.0, 10.0);
        let s = c.to_screen(p);
        c.pan(Vec2::new(25.0, -5.0));
        assert!(close(c.to_screen(p), s + Vec2::new(25.0, -5.0)));
    }

    #[test]
    fn fit_contains_bounds() {
        let mut c = cam();
        let b = Rect::new(-500.0, -100.0, 2000.0, 300.0);
        c.fit(b, 20.0, 2.0);
        let v = c.visible_world();
        assert!(v.x <= b.x && v.right() >= b.right());
        assert!(v.y <= b.y && v.bottom() >= b.bottom());
    }
}
