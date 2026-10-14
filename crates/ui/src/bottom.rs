//! Bottom area: a small tab strip switching between Output and Terminal.

use crate::event::{Event, MouseButton};
use crate::geom::RectF;
use crate::output::Output;
use crate::panel::{Cx, Panel};
use crate::sys::gfx::DrawCtx;
use crate::sys::text::{TextStyle, TextSystem};
use crate::sys::Waker;
use crate::terminal::Terminal;
use crate::theme::Theme;

const STRIP_H: f32 = 30.0;
const TAB_PAD: f32 = 12.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BottomTab {
    Output,
    Terminal,
}

pub struct BottomArea {
    pub tab: BottomTab,
    pub output: Output,
    pub terminal: Terminal,
    rect: RectF,
    strip: RectF,
    body: RectF,
    tabs: [RectF; 2],
}

impl BottomArea {
    pub fn new(waker: Waker) -> Self {
        Self {
            tab: BottomTab::Output,
            output: Output::new(),
            terminal: Terminal::new(waker),
            rect: RectF::default(),
            strip: RectF::default(),
            body: RectF::default(),
            tabs: [RectF::default(); 2],
        }
    }

    fn label(tab: BottomTab) -> &'static str {
        match tab {
            BottomTab::Output => "OUTPUT",
            BottomTab::Terminal => "TERMINAL",
        }
    }

    pub fn show(&mut self, tab: BottomTab, cx: &mut Cx) {
        if self.tab != tab {
            self.tab = tab;
            cx.repaint();
        }
        if tab == BottomTab::Terminal && !self.terminal.is_running() {
            self.terminal.start(cx);
        }
    }

    fn active(&mut self) -> &mut dyn Panel {
        match self.tab {
            BottomTab::Output => &mut self.output,
            BottomTab::Terminal => &mut self.terminal,
        }
    }
}

impl Panel for BottomArea {
    fn layout(&mut self, rect: RectF, text: &TextSystem) {
        self.rect = rect;
        let (strip, body) = rect.split_top(STRIP_H);
        self.strip = strip;
        self.body = body;
        let mut x = strip.x + 4.0;
        for (i, tab) in [BottomTab::Output, BottomTab::Terminal]
            .into_iter()
            .enumerate()
        {
            let w = text.measure(TextStyle::UiSmall, Self::label(tab)) + 2.0 * TAB_PAD;
            self.tabs[i] = RectF::new(x, strip.y, w, strip.h);
            x += w;
        }
        // Both panels are laid out so the terminal keeps its size (and the
        // shell its column count) while the Output tab is showing.
        self.output.layout(body, text);
        self.terminal.layout(body, text);
    }

    fn paint(&mut self, d: &mut DrawCtx, theme: &Theme, focused: bool) {
        d.fill_rect(self.strip, theme.panel_bg);
        for (i, tab) in [BottomTab::Output, BottomTab::Terminal]
            .into_iter()
            .enumerate()
        {
            let r = self.tabs[i];
            let active = tab == self.tab;
            let color = if active {
                theme.text_bright
            } else {
                theme.text_dim
            };
            d.text(
                TextStyle::UiSmall,
                Self::label(tab),
                RectF::new(r.x + TAB_PAD, r.y, r.w - TAB_PAD, r.h),
                color,
            );
            if active {
                let underline = if focused {
                    theme.accent
                } else {
                    theme.text_dim
                };
                d.fill_rect(
                    RectF::new(
                        r.x + TAB_PAD - 2.0,
                        r.bottom() - 3.0,
                        r.w - 2.0 * TAB_PAD + 4.0,
                        1.5,
                    ),
                    underline,
                );
            }
        }
        self.active().paint(d, theme, focused);
    }

    fn on_event(&mut self, e: &Event, cx: &mut Cx) -> bool {
        if let Event::MouseDown {
            pos,
            button: MouseButton::Left,
            ..
        } = *e
        {
            if self.strip.contains(pos) {
                if self.tabs[0].contains(pos) {
                    self.show(BottomTab::Output, cx);
                } else if self.tabs[1].contains(pos) {
                    self.show(BottomTab::Terminal, cx);
                }
                return true;
            }
        }
        self.active().on_event(e, cx)
    }
}
