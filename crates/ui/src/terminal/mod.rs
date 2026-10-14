//! Terminal panel: a ConPTY shell rendered from the emulator's cell grid.

mod grid;
mod input;

use std::path::PathBuf;

use grid::{Attr, Grid, TermColor};

use crate::event::{CursorShape, Event, Key, MouseButton};
use crate::geom::{PointF, RectF};
use crate::panel::{Cx, Panel};
use crate::sys::clipboard;
use crate::sys::conpty::{Pty, PtyEvent};
use crate::sys::gfx::DrawCtx;
use crate::sys::text::{TextStyle, TextSystem};
use crate::sys::Waker;
use crate::theme::{Color, Theme};

const PAD: f32 = 6.0;
const SHELL: &str = "powershell.exe -NoLogo";

pub struct Terminal {
    parser: vte::Parser,
    grid: Grid,
    pty: Option<Pty>,
    waker: Waker,
    /// Exit code of the last shell, shown until restart.
    exited: Option<u32>,
    /// Lines scrolled back into history (0 = live view).
    view_offset: usize,
    cwd: Option<PathBuf>,
    rect: RectF,
    cell_w: f32,
    cell_h: f32,
}

impl Terminal {
    pub fn new(waker: Waker) -> Self {
        Self {
            parser: vte::Parser::new(),
            grid: Grid::new(80, 24),
            pty: None,
            waker,
            exited: None,
            view_offset: 0,
            cwd: None,
            rect: RectF::default(),
            cell_w: 8.0,
            cell_h: 16.0,
        }
    }

    /// Working directory for the next shell start.
    pub fn set_cwd(&mut self, cwd: Option<PathBuf>) {
        self.cwd = cwd;
    }

    pub fn is_running(&self) -> bool {
        self.pty.is_some()
    }

    /// Starts the shell if it is not running.
    pub fn start(&mut self, cx: &mut Cx) {
        if self.pty.is_some() {
            return;
        }
        self.grid = Grid::new(self.grid.cols(), self.grid.rows());
        self.parser = vte::Parser::new();
        self.view_offset = 0;
        let cols = u16::try_from(self.grid.cols()).unwrap_or(u16::MAX);
        let rows = u16::try_from(self.grid.rows()).unwrap_or(u16::MAX);
        match Pty::spawn(SHELL, self.cwd.as_deref(), cols, rows, self.waker.clone()) {
            Ok(pty) => {
                self.pty = Some(pty);
                self.exited = None;
                let dir = self
                    .cwd
                    .as_ref()
                    .map_or_else(|| ".".to_owned(), |p| p.display().to_string());
                cx.log(format!(
                    "Terminal started: {SHELL} in {dir} ({cols}x{rows})"
                ));
            }
            Err(e) => cx.log_error(format!("Terminal failed to start: {e}")),
        }
        cx.repaint();
    }

    /// Drains PTY output. Called on the UI thread after a wake.
    pub fn pump(&mut self, cx: &mut Cx) {
        let mut exited = None;
        let mut got_output = false;
        if let Some(pty) = &self.pty {
            while let Some(event) = pty.try_recv() {
                match event {
                    PtyEvent::Output(bytes) => {
                        self.parser.advance(&mut self.grid, &bytes);
                        got_output = true;
                    }
                    PtyEvent::Exited(code) => exited = Some(code),
                }
            }
            let replies = self.grid.take_responses();
            if !replies.is_empty() {
                let _ = pty.write(&replies);
            }
        }
        if got_output {
            cx.repaint();
        }
        if let Some(code) = exited {
            self.pty = None;
            self.exited = Some(code);
            cx.log(format!("Terminal exited with code {code}"));
            cx.repaint();
        }
    }

    /// Types `command` followed by Enter into the running shell.
    pub fn run_command(&mut self, command: &str, cx: &mut Cx) {
        self.send(format!("{command}\r").as_bytes(), cx);
    }

    fn send(&mut self, bytes: &[u8], cx: &mut Cx) {
        if let Some(pty) = &self.pty {
            if let Err(e) = pty.write(bytes) {
                cx.log_error(format!("Terminal write failed: {e}"));
            }
            if self.view_offset != 0 {
                self.view_offset = 0;
                cx.repaint();
            }
        }
    }

    fn resolve(&self, c: TermColor, theme: &Theme, default: Color, bold: bool) -> Color {
        match c {
            TermColor::Default => default,
            TermColor::Indexed(i) => {
                let i = if bold && i < 8 { i + 8 } else { i };
                xterm_color(i, theme)
            }
            TermColor::Rgb(r, g, b) => Color::rgb8(r, g, b),
        }
    }

    fn colors(&self, attr: Attr, theme: &Theme) -> (Color, Option<Color>) {
        let fg = self.resolve(attr.fg, theme, theme.term_fg, attr.bold);
        let bg = match attr.bg {
            TermColor::Default => None,
            c => Some(self.resolve(c, theme, theme.term_bg, false)),
        };
        if attr.inverse {
            (bg.unwrap_or(theme.term_bg), Some(fg))
        } else {
            (fg, bg)
        }
    }
}

/// The xterm 256-colour palette, with 0–15 from the theme.
fn xterm_color(i: u8, theme: &Theme) -> Color {
    match i {
        0..=15 => theme.term_palette[usize::from(i)],
        16..=231 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color::rgb8(level(i / 36), level((i / 6) % 6), level(i % 6))
        }
        232..=255 => {
            let v = 8 + (i - 232) * 10;
            Color::rgb8(v, v, v)
        }
    }
}

impl Panel for Terminal {
    fn layout(&mut self, rect: RectF, text: &TextSystem) {
        self.rect = rect;
        let cell = text.cell_size();
        self.cell_w = cell.w.max(1.0);
        self.cell_h = cell.h.max(1.0);
        let cols = (((rect.w - 2.0 * PAD) / self.cell_w).floor() as usize).max(2);
        let rows = (((rect.h - 2.0 * PAD) / self.cell_h).floor() as usize).max(1);
        if rect.is_empty() || (cols == self.grid.cols() && rows == self.grid.rows()) {
            return;
        }
        self.grid.resize(cols, rows);
        if let Some(pty) = &self.pty {
            let c = u16::try_from(cols).unwrap_or(u16::MAX);
            let r = u16::try_from(rows).unwrap_or(u16::MAX);
            let _ = pty.resize(c, r);
        }
    }

    fn paint(&mut self, d: &mut DrawCtx, theme: &Theme, focused: bool) {
        d.fill_rect(self.rect, theme.term_bg);
        let origin = PointF::new(self.rect.x + PAD, self.rect.y + PAD);
        let (cw, ch) = (self.cell_w, self.cell_h);
        let offset = self.view_offset.min(self.grid.scrollback_len());
        let rect = self.rect;
        d.clip(rect, |d| {
            for row in 0..self.grid.rows() {
                let cells = self.grid.view_row(row, offset);
                let y = origin.y + row as f32 * ch;
                // Group cells into runs of identical attributes.
                let mut start = 0;
                while start < cells.len() {
                    let attr = cells[start].attr;
                    let mut end = start + 1;
                    while end < cells.len() && cells[end].attr == attr {
                        end += 1;
                    }
                    let (fg, bg) = self.colors(attr, theme);
                    let x = origin.x + start as f32 * cw;
                    if let Some(bg) = bg {
                        d.fill_rect(RectF::new(x, y, (end - start) as f32 * cw, ch), bg);
                    }
                    let run: String = cells[start..end].iter().map(|c| c.ch).collect();
                    let trimmed = run.trim_end();
                    if !trimmed.is_empty() {
                        d.text_at(TextStyle::Mono, trimmed, PointF::new(x, y), fg);
                    }
                    if attr.underline {
                        d.fill_rect(
                            RectF::new(x, y + ch - 1.0, (end - start) as f32 * cw, 1.0),
                            fg,
                        );
                    }
                    start = end;
                }
            }
            if offset == 0 && self.pty.is_some() && self.grid.cursor_visible() {
                let (r, c) = self.grid.cursor();
                let cur = RectF::new(origin.x + c as f32 * cw, origin.y + r as f32 * ch, cw, ch);
                if focused {
                    d.fill_rect(cur, theme.caret.with_alpha(0.7));
                } else {
                    d.stroke_rect(cur, theme.caret, 1.0);
                }
            }
            if let Some(code) = self.exited {
                let msg = format!("[process exited with code {code}; press Enter to restart]");
                let y = rect.bottom() - PAD - ch;
                d.fill_rect(
                    RectF::new(rect.x, y - 2.0, rect.w, ch + 4.0),
                    theme.chrome_bg,
                );
                d.text_at(TextStyle::Mono, &msg, PointF::new(origin.x, y), theme.dirty);
            }
        });
    }

    fn on_event(&mut self, e: &Event, cx: &mut Cx) -> bool {
        match *e {
            Event::KeyDown {
                key: Key::Enter, ..
            } if self.pty.is_none() => {
                self.start(cx);
                true
            }
            Event::KeyDown {
                key: Key::Letter('V'),
                mods,
            } if mods.ctrl => {
                if let Some(text) = clipboard::get_text(cx.window) {
                    let bytes = input::encode_paste(&text, self.grid.bracketed_paste());
                    self.send(&bytes, cx);
                }
                true
            }
            Event::KeyDown { key, mods } => {
                match input::encode_key(key, mods, self.grid.app_cursor_keys()) {
                    Some(bytes) => {
                        self.send(&bytes, cx);
                        true
                    }
                    None => false,
                }
            }
            Event::Char(c) => {
                let mut buf = [0u8; 4];
                let s = c.encode_utf8(&mut buf);
                self.send(s.as_bytes(), cx);
                true
            }
            Event::Wheel { lines, .. } => {
                let max = self.grid.scrollback_len() as isize;
                let next = (self.view_offset as isize + (lines * 3.0) as isize).clamp(0, max);
                self.view_offset = next as usize;
                cx.repaint();
                true
            }
            Event::MouseMove { pos, .. } => {
                if self.rect.contains(pos) {
                    cx.cursor = CursorShape::IBeam;
                }
                true
            }
            Event::MouseDown {
                button: MouseButton::Left,
                ..
            } => true,
            _ => false,
        }
    }
}
