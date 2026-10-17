//! The window's title bar, drawn by the app: the logo, the menu bar (see
//! `menubar.rs`), the window title and the minimize / maximize / close
//! buttons. The window procedure asks [`TitleBar::hit`] which parts drag the
//! window and which are caption buttons, so Windows keeps snapping, the
//! system menu and the Windows 11 snap layouts flyout.

use crate::geom::{PointF, RectF};
use crate::sys::gfx::DrawCtx;
use crate::sys::text::{TextStyle, TextSystem};
use crate::theme::Theme;

pub const TITLE_BAR_H: f32 = 34.0;
const LOGO_W: f32 = 40.0;
const BUTTON_W: f32 = 46.0;
/// Caption glyph size.
const GLYPH: f32 = 10.0;

/// What a point in the window is, for the window procedure's hit test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowHit {
    /// Handled by the UI.
    Client,
    /// Drags the window; a double-click maximizes.
    Caption,
    Minimize,
    Maximize,
    Close,
}

impl WindowHit {
    const BUTTONS: [Self; 3] = [Self::Minimize, Self::Maximize, Self::Close];
}

#[derive(Debug, Default)]
pub struct TitleBar {
    rect: RectF,
    logo: RectF,
    buttons: [RectF; 3],
    /// The caption button under the pointer.
    pub hover: Option<WindowHit>,
    /// The caption button being pressed.
    pub pressed: Option<WindowHit>,
    pub maximized: bool,
}

impl TitleBar {
    /// Lays the bar out over the window's top; returns the room left for the
    /// menu bar.
    pub fn layout(&mut self, rect: RectF) -> RectF {
        self.rect = rect;
        self.logo = RectF::new(rect.x, rect.y, LOGO_W, rect.h);
        let mut x = rect.right() - 3.0 * BUTTON_W;
        for b in &mut self.buttons {
            *b = RectF::new(x, rect.y, BUTTON_W, rect.h);
            x += BUTTON_W;
        }
        let left = self.logo.right();
        RectF::new(left, rect.y, (self.buttons[0].x - left).max(0.0), rect.h)
    }

    /// The caption button at `p`, or `Caption` anywhere else on the bar.
    pub fn hit(&self, p: PointF) -> WindowHit {
        if !self.rect.contains(p) {
            return WindowHit::Client;
        }
        WindowHit::BUTTONS
            .into_iter()
            .zip(self.buttons)
            .find(|(_, r)| r.contains(p))
            .map_or(WindowHit::Caption, |(b, _)| b)
    }

    /// The bar's background, logo and buttons; `title` is centred in the
    /// free space between `menu_end` and the buttons.
    pub fn paint(&self, d: &mut DrawCtx, theme: &Theme, title: &str, menu_end: f32) {
        d.fill_rect(self.rect, theme.chrome_bg);
        self.paint_logo(d, theme);

        let text: &TextSystem = d.text_system();
        let w = text.measure(TextStyle::UiSmall, title);
        let free_l = menu_end + 16.0;
        let free_r = self.buttons[0].x - 16.0;
        // Centred on the window if it fits there, else in the free space.
        let mut x = self.rect.x + (self.rect.w - w) / 2.0;
        if x < free_l || x + w > free_r {
            x = free_l.max(free_r - w);
        }
        if free_r > free_l {
            d.clip(
                RectF::new(free_l, self.rect.y, free_r - free_l, self.rect.h),
                |d| {
                    d.text(
                        TextStyle::UiSmall,
                        title,
                        RectF::new(x, self.rect.y, w + 4.0, self.rect.h),
                        theme.text_dim,
                    );
                },
            );
        }

        for (b, r) in WindowHit::BUTTONS.into_iter().zip(self.buttons) {
            let hot = self.hover == Some(b);
            let down = hot && self.pressed == Some(b);
            let glyph = match (b, hot) {
                (WindowHit::Close, true) => {
                    let bg = theme.caption_close;
                    d.fill_rect(r, if down { bg.with_alpha(0.8) } else { bg });
                    theme.text_bright
                }
                (_, true) => {
                    let bg = if down {
                        theme.row_selected_bg
                    } else {
                        theme.hover_bg
                    };
                    d.fill_rect(r, bg);
                    theme.text_bright
                }
                _ => theme.text,
            };
            self.paint_glyph(d, b, r, glyph);
        }
    }

    /// A small graph: three lime nodes, joined.
    fn paint_logo(&self, d: &mut DrawCtx, theme: &Theme) {
        let c = PointF::new(
            self.logo.x + self.logo.w / 2.0 + 2.0,
            self.logo.y + self.logo.h / 2.0,
        );
        let nodes = [
            PointF::new(c.x - 6.0, c.y + 5.0),
            PointF::new(c.x + 6.0, c.y + 5.0),
            PointF::new(c.x, c.y - 6.0),
        ];
        let edge = theme.accent.with_alpha(0.55);
        for i in 0..3 {
            d.line(nodes[i], nodes[(i + 1) % 3], edge, 1.5);
        }
        for n in nodes {
            d.fill_round_rect(
                RectF::new(n.x - 3.0, n.y - 3.0, 6.0, 6.0),
                3.0,
                theme.accent,
            );
        }
    }

    fn paint_glyph(&self, d: &mut DrawCtx, b: WindowHit, r: RectF, color: crate::theme::Color) {
        let c = PointF::new(
            (r.x + r.w / 2.0).round() + 0.5,
            (r.y + r.h / 2.0).round() + 0.5,
        );
        let h = GLYPH / 2.0;
        match b {
            WindowHit::Minimize => {
                d.line(
                    PointF::new(c.x - h, c.y),
                    PointF::new(c.x + h, c.y),
                    color,
                    1.0,
                );
            }
            WindowHit::Maximize if self.maximized => {
                // Restore: a square with another behind it.
                let s = GLYPH - 2.0;
                let front = RectF::new(c.x - h, c.y - h + 2.0, s, s);
                d.stroke_rect(front, color, 1.0);
                let (x0, y0) = (front.x + 2.0, front.y - 2.0);
                let (x1, y1) = (front.right() + 2.0, front.bottom() - 2.0);
                d.line(PointF::new(x0, y0), PointF::new(x1, y0), color, 1.0);
                d.line(PointF::new(x1, y0), PointF::new(x1, y1), color, 1.0);
            }
            WindowHit::Maximize => {
                d.stroke_rect(RectF::new(c.x - h, c.y - h, GLYPH, GLYPH), color, 1.0);
            }
            WindowHit::Close => {
                d.line(
                    PointF::new(c.x - h, c.y - h),
                    PointF::new(c.x + h, c.y + h),
                    color,
                    1.0,
                );
                d.line(
                    PointF::new(c.x + h, c.y - h),
                    PointF::new(c.x - h, c.y + h),
                    color,
                    1.0,
                );
            }
            WindowHit::Client | WindowHit::Caption => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_finds_buttons_and_caption() {
        let mut t = TitleBar::default();
        let menus = t.layout(RectF::new(0.0, 0.0, 1000.0, TITLE_BAR_H));
        assert_eq!(menus.x, LOGO_W);
        assert_eq!(menus.right(), 1000.0 - 3.0 * BUTTON_W);
        assert_eq!(t.hit(PointF::new(990.0, 10.0)), WindowHit::Close);
        assert_eq!(t.hit(PointF::new(1000.0 - 70.0, 10.0)), WindowHit::Maximize);
        assert_eq!(
            t.hit(PointF::new(1000.0 - 120.0, 10.0)),
            WindowHit::Minimize
        );
        assert_eq!(t.hit(PointF::new(500.0, 10.0)), WindowHit::Caption);
        assert_eq!(t.hit(PointF::new(500.0, 60.0)), WindowHit::Client);
    }
}
