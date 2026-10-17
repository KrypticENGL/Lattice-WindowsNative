//! The call graph's details window: a small card centred on the canvas
//! that describes one node (a file, class, function, variable, ...). It is
//! modal and fixed in place: while it is open the canvas takes no input,
//! and it closes with `Esc`, its `×`, or a click outside it.

use std::path::PathBuf;

use renderer::Details;

use crate::event::{CursorShape, Event, Key, MouseButton};
use crate::geom::{PointF, RectF};
use crate::panel::Cx;
use crate::sys::gfx::DrawCtx;
use crate::sys::text::{TextStyle, TextSystem};
use crate::theme::Theme;
use crate::visualizer::{rgba, to_rect};

const MAX_W: f32 = 480.0;
const MARGIN: f32 = 24.0;
const PAD: f32 = 16.0;
const HEADER_H: f32 = 62.0;
const FOOTER_H: f32 = 48.0;
const KEY_W: f32 = 112.0;
const ROW_GAP: f32 = 6.0;
const SECTION_GAP: f32 = 10.0;
const WHEEL_LINE: f32 = 40.0;

/// What the host should do after an event.
pub enum ModalAction {
    None,
    Close,
    Open(PathBuf, u32),
}

enum Line {
    Row(String, String),
    Heading(String),
    Item(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    Close,
    Open,
}

pub struct InfoModal {
    details: Details,
    lines: Vec<Line>,
    canvas: RectF,
    card: RectF,
    body: RectF,
    close: RectF,
    open: Option<RectF>,
    hover: Option<Part>,
    scroll: f32,
    content_h: f32,
}

impl InfoModal {
    pub fn new(details: Details) -> Self {
        let mut lines: Vec<Line> = details
            .rows
            .iter()
            .map(|(k, v)| Line::Row(k.clone(), v.clone()))
            .collect();
        for (heading, items) in &details.lists {
            lines.push(Line::Heading(format!("{heading}  ({})", items.len())));
            lines.extend(items.iter().map(|s| Line::Item(s.clone())));
        }
        Self {
            details,
            lines,
            canvas: RectF::default(),
            card: RectF::default(),
            body: RectF::default(),
            close: RectF::default(),
            open: None,
            hover: None,
            scroll: 0.0,
            content_h: 0.0,
        }
    }

    fn line_h(line: &Line, text: &TextSystem) -> f32 {
        match line {
            Line::Heading(_) => text.line_height(TextStyle::UiBold) + ROW_GAP + SECTION_GAP,
            _ => text.line_height(TextStyle::Ui) + ROW_GAP,
        }
    }

    /// Centres the card on `canvas`, as tall as its content allows.
    pub fn layout(&mut self, canvas: RectF, text: &TextSystem) {
        self.canvas = canvas;
        self.content_h = self.lines.iter().map(|l| Self::line_h(l, text)).sum();
        let footer = if self.details.source.is_some() {
            FOOTER_H
        } else {
            PAD
        };
        let w = MAX_W.min(canvas.w - 2.0 * MARGIN).max(160.0);
        let max_h = (canvas.h - 2.0 * MARGIN).max(HEADER_H + footer + 40.0);
        let h = (HEADER_H + self.content_h + footer).min(max_h);
        self.card = RectF::new(
            canvas.x + (canvas.w - w) / 2.0,
            canvas.y + (canvas.h - h) / 2.0,
            w,
            h,
        );
        self.body = RectF::new(
            self.card.x + PAD,
            self.card.y + HEADER_H,
            w - 2.0 * PAD,
            (h - HEADER_H - footer).max(0.0),
        );
        self.close = RectF::new(self.card.right() - 36.0, self.card.y + 10.0, 26.0, 26.0);
        self.open = self.details.source.as_ref().map(|_| {
            let bw = text.measure(TextStyle::Ui, "Open source") + 28.0;
            RectF::new(
                self.card.right() - PAD - bw,
                self.card.bottom() - FOOTER_H + 10.0,
                bw,
                28.0,
            )
        });
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
    }

    fn max_scroll(&self) -> f32 {
        (self.content_h - self.body.h).max(0.0)
    }

    pub fn paint(&self, d: &mut DrawCtx, theme: &Theme) {
        // Dim the canvas behind: the card is modal.
        d.fill_rect(self.canvas, theme.graph_bg.with_alpha(0.55));
        let card = self.card;
        d.fill_round_rect(card, 8.0, theme.panel_bg);
        renderer::Canvas::stroke_rect(d, to_rect(card), 8.0, rgba(theme.border), 1.0);

        let title_w = card.w - 2.0 * PAD - 36.0;
        d.text(
            TextStyle::UiBold,
            &self.details.title,
            RectF::new(card.x + PAD, card.y + 12.0, title_w, 22.0),
            theme.text,
        );
        d.text(
            TextStyle::UiSmall,
            &self.details.kind,
            RectF::new(card.x + PAD, card.y + 34.0, title_w, 18.0),
            theme.accent,
        );
        let close_bg = if self.hover == Some(Part::Close) {
            Some(theme.hover_bg)
        } else {
            None
        };
        if let Some(bg) = close_bg {
            d.fill_round_rect(self.close, 4.0, bg);
        }
        let c = PointF::new(
            self.close.x + self.close.w / 2.0,
            self.close.y + self.close.h / 2.0,
        );
        let k = 4.5;
        let x_color = if close_bg.is_some() {
            theme.text
        } else {
            theme.text_dim
        };
        d.line(
            PointF::new(c.x - k, c.y - k),
            PointF::new(c.x + k, c.y + k),
            x_color,
            1.4,
        );
        d.line(
            PointF::new(c.x - k, c.y + k),
            PointF::new(c.x + k, c.y - k),
            x_color,
            1.4,
        );
        d.fill_rect(
            RectF::new(card.x + 1.0, card.y + HEADER_H - 8.0, card.w - 2.0, 1.0),
            theme.border,
        );

        let body = self.body;
        let scroll = self.scroll;
        let lines = &self.lines;
        d.clip(body, |d| {
            let mut y = body.y - scroll;
            for line in lines {
                let h = Self::line_h(line, d.text_system());
                if y + h >= body.y && y <= body.bottom() {
                    match line {
                        Line::Row(k, v) => {
                            d.text(
                                TextStyle::Ui,
                                k,
                                RectF::new(body.x, y, KEY_W - 8.0, h),
                                theme.text_dim,
                            );
                            d.text(
                                TextStyle::Ui,
                                v,
                                RectF::new(body.x + KEY_W, y, body.w - KEY_W, h),
                                theme.text,
                            );
                        }
                        Line::Heading(s) => {
                            d.text(
                                TextStyle::UiBold,
                                s,
                                RectF::new(body.x, y + SECTION_GAP, body.w, h - SECTION_GAP),
                                theme.text,
                            );
                        }
                        Line::Item(s) => {
                            let dot = PointF::new(body.x + 6.0, y + h / 2.0);
                            d.fill_round_rect(
                                RectF::new(dot.x - 2.0, dot.y - 2.0, 4.0, 4.0),
                                2.0,
                                theme.text_dim,
                            );
                            d.text(
                                TextStyle::Ui,
                                s,
                                RectF::new(body.x + 16.0, y, body.w - 16.0, h),
                                theme.text,
                            );
                        }
                    }
                }
                y += h;
            }
        });
        if self.max_scroll() > 0.0 {
            // Scroll thumb.
            let track = body.h;
            let thumb = (track * body.h / self.content_h).max(24.0);
            let t = scroll / self.max_scroll();
            let r = RectF::new(card.right() - 7.0, body.y + (track - thumb) * t, 3.0, thumb);
            d.fill_round_rect(r, 1.5, theme.text_dim.with_alpha(0.5));
        }

        if let Some(open) = self.open {
            d.fill_rect(
                RectF::new(card.x + 1.0, card.bottom() - FOOTER_H, card.w - 2.0, 1.0),
                theme.border,
            );
            let fill = if self.hover == Some(Part::Open) {
                theme.accent.with_alpha(0.85)
            } else {
                theme.button_bg
            };
            d.fill_round_rect(open, 5.0, fill);
            let w = d.text_system().measure(TextStyle::Ui, "Open source");
            d.text(
                TextStyle::Ui,
                "Open source",
                RectF::new(open.x + (open.w - w) / 2.0, open.y, w + 2.0, open.h),
                theme.button_text,
            );
            if let Some((path, line)) = &self.details.source {
                let name = path
                    .file_name()
                    .map_or_else(|| path.to_string_lossy(), |n| n.to_string_lossy());
                d.text(
                    TextStyle::UiSmall,
                    &format!("{name}:{line}"),
                    RectF::new(card.x + PAD, open.y, open.x - card.x - PAD - 8.0, open.h),
                    theme.text_dim,
                );
            }
        }
    }

    fn part_at(&self, p: PointF) -> Option<Part> {
        if self.close.contains(p) {
            Some(Part::Close)
        } else if self.open.is_some_and(|r| r.contains(p)) {
            Some(Part::Open)
        } else {
            None
        }
    }

    pub fn contains(&self, p: PointF) -> bool {
        self.card.contains(p)
    }

    /// Handles every pointer event and `Esc`. Returns `None` for events it
    /// lets through (other keys).
    pub fn on_event(&mut self, e: &Event, cx: &mut Cx) -> Option<ModalAction> {
        match *e {
            Event::KeyDown {
                key: Key::Escape, ..
            } => Some(ModalAction::Close),
            Event::MouseDown {
                pos,
                button: MouseButton::Left,
                ..
            } => Some(match self.part_at(pos) {
                Some(Part::Close) => ModalAction::Close,
                Some(Part::Open) => match &self.details.source {
                    Some((path, line)) => ModalAction::Open(path.clone(), *line),
                    None => ModalAction::None,
                },
                None if !self.card.contains(pos) => ModalAction::Close,
                None => ModalAction::None,
            }),
            Event::MouseMove { pos, .. } => {
                let hover = self.part_at(pos);
                if hover != self.hover {
                    self.hover = hover;
                    cx.repaint();
                }
                cx.cursor = if hover.is_some() {
                    CursorShape::Hand
                } else {
                    CursorShape::Arrow
                };
                Some(ModalAction::None)
            }
            Event::Wheel { pos, lines, .. } => {
                if self.card.contains(pos) {
                    let before = self.scroll;
                    self.scroll = (self.scroll - lines * WHEEL_LINE).clamp(0.0, self.max_scroll());
                    if self.scroll != before {
                        cx.repaint();
                    }
                }
                Some(ModalAction::None)
            }
            Event::MouseUp { .. } | Event::MouseLeave => {
                if self.hover.take().is_some() {
                    cx.repaint();
                }
                Some(ModalAction::None)
            }
            _ => None,
        }
    }
}
