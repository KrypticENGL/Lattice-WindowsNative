//! The drawing surface the renderer targets, and its colour style.
//!
//! The UI crate implements [`Canvas`] on top of its single Direct2D device
//! context, so this crate stays free of Win32 types and unsafe code. Calls
//! are batched: one call draws every segment (or disc) of one style.

use crate::geom::{Affine, Rect, Vec2};
use crate::model::{EdgeClass, NodeClass};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub fn with_alpha(self, a: f32) -> Self {
        Self { a, ..self }
    }

    /// `t` of the way from `self` to `o`.
    pub fn mix(self, o: Self, t: f32) -> Self {
        let m = |a: f32, b: f32| a + (b - a) * t;
        Self {
            r: m(self.r, o.r),
            g: m(self.g, o.g),
            b: m(self.b, o.b),
            a: m(self.a, o.a),
        }
    }

    /// Scales the alpha (for dimmed elements).
    pub fn faded(self, k: f32) -> Self {
        Self {
            a: self.a * k,
            ..self
        }
    }
}

/// Colours for the graph; filled from the UI theme.
#[derive(Debug, Clone, PartialEq)]
pub struct Style {
    pub node: [Rgba; NodeClass::COUNT],
    pub edge: [Rgba; EdgeClass::COUNT],
    /// Selection ring and edges of the selected node.
    pub highlight: Rgba,
    pub hover: Rgba,
    pub label: Rgba,
    pub label_dim: Rgba,
    pub card_bg: Rgba,
    pub card_border: Rgba,
    /// Alpha multiplier for elements unrelated to the selection.
    pub dim: f32,
}

pub trait Canvas {
    /// World-to-screen transform for the geometric calls that follow.
    fn set_transform(&mut self, t: Affine);
    fn reset_transform(&mut self);
    /// Antialiasing on (default) or off (cheaper, for dot-level detail).
    fn set_antialias(&mut self, on: bool);
    /// Strokes every `[from, to]` segment with one colour and width.
    fn stroke_segments(&mut self, segments: &[[Vec2; 2]], color: Rgba, width: f32);
    fn fill_discs(&mut self, centers: &[Vec2], radius: f32, color: Rgba);
    fn fill_squares(&mut self, centers: &[Vec2], half: f32, color: Rgba);
    fn stroke_circle(&mut self, center: Vec2, radius: f32, color: Rgba, width: f32);
    fn fill_rect(&mut self, r: Rect, radius: f32, color: Rgba);
    fn stroke_rect(&mut self, r: Rect, radius: f32, color: Rgba, width: f32);
    /// One line of UI text with its top-left at `origin`.
    fn text(&mut self, s: &str, origin: Vec2, color: Rgba, bold: bool);
    fn measure_text(&self, s: &str, bold: bool) -> f32;
    fn line_height(&self) -> f32;
}
