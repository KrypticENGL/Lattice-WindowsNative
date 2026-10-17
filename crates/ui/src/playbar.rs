//! The replay's play bar, under the Data canvas, laid out like a media
//! player's: previous / play-pause / next, the current step, a seek track,
//! the step count, then speed and Record again.

use crate::event::{CursorShape, Event, MouseButton};
use crate::geom::{PointF, RectF};
use crate::panel::{Action, Cx};
use crate::replay::{ReplayAction, ReplayStatus};
use crate::sys::gfx::DrawCtx;
use crate::sys::text::{TextStyle, TextSystem};
use crate::theme::{Color, Theme};
use crate::visualizer::rgba;

pub const PLAYBAR_H: f32 = 46.0;
const ROUND: f32 = 30.0;
const SMALL: f32 = 26.0;
const TRACK_H: f32 = 4.0;
const KNOB_R: f32 = 7.0;
const RECORD_LABEL: &str = "Record again";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    Prev,
    Toggle,
    Next,
    Track,
    Speed,
    Record,
}

#[derive(Default)]
pub struct PlayBar {
    rect: RectF,
    items: Vec<(Item, RectF)>,
    /// The track line itself (the `Track` item is its taller hit area).
    track: RectF,
    current: RectF,
    total: RectF,
    hover: Option<Item>,
    /// Dragging the knob: the bar owns the mouse until release.
    seeking: bool,
}

/// What the bar shows: a replay, or a recording in progress.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BarState {
    Recording(usize),
    Replay(ReplayStatus),
}

impl PlayBar {
    pub fn layout(&mut self, rect: RectF, state: BarState, text: &TextSystem) {
        self.rect = rect;
        self.items.clear();
        let cy = rect.y + rect.h / 2.0;
        let square = |x: f32, s: f32| RectF::new(x, cy - s / 2.0, s, s);
        let mut x = rect.x + 12.0;
        for (item, s) in [
            (Item::Prev, SMALL),
            (Item::Toggle, ROUND),
            (Item::Next, SMALL),
        ] {
            self.items.push((item, square(x, s)));
            x += s + 6.0;
        }
        x += 8.0;
        // Both counts get the width of the larger one, so the track keeps
        // its place while the current step's digits change.
        let len = match state {
            BarState::Replay(s) => s.len,
            BarState::Recording(n) => n,
        };
        let digits = text.measure(TextStyle::Ui, &len.max(10).to_string());
        self.current = RectF::new(x, rect.y, digits, rect.h);
        x += digits + 12.0;

        let mut right = rect.right() - 12.0;
        let record_w = text.measure(TextStyle::Ui, RECORD_LABEL) + 24.0;
        right -= record_w;
        let record = RectF::new(right, cy - SMALL / 2.0, record_w, SMALL);
        right -= 6.0;
        let speed_w = text.measure(TextStyle::Ui, "0.5\u{d7}") + 20.0;
        right -= speed_w;
        let speed = RectF::new(right, cy - SMALL / 2.0, speed_w, SMALL);
        right -= 12.0 + digits;
        self.total = RectF::new(right, rect.y, digits, rect.h);
        right -= 12.0;

        let w = (right - x).max(0.0);
        self.track = RectF::new(x, cy - TRACK_H / 2.0, w, TRACK_H);
        self.items.push((
            Item::Track,
            RectF::new(x - KNOB_R, rect.y + 6.0, w + 2.0 * KNOB_R, rect.h - 12.0),
        ));
        self.items.push((Item::Speed, speed));
        self.items.push((Item::Record, record));
    }

    pub fn paint(&self, d: &mut DrawCtx, theme: &Theme, state: BarState) {
        d.fill_rect(self.rect, theme.panel_bg);
        d.fill_rect(
            RectF::new(self.rect.x, self.rect.y, self.rect.w, 1.0),
            theme.border,
        );
        let replay = match state {
            BarState::Replay(s) => Some(s),
            BarState::Recording(_) => None,
        };
        let enabled = replay.is_some();
        for &(item, r) in &self.items {
            let hover = enabled && self.hover == Some(item);
            match item {
                Item::Toggle => {
                    let fill = match (enabled, hover) {
                        (false, _) => theme.border,
                        (true, true) => theme.accent.with_alpha(0.85),
                        (true, false) => theme.button_bg,
                    };
                    d.fill_round_rect(r, r.h / 2.0, fill);
                    let ink = if enabled {
                        theme.button_text
                    } else {
                        theme.text_dim
                    };
                    let c = PointF::new(r.x + r.w / 2.0, r.y + r.h / 2.0);
                    if replay.is_some_and(|s| s.playing) {
                        pause_icon(d, c, ink);
                    } else {
                        play_icon(d, PointF::new(c.x + 1.5, c.y), 6.5, ink);
                    }
                }
                Item::Prev | Item::Next => {
                    if hover {
                        d.fill_round_rect(r, 5.0, theme.hover_bg);
                    }
                    let ink = icon_ink(theme, enabled, hover);
                    skip_icon(d, r, item == Item::Next, ink);
                }
                Item::Speed | Item::Record => {
                    let label = match (item, replay) {
                        (Item::Speed, Some(s)) => speed_label(s.speed),
                        (Item::Speed, None) => "1\u{d7}".to_string(),
                        _ => RECORD_LABEL.to_string(),
                    };
                    if hover {
                        d.fill_round_rect(r, 5.0, theme.hover_bg);
                    }
                    renderer::Canvas::stroke_rect(
                        d,
                        renderer::Rect::new(r.x, r.y, r.w, r.h),
                        5.0,
                        rgba(theme.border),
                        1.0,
                    );
                    let w = d.text_system().measure(TextStyle::Ui, &label);
                    d.text(
                        TextStyle::Ui,
                        &label,
                        RectF::new(r.x + (r.w - w) / 2.0, r.y, w + 2.0, r.h),
                        icon_ink(theme, enabled, hover),
                    );
                }
                Item::Track => {}
            }
        }

        let t = self.track;
        d.fill_round_rect(t, TRACK_H / 2.0, theme.border);
        match state {
            BarState::Recording(n) => {
                let label = format!("Recording\u{2026} {n} steps");
                let w = d.text_system().measure(TextStyle::UiSmall, &label);
                let r = RectF::new(t.x + (t.w - w) / 2.0 - 10.0, t.y - 11.0, w + 20.0, 22.0);
                d.fill_round_rect(r, 11.0, theme.panel_bg);
                d.text(
                    TextStyle::UiSmall,
                    &label,
                    RectF::new(r.x + 10.0, r.y, w + 2.0, r.h),
                    theme.text_dim,
                );
                self.count(d, self.current, &n.to_string(), theme.text_dim, true);
                self.count(d, self.total, "?", theme.text_dim, false);
            }
            BarState::Replay(s) => {
                let x = self.knob_x(s);
                let done = RectF::new(t.x, t.y, x - t.x, t.h);
                d.fill_round_rect(done, TRACK_H / 2.0, theme.accent);
                let knob = if self.seeking || self.hover == Some(Item::Track) {
                    KNOB_R + 1.0
                } else {
                    KNOB_R
                };
                let c = renderer::Vec2::new(x, t.y + t.h / 2.0);
                renderer::Canvas::fill_discs(d, &[c], knob, rgba(theme.accent));
                self.count(d, self.current, &(s.pos + 1).to_string(), theme.text, true);
                self.count(d, self.total, &s.len.to_string(), theme.text_dim, false);
            }
        }
    }

    /// The current step (right-aligned, next to the track) or the total
    /// (left-aligned).
    fn count(&self, d: &mut DrawCtx, r: RectF, s: &str, c: Color, right_align: bool) {
        let w = d.text_system().measure(TextStyle::Ui, s);
        let x = if right_align { r.right() - w } else { r.x };
        d.text(TextStyle::Ui, s, RectF::new(x, r.y, w + 2.0, r.h), c);
    }

    fn knob_x(&self, s: ReplayStatus) -> f32 {
        let frac = if s.len > 1 {
            s.pos as f32 / (s.len - 1) as f32
        } else {
            1.0
        };
        self.track.x + frac * self.track.w
    }

    /// The step under `x` on the track.
    fn step_at(&self, x: f32, s: ReplayStatus) -> usize {
        if s.len <= 1 || self.track.w <= 0.0 {
            return 0;
        }
        let frac = ((x - self.track.x) / self.track.w).clamp(0.0, 1.0);
        (frac * (s.len - 1) as f32).round() as usize
    }

    fn item_at(&self, p: PointF) -> Option<Item> {
        self.items
            .iter()
            .find(|(_, r)| r.contains(p))
            .map(|&(i, _)| i)
    }

    pub fn contains(&self, p: PointF) -> bool {
        self.rect.contains(p)
    }

    /// Drops hover (the mouse left the bar).
    pub fn leave(&mut self) -> bool {
        self.hover.take().is_some()
    }

    /// Mouse input over the bar (or anywhere while seeking). Returns
    /// `true` if it was the bar's.
    pub fn on_event(&mut self, e: &Event, state: BarState, cx: &mut Cx) -> bool {
        let replay = match state {
            BarState::Replay(s) => Some(s),
            BarState::Recording(_) => None,
        };
        match *e {
            Event::MouseDown {
                pos,
                button: MouseButton::Left,
                ..
            } if self.contains(pos) => {
                let (Some(s), Some(item)) = (replay, self.item_at(pos)) else {
                    return true;
                };
                let a = match item {
                    Item::Prev => ReplayAction::Step(-1),
                    Item::Toggle => ReplayAction::Toggle,
                    Item::Next => ReplayAction::Step(1),
                    Item::Speed => ReplayAction::Speed,
                    Item::Record => ReplayAction::Record,
                    Item::Track => {
                        self.seeking = true;
                        ReplayAction::Seek(self.step_at(pos.x, s))
                    }
                };
                cx.act(Action::Replay(a));
                cx.repaint();
                true
            }
            Event::MouseMove { pos, .. } if self.seeking => {
                if let Some(s) = replay {
                    let i = self.step_at(pos.x, s);
                    if i != s.pos {
                        cx.act(Action::Replay(ReplayAction::Seek(i)));
                    }
                }
                cx.cursor = CursorShape::Hand;
                true
            }
            Event::MouseUp {
                button: MouseButton::Left,
                ..
            } if self.seeking => {
                self.seeking = false;
                cx.repaint();
                true
            }
            Event::MouseMove { pos, .. } if self.contains(pos) => {
                let hover = replay.and(self.item_at(pos));
                if hover != self.hover {
                    self.hover = hover;
                    cx.repaint();
                }
                if hover.is_some() {
                    cx.cursor = CursorShape::Hand;
                }
                true
            }
            _ => false,
        }
    }
}

fn icon_ink(theme: &Theme, enabled: bool, hover: bool) -> Color {
    match (enabled, hover) {
        (false, _) => theme.text_dim.with_alpha(0.45),
        (true, true) => theme.text,
        (true, false) => theme.text_dim,
    }
}

/// A right-pointing triangle centred near `c`.
fn play_icon(d: &mut DrawCtx, c: PointF, r: f32, ink: Color) {
    d.fill_triangle(
        PointF::new(c.x - r * 0.75, c.y - r),
        PointF::new(c.x + r, c.y),
        PointF::new(c.x - r * 0.75, c.y + r),
        ink,
    );
}

fn pause_icon(d: &mut DrawCtx, c: PointF, ink: Color) {
    for dx in [-4.5, 1.5] {
        d.fill_round_rect(RectF::new(c.x + dx, c.y - 6.0, 3.0, 12.0), 1.0, ink);
    }
}

/// Previous / next step: a triangle against a bar.
fn skip_icon(d: &mut DrawCtx, r: RectF, next: bool, ink: Color) {
    let c = PointF::new(r.x + r.w / 2.0, r.y + r.h / 2.0);
    let (tip, base, bar) = if next {
        (c.x + 3.0, c.x - 5.0, c.x + 3.5)
    } else {
        (c.x - 3.0, c.x + 5.0, c.x - 5.5)
    };
    d.fill_triangle(
        PointF::new(base, c.y - 5.5),
        PointF::new(tip, c.y),
        PointF::new(base, c.y + 5.5),
        ink,
    );
    d.fill_rect(RectF::new(bar, c.y - 5.5, 2.0, 11.0), ink);
}

fn speed_label(speed: f32) -> String {
    if speed.fract() == 0.0 {
        format!("{speed:.0}\u{d7}")
    } else {
        format!("{speed}\u{d7}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(len: usize) -> (PlayBar, ReplayStatus) {
        let b = PlayBar {
            track: RectF::new(100.0, 20.0, 200.0, TRACK_H),
            ..PlayBar::default()
        };
        let s = ReplayStatus {
            pos: 0,
            len,
            playing: false,
            speed: 1.0,
        };
        (b, s)
    }

    #[test]
    fn track_maps_both_ends_to_first_and_last_step() {
        let (b, s) = bar(11);
        assert_eq!(b.step_at(50.0, s), 0);
        assert_eq!(b.step_at(100.0, s), 0);
        assert_eq!(b.step_at(200.0, s), 5);
        assert_eq!(b.step_at(300.0, s), 10);
        assert_eq!(b.step_at(900.0, s), 10);
        let last = ReplayStatus { pos: 10, ..s };
        assert_eq!(b.knob_x(last), 300.0);
    }

    #[test]
    fn speed_labels() {
        assert_eq!(speed_label(0.5), "0.5\u{d7}");
        assert_eq!(speed_label(2.0), "2\u{d7}");
    }
}
