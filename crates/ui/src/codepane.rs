//! The source pane in the Data mode's debug column: the file of the frame
//! shown in the call stack, scrolled to the line it is stopped at. The
//! innermost frame's line is marked like the editor's (lime chevron and
//! tint); a caller's line, its call site, gets a fainter tint.
//!
//! Read-only. The gutter toggles breakpoints; double-clicking a line opens
//! it in the editor.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::event::{Event, MouseButton};
use crate::geom::{PointF, RectF};
use crate::panel::{Action, Cx, Panel};
use crate::sys::gfx::DrawCtx;
use crate::sys::text::{TextStyle, TextSystem};
use crate::theme::Theme;

const HEADER_H: f32 = 30.0;
const PAD: f32 = 10.0;
const GUTTER_PAD: f32 = 14.0;
const TAB: usize = 4;
/// Larger files are not shown.
const MAX_BYTES: u64 = 8 << 20;
/// Text alpha while the program runs and the line is from the last stop.
const STALE: f32 = 0.55;
/// A caller frame's line: this much of the stop-line tint.
const CALL_SITE_TINT: f32 = 0.5;

/// The line to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub file: PathBuf,
    /// 1-based.
    pub line: u32,
    /// The frame the program is stopped in (not a caller).
    pub innermost: bool,
    pub func: String,
}

/// What a click in the pane asks the host to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeAction {
    ToggleBreakpoint(PathBuf, u32),
    Open(PathBuf, u32),
}

#[derive(Default)]
pub struct CodePane {
    rect: RectF,
    body: RectF,
    gutter_w: f32,
    line_h: f32,
    char_w: f32,
    text_dy: f32,
    target: Option<Target>,
    lines: Vec<String>,
    /// Why the file could not be shown.
    error: Option<String>,
    /// The file read into `lines` and its modification time then.
    loaded: Option<(PathBuf, Option<SystemTime>)>,
    /// Sorted, 1-based.
    breakpoints: Vec<u32>,
    scroll: f32,
    hscroll: f32,
    /// Scroll the target line into the middle at the next layout.
    recenter: bool,
    /// The program is running: the line is from the last stop.
    stale: bool,
}

impl CodePane {
    /// The frame to show; called before each layout. A new file or line
    /// is scrolled into view.
    pub fn show(&mut self, target: Option<Target>) {
        if target == self.target {
            return;
        }
        let same_file = self.target.as_ref().map(|t| &t.file) == target.as_ref().map(|t| &t.file);
        self.target = target;
        if !same_file {
            self.load();
            self.hscroll = 0.0;
        }
        self.recenter = true;
    }

    /// A new stop: re-reads the file if it changed and scrolls the line
    /// back into view.
    pub fn stopped(&mut self) {
        let fresh = match (&self.loaded, &self.target) {
            (Some((path, at)), Some(t)) => *path == t.file && *at == modified(path),
            _ => false,
        };
        if !fresh {
            self.load();
        }
        self.recenter = true;
    }

    pub fn clear(&mut self) {
        *self = Self {
            rect: self.rect,
            body: self.body,
            gutter_w: self.gutter_w,
            line_h: self.line_h,
            char_w: self.char_w,
            text_dy: self.text_dy,
            ..Self::default()
        };
    }

    /// The file shown, for its breakpoints.
    pub fn path(&self) -> Option<&Path> {
        self.target.as_ref().map(|t| t.file.as_path())
    }

    pub fn set_breakpoints(&mut self, lines: Vec<u32>) {
        self.breakpoints = lines;
    }

    pub fn set_stale(&mut self, stale: bool) {
        self.stale = stale;
    }

    fn load(&mut self) {
        self.lines.clear();
        self.error = None;
        self.loaded = None;
        let Some(t) = &self.target else {
            return;
        };
        if t.file.as_os_str().is_empty() {
            self.error = Some(format!("No source for {} (no debug info)", t.func));
            return;
        }
        match read_lines(&t.file) {
            Ok(lines) => {
                self.lines = lines;
                self.loaded = Some((t.file.clone(), modified(&t.file)));
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn place(&mut self, rect: RectF, text: &TextSystem) {
        self.rect = rect;
        self.body = rect.split_top(HEADER_H).1;
        let mono = text.line_height(TextStyle::Mono);
        self.line_h = (mono * 1.35).round();
        self.text_dy = ((self.line_h - mono) / 2.0).round();
        self.char_w = text.cell_size().w;
        let digits = self.lines.len().to_string().len().max(3) as f32;
        self.gutter_w = digits * self.char_w + 2.0 * GUTTER_PAD;
        if self.recenter && !self.body.is_empty() {
            self.recenter = false;
            if let Some(t) = &self.target {
                let y = (t.line.max(1) - 1) as f32 * self.line_h;
                self.scroll = y - (self.body.h - self.line_h) / 2.0;
            }
        }
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        let max = (self.lines.len() as f32 * self.line_h - self.body.h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
        let longest = self
            .lines
            .iter()
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(0);
        let text_w = self.body.w - self.gutter_w - PAD;
        let hmax = (longest as f32 * self.char_w - text_w).max(0.0);
        self.hscroll = self.hscroll.clamp(0.0, hmax);
    }

    /// The 1-based line under `p` and whether it is in the gutter.
    fn line_at(&self, p: PointF) -> Option<(u32, bool)> {
        if !self.body.contains(p) || self.line_h <= 0.0 {
            return None;
        }
        let i = ((p.y - self.body.y + self.scroll) / self.line_h) as usize;
        (i < self.lines.len()).then_some((i as u32 + 1, p.x < self.body.x + self.gutter_w))
    }

    /// Handles an event over the pane. Returns whether to repaint and
    /// what the host should do.
    fn handle(&mut self, e: &Event) -> (bool, Option<CodeAction>) {
        match *e {
            Event::MouseDown {
                pos,
                button: MouseButton::Left,
                clicks,
                ..
            } => {
                let (Some((line, gutter)), Some(t)) = (self.line_at(pos), &self.target) else {
                    return (false, None);
                };
                let file = t.file.clone();
                match (gutter, clicks >= 2) {
                    (true, _) => (true, Some(CodeAction::ToggleBreakpoint(file, line))),
                    (false, true) => (true, Some(CodeAction::Open(file, line))),
                    (false, false) => (false, None),
                }
            }
            Event::Wheel {
                lines,
                horizontal,
                mods,
                ..
            } => {
                let before = (self.scroll, self.hscroll);
                if horizontal || mods.shift {
                    self.hscroll -= lines * 6.0 * self.char_w;
                } else {
                    self.scroll -= lines * 3.0 * self.line_h;
                }
                self.clamp_scroll();
                ((self.scroll, self.hscroll) != before, None)
            }
            _ => (false, None),
        }
    }

    fn draw(&self, d: &mut DrawCtx, theme: &Theme) {
        let stale = self.stale;
        let r = self.rect;
        d.fill_rect(r, theme.editor_bg);
        let a = if stale { STALE } else { 1.0 };

        // Header: file name, and the frame when it is a caller.
        d.fill_rect(RectF::new(r.x, r.y, r.w, HEADER_H), theme.panel_bg);
        d.fill_rect(
            RectF::new(r.x, r.y + HEADER_H - 1.0, r.w, 1.0),
            theme.border,
        );
        let header = RectF::new(r.x + PAD, r.y, r.w - 2.0 * PAD, HEADER_H);
        let title = match &self.target {
            Some(t) => t.file.file_name().map_or_else(
                || t.file.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            ),
            None => "SOURCE".into(),
        };
        let tw = d.text_system().measure(TextStyle::UiBold, &title);
        d.text(TextStyle::UiBold, &title, header, theme.text.with_alpha(a));
        if let Some(t) = &self.target {
            let what = if t.innermost {
                format!("line {}", t.line)
            } else {
                format!("{} \u{b7} called at line {}", t.func, t.line)
            };
            let w = d.text_system().measure(TextStyle::UiSmall, &what);
            let x = (header.right() - w).max(header.x + tw + 12.0);
            let at = RectF::new(x, header.y, header.right() - x + 2.0, header.h);
            d.text(TextStyle::UiSmall, &what, at, theme.text_dim.with_alpha(a));
        }

        let body = self.body;
        if self.lines.is_empty() {
            let msg = match (&self.target, &self.error) {
                (_, Some(e)) => e.as_str(),
                (None, _) => "The stopped line shows here.",
                (Some(_), None) => "Empty file",
            };
            let at = RectF::new(body.x + PAD, body.y + 6.0, body.w - 2.0 * PAD, 22.0);
            d.text(TextStyle::Ui, msg, at, theme.text_dim);
            return;
        }

        let lh = self.line_h;
        let first = (self.scroll / lh) as usize;
        let count = (body.h / lh) as usize + 2;
        let marked = self.target.as_ref().map(|t| (t.line, t.innermost));
        let gutter = RectF::new(body.x, body.y, self.gutter_w, body.h);
        let text_rect = RectF::new(gutter.right(), body.y, body.w - gutter.w, body.h);
        d.clip(body, |d| {
            for i in first..(first + count).min(self.lines.len()) {
                let line = i as u32 + 1;
                let y = body.y + i as f32 * lh - self.scroll;
                let cy = y + lh / 2.0;
                if let Some((_, innermost)) = marked.filter(|&(m, _)| m == line) {
                    let tint = if innermost {
                        theme.exec_line_bg
                    } else {
                        let c = theme.exec_line_bg;
                        c.with_alpha(c.a * CALL_SITE_TINT)
                    };
                    d.fill_rect(RectF::new(body.x, y, body.w, lh), tint);
                    // The editor's chevron; hollow (one stroke, dim) for a
                    // caller.
                    let x = body.x + 15.0;
                    let c = if innermost {
                        theme.accent.with_alpha(a)
                    } else {
                        theme.accent.with_alpha(0.5 * a)
                    };
                    d.line(PointF::new(x, cy - 4.5), PointF::new(x + 5.0, cy), c, 2.0);
                    d.line(PointF::new(x + 5.0, cy), PointF::new(x, cy + 4.5), c, 2.0);
                }
                if self.breakpoints.binary_search(&line).is_ok() {
                    let dot = 9.0f32.min(lh - 4.0);
                    d.fill_round_rect(
                        RectF::new(body.x + 4.0, cy - dot / 2.0, dot, dot),
                        dot / 2.0,
                        theme.breakpoint,
                    );
                }
                let num = line.to_string();
                let x = gutter.right() - GUTTER_PAD - num.len() as f32 * self.char_w;
                let color = if marked.is_some_and(|(m, _)| m == line) {
                    theme.gutter_text_active
                } else {
                    theme.gutter_text
                };
                d.text_at(
                    TextStyle::Mono,
                    &num,
                    PointF::new(x, y + self.text_dy),
                    color,
                );
            }
            d.clip(text_rect, |d| {
                for i in first..(first + count).min(self.lines.len()) {
                    let y = body.y + i as f32 * lh - self.scroll;
                    let x = text_rect.x + 4.0 - self.hscroll;
                    d.text_at(
                        TextStyle::Mono,
                        &self.lines[i],
                        PointF::new(x, y + self.text_dy),
                        theme.text.with_alpha(a),
                    );
                }
            });
        });
    }
}

impl Panel for CodePane {
    fn layout(&mut self, rect: RectF, text: &TextSystem) {
        self.place(rect, text);
    }

    fn paint(&mut self, d: &mut DrawCtx, theme: &Theme, _focused: bool) {
        self.draw(d, theme);
    }

    fn on_event(&mut self, e: &Event, cx: &mut Cx) -> bool {
        let (repaint, action) = self.handle(e);
        if repaint {
            cx.repaint();
        }
        match action {
            Some(CodeAction::ToggleBreakpoint(path, line)) => {
                cx.act(Action::ToggleBreakpoint { path, line });
            }
            Some(CodeAction::Open(path, line)) => cx.act(Action::OpenSource { path, line }),
            None => {}
        }
        true
    }
}

fn modified(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// The file's lines with tabs expanded.
fn read_lines(p: &Path) -> Result<Vec<String>, String> {
    let meta = std::fs::metadata(p).map_err(|_| format!("Source not found: {}", p.display()))?;
    if meta.len() > MAX_BYTES {
        return Err(format!("{} is too large to show", p.display()));
    }
    let bytes = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
    Ok(split_lines(&String::from_utf8_lossy(&bytes)))
}

fn split_lines(s: &str) -> Vec<String> {
    s.lines().map(expand_tabs).collect()
}

fn expand_tabs(line: &str) -> String {
    if !line.contains('\t') {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len() + 8);
    let mut col = 0;
    for ch in line.chars() {
        if ch == '\t' {
            let n = TAB - col % TAB;
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            out.push(ch);
            col += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Modifiers;

    fn pane(lines: usize, line: u32) -> CodePane {
        let mut p = CodePane {
            target: Some(Target {
                file: PathBuf::from("m.cpp"),
                line,
                innermost: true,
                func: "main".into(),
            }),
            lines: (1..=lines).map(|i| format!("line {i}")).collect(),
            line_h: 20.0,
            char_w: 8.0,
            gutter_w: 50.0,
            recenter: true,
            ..CodePane::default()
        };
        p.rect = RectF::new(0.0, 0.0, 400.0, HEADER_H + 200.0);
        p.body = p.rect.split_top(HEADER_H).1;
        p
    }

    /// `layout` without a text system: recentre and clamp.
    fn settle(p: &mut CodePane) {
        if p.recenter {
            p.recenter = false;
            let t = p.target.as_ref().unwrap();
            p.scroll = (t.line - 1) as f32 * p.line_h - (p.body.h - p.line_h) / 2.0;
        }
        p.clamp_scroll();
    }

    fn click(x: f32, y: f32, clicks: u8) -> Event {
        Event::MouseDown {
            pos: PointF::new(x, y),
            button: MouseButton::Left,
            mods: Modifiers::default(),
            clicks,
        }
    }

    #[test]
    fn the_line_is_centred_and_clamped() {
        let mut p = pane(100, 50);
        settle(&mut p);
        // 10 lines visible; line 50 starts at 980, centred at 980 - 90.
        assert_eq!(p.scroll, 890.0);
        let mut p = pane(100, 2);
        settle(&mut p);
        assert_eq!(p.scroll, 0.0, "near the top it cannot centre");
        let mut p = pane(100, 100);
        settle(&mut p);
        assert_eq!(p.scroll, 100.0 * 20.0 - 200.0);
    }

    #[test]
    fn gutter_toggles_and_double_click_opens() {
        let mut p = pane(100, 1);
        settle(&mut p);
        let row3 = HEADER_H + 2.5 * 20.0;
        assert_eq!(
            p.handle(&click(10.0, row3, 1)),
            (true, Some(CodeAction::ToggleBreakpoint("m.cpp".into(), 3)))
        );
        assert_eq!(p.handle(&click(200.0, row3, 1)), (false, None));
        assert_eq!(
            p.handle(&click(200.0, row3, 2)),
            (true, Some(CodeAction::Open("m.cpp".into(), 3)))
        );
        assert_eq!(p.handle(&click(200.0, 5.0, 2)), (false, None), "header");
    }

    #[test]
    fn a_new_target_line_recentres() {
        let mut p = pane(100, 50);
        settle(&mut p);
        let t = Target {
            line: 80,
            ..p.target.clone().unwrap()
        };
        p.show(Some(t));
        assert!(p.recenter);
        // Same target again: the user's scroll is kept.
        p.recenter = false;
        p.show(p.target.clone());
        assert!(!p.recenter);
    }

    #[test]
    fn tabs_expand_to_stops() {
        assert_eq!(expand_tabs("\tx"), "    x");
        assert_eq!(expand_tabs("ab\tc"), "ab  c");
        assert_eq!(split_lines("a\r\nb\n"), ["a", "b"]);
    }
}
