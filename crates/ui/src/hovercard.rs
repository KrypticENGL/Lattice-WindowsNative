//! The "View in memory" card that appears under a hovered variable (Data
//! view slot, call graph field or variable, identifier in the source). It
//! stays while the pointer is over the variable, the card or the gap
//! between them, so it can be reached and clicked. When memory cannot be
//! shown yet (no run, or not live at this stop) it says why instead.

use crate::event::{CursorShape, Event, MouseButton};
use crate::geom::{PointF, RectF};
use crate::memory::{self, MemTarget};
use crate::panel::{Action, Cx};
use crate::sys::gfx::DrawCtx;
use crate::sys::text::{TextStyle, TextSystem};
use crate::theme::Theme;

const GAP: f32 = 4.0;
const PAD_X: f32 = 10.0;
const H: f32 = 26.0;

#[derive(Debug, Clone, PartialEq)]
pub struct HoverCard {
    pub target: MemTarget,
    anchor: RectF,
    card: RectF,
    label: String,
    /// Clicking opens the Memory panel.
    ready: bool,
    hot: bool,
}

impl HoverCard {
    /// A card for `target` under `anchor`, kept inside `bounds`.
    pub fn new(
        target: MemTarget,
        anchor: RectF,
        bounds: RectF,
        snap: Option<&debugger::Snapshot>,
        text: &TextSystem,
    ) -> Self {
        let (label, ready) = match memory::card_label(snap, &target) {
            Ok(l) => (format!("\u{25A6}  {l}"), true),
            Err(why) => (why, false),
        };
        let w = text.measure(TextStyle::UiSmall, &label) + 2.0 * PAD_X;
        let x = anchor.x.min(bounds.right() - w).max(bounds.x);
        let below = anchor.bottom() + GAP;
        let y = if below + H <= bounds.bottom() {
            below
        } else {
            (anchor.y - GAP - H).max(bounds.y)
        };
        Self {
            target,
            anchor,
            card: RectF::new(x, y, w, H),
            label,
            ready,
            hot: false,
        }
    }

    /// The pointer is over the anchor, the card or the gap between.
    fn keeps(&self, p: PointF) -> bool {
        let (a, c) = (self.anchor, self.card);
        let x = a.x.min(c.x);
        let span = RectF::new(x, a.y.min(c.y), a.right().max(c.right()) - x, 0.0);
        let bridge = RectF::new(
            span.x,
            a.bottom().min(c.bottom()),
            span.w,
            (a.y.max(c.y) - a.bottom().min(c.bottom())).max(0.0),
        );
        a.contains(p) || c.contains(p) || bridge.contains(p)
    }

    pub fn paint(&self, d: &mut DrawCtx, theme: &Theme) {
        let c = self.card;
        d.fill_round_rect(c, 6.0, theme.graph_card_bg);
        d.stroke_rect(c, if self.hot { theme.accent } else { theme.border }, 1.0);
        let color = match (self.ready, self.hot) {
            (true, true) => theme.text_bright,
            (true, false) => theme.accent,
            (false, _) => theme.text_dim,
        };
        let at = RectF::new(c.x + PAD_X, c.y, c.w - PAD_X, c.h);
        d.text(TextStyle::UiSmall, &self.label, at, color);
    }
}

/// Mouse handling shared by every host. `find` gives the card for what is
/// under the pointer. Returns `true` when the event was the card's (a move
/// over it, or a click on it) and the host should not handle it.
pub fn track(
    card: &mut Option<HoverCard>,
    e: &Event,
    cx: &mut Cx,
    find: impl FnOnce(PointF) -> Option<HoverCard>,
) -> bool {
    match *e {
        Event::MouseMove {
            pos,
            left_down: false,
            ..
        } => {
            if let Some(c) = card.as_mut().filter(|c| c.keeps(pos)) {
                let hot = c.card.contains(pos);
                if hot != c.hot {
                    c.hot = hot;
                    cx.repaint();
                }
                if hot {
                    cx.cursor = if c.ready {
                        CursorShape::Hand
                    } else {
                        CursorShape::Arrow
                    };
                    return true;
                }
                return false;
            }
            let next = find(pos);
            if next != *card {
                *card = next;
                cx.repaint();
            }
            false
        }
        Event::MouseDown {
            pos,
            button: MouseButton::Left,
            ..
        } => {
            let Some(c) = card.as_ref().filter(|c| c.card.contains(pos)) else {
                if card.take().is_some() {
                    cx.repaint();
                }
                return false;
            };
            if c.ready {
                cx.act(Action::ShowMemory(c.target.clone()));
                *card = None;
                cx.repaint();
            }
            true
        }
        Event::MouseMove { .. } | Event::MouseLeave | Event::Wheel { .. } => {
            if card.take().is_some() {
                cx.repaint();
            }
            false
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(anchor: RectF, card: RectF) -> HoverCard {
        HoverCard {
            target: MemTarget::Global { name: "g".into() },
            anchor,
            card,
            label: String::new(),
            ready: true,
            hot: false,
        }
    }

    #[test]
    fn stays_across_the_gap() {
        let c = card(
            RectF::new(10.0, 10.0, 40.0, 16.0),
            RectF::new(10.0, 30.0, 120.0, 26.0),
        );
        assert!(c.keeps(PointF::new(20.0, 20.0)), "anchor");
        assert!(c.keeps(PointF::new(20.0, 28.0)), "gap");
        assert!(c.keeps(PointF::new(100.0, 40.0)), "card");
        assert!(!c.keeps(PointF::new(100.0, 15.0)), "beside the anchor");
        assert!(!c.keeps(PointF::new(20.0, 70.0)), "below");
        // Placed above when there is no room below.
        let above = card(
            RectF::new(10.0, 80.0, 40.0, 16.0),
            RectF::new(10.0, 50.0, 120.0, 26.0),
        );
        assert!(above.keeps(PointF::new(20.0, 78.0)));
    }
}
