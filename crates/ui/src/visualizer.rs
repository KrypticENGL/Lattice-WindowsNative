//! Visualizer tab placeholder. It is a separate root view: it owns its whole
//! area and never shares layout or input with the Code tab. Graph drawing
//! arrives in stage 4.

use crate::event::Event;
use crate::geom::RectF;
use crate::panel::{Cx, Panel};
use crate::sys::gfx::DrawCtx;
use crate::sys::text::{TextStyle, TextSystem};
use crate::theme::Theme;

#[derive(Default)]
pub struct Visualizer {
    rect: RectF,
}

impl Panel for Visualizer {
    fn layout(&mut self, rect: RectF, _text: &TextSystem) {
        self.rect = rect;
    }

    fn paint(&mut self, d: &mut DrawCtx, theme: &Theme, _focused: bool) {
        d.fill_rect(self.rect, theme.editor_bg);
        let r = self.rect;
        let lines = [
            (TextStyle::UiBold, "Visualizer", theme.text),
            (
                TextStyle::Ui,
                "The code graph will be drawn here in a later stage.",
                theme.text_dim,
            ),
        ];
        let mut y = r.y + r.h / 2.0 - 24.0;
        for (style, text, color) in lines {
            let w = d.text_system().measure(style, text);
            d.text(
                style,
                text,
                RectF::new(r.x + ((r.w - w) / 2.0).max(8.0), y, w + 4.0, 24.0),
                color,
            );
            y += 26.0;
        }
    }

    fn on_event(&mut self, _e: &Event, _cx: &mut Cx) -> bool {
        false
    }
}
