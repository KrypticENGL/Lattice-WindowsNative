//! Draggable splitter dividing a rect into two panes.

use crate::event::{CursorShape, Event, MouseButton};
use crate::geom::{PointF, RectF};
use crate::panel::Cx;
use crate::sys::gfx::DrawCtx;
use crate::theme::Theme;

/// Divider thickness (visual) and the wider grab zone around it, in DIPs.
const DIVIDER: f32 = 1.0;
const GRAB: f32 = 4.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Panes side by side; the divider is vertical.
    Horizontal,
    /// Panes stacked; the divider is horizontal.
    Vertical,
}

/// Which pane keeps its size when the total changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    Start,
    End,
}

#[derive(Debug, Clone)]
pub struct Splitter {
    axis: Axis,
    anchor: Anchor,
    /// Size of the anchored pane along the axis.
    size: f32,
    min_anchored: f32,
    min_other: f32,
    bounds: RectF,
    divider: RectF,
    /// Pointer offset from the divider start while dragging.
    drag: Option<f32>,
    hover: bool,
}

/// Clamps the anchored pane size so both panes respect their minimums.
/// When the total is too small for both, the other pane wins.
pub fn clamp_size(size: f32, extent: f32, min_anchored: f32, min_other: f32) -> f32 {
    let available = (extent - DIVIDER).max(0.0);
    size.min(available - min_other)
        .max(min_anchored)
        .min(available)
        .max(0.0)
}

impl Splitter {
    pub fn new(axis: Axis, anchor: Anchor, size: f32, min_anchored: f32, min_other: f32) -> Self {
        Self {
            axis,
            anchor,
            size,
            min_anchored,
            min_other,
            bounds: RectF::default(),
            divider: RectF::default(),
            drag: None,
            hover: false,
        }
    }

    fn extent(&self) -> f32 {
        match self.axis {
            Axis::Horizontal => self.bounds.w,
            Axis::Vertical => self.bounds.h,
        }
    }

    fn along(&self, p: PointF) -> f32 {
        match self.axis {
            Axis::Horizontal => p.x - self.bounds.x,
            Axis::Vertical => p.y - self.bounds.y,
        }
    }

    /// Lays out the two panes inside `r`: `(start, end)`.
    pub fn layout(&mut self, r: RectF) -> (RectF, RectF) {
        self.bounds = r;
        let extent = self.extent();
        let anchored = clamp_size(self.size, extent, self.min_anchored, self.min_other);
        let available = (extent - DIVIDER).max(0.0);
        let start = match self.anchor {
            Anchor::Start => anchored,
            Anchor::End => available - anchored,
        };
        match self.axis {
            Axis::Horizontal => {
                let (a, rest) = r.split_left(start);
                let (div, b) = rest.split_left(DIVIDER);
                self.divider = div;
                (a, b)
            }
            Axis::Vertical => {
                let (a, rest) = r.split_top(start);
                let (div, b) = rest.split_top(DIVIDER);
                self.divider = div;
                (a, b)
            }
        }
    }

    fn grab_zone(&self) -> RectF {
        match self.axis {
            Axis::Horizontal => RectF::new(
                self.divider.x - GRAB,
                self.divider.y,
                self.divider.w + 2.0 * GRAB,
                self.divider.h,
            ),
            Axis::Vertical => RectF::new(
                self.divider.x,
                self.divider.y - GRAB,
                self.divider.w,
                self.divider.h + 2.0 * GRAB,
            ),
        }
    }

    fn cursor(&self) -> CursorShape {
        match self.axis {
            Axis::Horizontal => CursorShape::SizeWE,
            Axis::Vertical => CursorShape::SizeNS,
        }
    }

    /// Handles hover, drag start, drag and release. Returns `true` if the
    /// event belongs to the splitter.
    pub fn on_event(&mut self, e: &Event, cx: &mut Cx) -> bool {
        match *e {
            Event::MouseDown {
                pos,
                button: MouseButton::Left,
                ..
            } if self.grab_zone().contains(pos) => {
                let divider_start = self.along(PointF::new(self.divider.x, self.divider.y));
                self.drag = Some(self.along(pos) - divider_start);
                cx.cursor = self.cursor();
                true
            }
            Event::MouseMove { pos, .. } => {
                if let Some(grab) = self.drag {
                    let start = self.along(pos) - grab;
                    let available = (self.extent() - DIVIDER).max(0.0);
                    self.size = match self.anchor {
                        Anchor::Start => start,
                        Anchor::End => available - start,
                    };
                    self.size =
                        clamp_size(self.size, self.extent(), self.min_anchored, self.min_other);
                    cx.cursor = self.cursor();
                    cx.repaint();
                    return true;
                }
                let hover = self.grab_zone().contains(pos);
                if hover != self.hover {
                    self.hover = hover;
                    cx.repaint();
                }
                if hover {
                    cx.cursor = self.cursor();
                }
                hover
            }
            Event::MouseUp {
                button: MouseButton::Left,
                ..
            } if self.drag.is_some() => {
                self.drag = None;
                cx.repaint();
                true
            }
            Event::MouseLeave => {
                if self.hover {
                    self.hover = false;
                    cx.repaint();
                }
                false
            }
            _ => false,
        }
    }

    pub fn paint(&self, d: &mut DrawCtx, theme: &Theme) {
        let color = if self.hover || self.drag.is_some() {
            theme.accent
        } else {
            theme.border
        };
        let r = if self.hover || self.drag.is_some() {
            match self.axis {
                Axis::Horizontal => {
                    RectF::new(self.divider.x - 1.0, self.divider.y, 3.0, self.divider.h)
                }
                Axis::Vertical => {
                    RectF::new(self.divider.x, self.divider.y - 1.0, self.divider.w, 3.0)
                }
            }
        } else {
            self.divider
        };
        d.fill_rect(r, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_respects_both_minimums() {
        assert_eq!(clamp_size(50.0, 501.0, 100.0, 200.0), 100.0);
        assert_eq!(clamp_size(450.0, 501.0, 100.0, 200.0), 300.0);
        assert_eq!(clamp_size(250.0, 501.0, 100.0, 200.0), 250.0);
    }

    #[test]
    fn clamp_when_too_small_keeps_within_extent() {
        let s = clamp_size(250.0, 101.0, 100.0, 200.0);
        assert!((0.0..=100.0).contains(&s));
    }

    #[test]
    fn end_anchor_keeps_end_pane_size_on_resize() {
        let mut s = Splitter::new(Axis::Vertical, Anchor::End, 200.0, 50.0, 50.0);
        let (_, b) = s.layout(RectF::new(0.0, 0.0, 100.0, 601.0));
        assert_eq!(b.h, 200.0);
        let (a, b) = s.layout(RectF::new(0.0, 0.0, 100.0, 801.0));
        assert_eq!(b.h, 200.0);
        assert_eq!(a.h, 600.0);
    }

    #[test]
    fn panes_and_divider_fill_bounds() {
        let mut s = Splitter::new(Axis::Horizontal, Anchor::Start, 250.0, 100.0, 100.0);
        let (a, b) = s.layout(RectF::new(10.0, 0.0, 800.0, 100.0));
        assert_eq!(a.x, 10.0);
        assert_eq!(a.w, 250.0);
        assert_eq!(b.x, 10.0 + 250.0 + DIVIDER);
        assert_eq!(a.w + DIVIDER + b.w, 800.0);
    }
}
