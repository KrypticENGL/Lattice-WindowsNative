//! Output panel: timestamped log lines from the IDE.

use crate::event::Event;
use crate::geom::{PointF, RectF};
use crate::panel::{Cx, Panel};
use crate::sys::gfx::DrawCtx;
use crate::sys::text::{TextStyle, TextSystem};
use crate::sys::time;
use crate::theme::Theme;

const MAX_LINES: usize = 10_000;
const PAD: f32 = 8.0;

struct Line {
    time: String,
    text: String,
    error: bool,
}

#[derive(Default)]
pub struct Output {
    lines: Vec<Line>,
    rect: RectF,
    line_h: f32,
    /// Scroll offset in DIPs from the top.
    scroll: f32,
    /// Follow new lines while the view is at the bottom.
    follow: bool,
}

impl Output {
    pub fn new() -> Self {
        Self {
            follow: true,
            line_h: 18.0,
            ..Self::default()
        }
    }

    pub fn push(&mut self, error: bool, text: &str) {
        for part in text.lines() {
            self.lines.push(Line {
                time: time::local_hms(),
                text: part.to_owned(),
                error,
            });
        }
        if self.lines.len() > MAX_LINES {
            let excess = self.lines.len() - MAX_LINES;
            self.lines.drain(..excess);
        }
        if self.follow {
            self.scroll = self.max_scroll();
        }
    }

    fn max_scroll(&self) -> f32 {
        (self.lines.len() as f32 * self.line_h + 2.0 * PAD - self.rect.h).max(0.0)
    }
}

impl Panel for Output {
    fn layout(&mut self, rect: RectF, text: &TextSystem) {
        self.rect = rect;
        self.line_h = text.line_height(TextStyle::Mono).max(1.0);
        if self.follow {
            self.scroll = self.max_scroll();
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
    }

    fn paint(&mut self, d: &mut DrawCtx, theme: &Theme, _focused: bool) {
        d.fill_rect(self.rect, theme.panel_bg);
        let time_w = d.text_system().cell_size().w * 9.0;
        let first = (self.scroll / self.line_h) as usize;
        let count = (self.rect.h / self.line_h).ceil() as usize + 1;
        let rect = self.rect;
        let lines = &self.lines;
        let (scroll, line_h) = (self.scroll, self.line_h);
        d.clip(rect, |d| {
            for (i, line) in lines.iter().enumerate().skip(first).take(count) {
                let y = rect.y + PAD + i as f32 * line_h - scroll;
                d.text_at(
                    TextStyle::Mono,
                    &line.time,
                    PointF::new(rect.x + PAD, y),
                    theme.text_dim,
                );
                let color = if line.error { theme.error } else { theme.text };
                d.text_at(
                    TextStyle::Mono,
                    &line.text,
                    PointF::new(rect.x + PAD + time_w, y),
                    color,
                );
            }
        });
    }

    fn on_event(&mut self, e: &Event, cx: &mut Cx) -> bool {
        if let Event::Wheel { lines, .. } = *e {
            self.scroll = (self.scroll - lines * 3.0 * self.line_h).clamp(0.0, self.max_scroll());
            self.follow = self.scroll >= self.max_scroll() - 1.0;
            cx.repaint();
            return true;
        }
        matches!(e, Event::MouseDown { .. })
    }
}
