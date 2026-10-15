//! Tabbed editor area: a tab strip over the active [`Document`].

mod buffer;
mod undo;
mod view;

use std::path::Path;

use view::Document;

use crate::event::{CursorShape, Event, Key, MouseButton};
use crate::geom::{PointF, RectF};
use crate::panel::{Cx, Panel};
use crate::sys::dialog::{self, SaveChoice};
use crate::sys::gfx::DrawCtx;
use crate::sys::text::{TextStyle, TextSystem};
use crate::theme::Theme;

const STRIP_H: f32 = 34.0;
const TAB_PAD: f32 = 12.0;
const CLOSE_W: f32 = 18.0;

struct TabHit {
    tab: RectF,
    close: RectF,
}

#[derive(Default)]
pub struct EditorArea {
    docs: Vec<Document>,
    active: usize,
    rect: RectF,
    strip: RectF,
    body: RectF,
    hits: Vec<TabHit>,
    hover: Option<usize>,
    hover_close: Option<usize>,
}

impl EditorArea {
    pub fn has_documents(&self) -> bool {
        !self.docs.is_empty()
    }

    /// Opens `path`, or activates it if already open.
    pub fn open(&mut self, path: &Path, cx: &mut Cx) {
        if let Some(i) = self.docs.iter().position(|d| d.path() == path) {
            self.active = i;
            cx.repaint();
            return;
        }
        match Document::open(path) {
            Ok(mut doc) => {
                if doc.is_read_only() {
                    cx.log_error(format!(
                        "{} is not valid UTF-8; opened read-only",
                        path.display()
                    ));
                } else {
                    cx.log(format!("Opened {}", path.display()));
                }
                doc.layout(self.body, cx.text);
                self.docs.push(doc);
                self.active = self.docs.len() - 1;
                cx.repaint();
            }
            Err(e) => cx.log_error(format!("Open failed: {e}")),
        }
    }

    /// Opens `path` (or activates it) with the caret on `line` (1-based).
    pub fn open_at(&mut self, path: &Path, line: u32, cx: &mut Cx) {
        self.open(path, cx);
        if let Some(doc) = self.docs.get_mut(self.active) {
            if doc.path() == path {
                doc.goto_line(line);
                cx.repaint();
            }
        }
    }

    /// Closes tab `i`, prompting if it has unsaved changes. Returns `false`
    /// if the user cancelled.
    fn close(&mut self, i: usize, cx: &mut Cx) -> bool {
        let Some(doc) = self.docs.get_mut(i) else {
            return true;
        };
        if doc.is_dirty() && !resolve_unsaved(doc, cx) {
            return false;
        }
        self.docs.remove(i);
        if self.active >= self.docs.len() {
            self.active = self.docs.len().saturating_sub(1);
        } else if self.active > i {
            self.active -= 1;
        }
        self.hover = None;
        self.hover_close = None;
        cx.repaint();
        true
    }

    /// Prompts for every dirty document. `false` if the user cancelled.
    pub fn query_close(&mut self, cx: &mut Cx) -> bool {
        for i in 0..self.docs.len() {
            if self.docs[i].is_dirty() {
                self.active = i;
                if !resolve_unsaved(&mut self.docs[i], cx) {
                    return false;
                }
            }
        }
        true
    }

    pub fn clear_caches(&mut self) {
        for doc in &mut self.docs {
            doc.clear_layouts();
        }
    }

    fn tab_label(doc: &Document) -> String {
        if doc.is_read_only() {
            format!("{} (read-only)", doc.name())
        } else {
            doc.name().to_owned()
        }
    }

    fn compute_hits(&mut self, text: &TextSystem) {
        self.hits.clear();
        let mut x = self.strip.x;
        for doc in &self.docs {
            let w = text.measure(TextStyle::Ui, &Self::tab_label(doc)) + 2.0 * TAB_PAD + CLOSE_W;
            let tab = RectF::new(x, self.strip.y, w, self.strip.h);
            let close = RectF::new(
                tab.right() - TAB_PAD / 2.0 - CLOSE_W,
                tab.y + (tab.h - CLOSE_W) / 2.0,
                CLOSE_W,
                CLOSE_W,
            );
            self.hits.push(TabHit { tab, close });
            x += w;
        }
    }

    fn hit(&self, p: PointF) -> Option<(usize, bool)> {
        self.hits
            .iter()
            .position(|h| h.tab.contains(p))
            .map(|i| (i, self.hits[i].close.contains(p)))
    }
}

/// Asks to save a dirty document. `false` means cancel.
fn resolve_unsaved(doc: &mut Document, cx: &mut Cx) -> bool {
    match dialog::confirm_save(cx.window, doc.name()) {
        SaveChoice::Save => match doc.save() {
            Ok(()) => {
                cx.log(format!("Saved {}", doc.path().display()));
                true
            }
            Err(e) => {
                cx.log_error(format!("Save failed: {e}"));
                false
            }
        },
        SaveChoice::Discard => true,
        SaveChoice::Cancel => false,
    }
}

impl Panel for EditorArea {
    fn layout(&mut self, rect: RectF, text: &TextSystem) {
        self.rect = rect;
        let (strip, body) = rect.split_top(STRIP_H);
        self.strip = strip;
        self.body = body;
        self.compute_hits(text);
        if let Some(doc) = self.docs.get_mut(self.active) {
            doc.layout(body, text);
        }
    }

    fn paint(&mut self, d: &mut DrawCtx, theme: &Theme, focused: bool) {
        d.fill_rect(self.strip, theme.tab_inactive_bg);
        let strip = self.strip;
        let active = self.active;
        let hover = self.hover;
        let hover_close = self.hover_close;
        let labels: Vec<(String, bool)> = self
            .docs
            .iter()
            .map(|doc| (Self::tab_label(doc), doc.is_dirty()))
            .collect();
        let hits = &self.hits;
        d.clip(strip, |d| {
            for (i, (hit, (label, dirty))) in hits.iter().zip(&labels).enumerate() {
                let is_active = i == active;
                let bg = if is_active {
                    theme.tab_active_bg
                } else if hover == Some(i) {
                    theme.hover_bg
                } else {
                    theme.tab_inactive_bg
                };
                d.fill_rect(hit.tab, bg);
                if is_active {
                    let accent = if focused { theme.accent } else { theme.border };
                    d.fill_rect(RectF::new(hit.tab.x, hit.tab.y, hit.tab.w, 2.0), accent);
                }
                d.fill_rect(
                    RectF::new(hit.tab.right() - 1.0, hit.tab.y, 1.0, hit.tab.h),
                    theme.border,
                );
                let text_rect = RectF::new(
                    hit.tab.x + TAB_PAD,
                    hit.tab.y,
                    hit.tab.w - 2.0 * TAB_PAD - CLOSE_W,
                    hit.tab.h,
                );
                let color = if is_active {
                    theme.text_bright
                } else {
                    theme.text_dim
                };
                d.text(TextStyle::Ui, label, text_rect, color);
                // Close button, or a dirty dot when not hovered.
                let show_close = hover == Some(i) || is_active;
                if hover_close == Some(i) {
                    d.fill_round_rect(hit.close, 3.0, theme.hover_bg);
                }
                if *dirty && hover_close != Some(i) {
                    let c = hit.close;
                    d.fill_round_rect(
                        RectF::new(c.x + c.w / 2.0 - 4.0, c.y + c.h / 2.0 - 4.0, 8.0, 8.0),
                        4.0,
                        theme.dirty,
                    );
                } else if show_close {
                    let c = hit.close.inset(5.0);
                    d.line(
                        PointF::new(c.x, c.y),
                        PointF::new(c.right(), c.bottom()),
                        color,
                        1.2,
                    );
                    d.line(
                        PointF::new(c.right(), c.y),
                        PointF::new(c.x, c.bottom()),
                        color,
                        1.2,
                    );
                }
            }
        });
        d.fill_rect(
            RectF::new(strip.x, strip.bottom() - 1.0, strip.w, 1.0),
            theme.border,
        );

        match self.docs.get_mut(self.active) {
            Some(doc) => doc.paint(d, theme, focused),
            None => {
                d.fill_rect(self.body, theme.editor_bg);
                d.text(
                    TextStyle::Ui,
                    "Double-click a file in the Explorer to open it",
                    RectF::new(
                        self.body.x + 24.0,
                        self.body.y + self.body.h / 2.0 - 12.0,
                        self.body.w - 48.0,
                        24.0,
                    ),
                    theme.text_dim,
                );
            }
        }
    }

    fn on_event(&mut self, e: &Event, cx: &mut Cx) -> bool {
        match *e {
            Event::KeyDown {
                key: Key::Tab,
                mods,
            } if mods.ctrl => {
                if !self.docs.is_empty() {
                    let n = self.docs.len();
                    self.active = if mods.shift {
                        (self.active + n - 1) % n
                    } else {
                        (self.active + 1) % n
                    };
                    cx.repaint();
                }
                return true;
            }
            Event::KeyDown {
                key: Key::Letter('W'),
                mods,
            } if mods.ctrl => {
                if !self.docs.is_empty() {
                    self.close(self.active, cx);
                }
                return true;
            }
            Event::MouseDown { pos, button, .. } if self.strip.contains(pos) => {
                if let Some((i, on_close)) = self.hit(pos) {
                    if button == MouseButton::Middle || (button == MouseButton::Left && on_close) {
                        self.close(i, cx);
                    } else if button == MouseButton::Left {
                        self.active = i;
                        cx.repaint();
                    }
                }
                return true;
            }
            Event::MouseMove { pos, .. } if self.strip.contains(pos) => {
                let (hover, hover_close) = match self.hit(pos) {
                    Some((i, c)) => (Some(i), c.then_some(i)),
                    None => (None, None),
                };
                if (hover, hover_close) != (self.hover, self.hover_close) {
                    self.hover = hover;
                    self.hover_close = hover_close;
                    cx.repaint();
                }
                if hover.is_some() {
                    cx.cursor = CursorShape::Arrow;
                }
                // Keep forwarding moves to the document so drags continue.
            }
            Event::MouseMove { .. } | Event::MouseLeave
                if self.hover.is_some() || self.hover_close.is_some() =>
            {
                self.hover = None;
                self.hover_close = None;
                cx.repaint();
            }
            _ => {}
        }
        let body = self.body;
        let text = cx.text;
        match self.docs.get_mut(self.active) {
            Some(doc) => {
                doc.layout(body, text);
                doc.on_event(e, cx)
            }
            None => false,
        }
    }
}
