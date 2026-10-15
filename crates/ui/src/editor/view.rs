//! One open document: buffer + undo + caret/selection + scroll, with input
//! handling and painting. Line layouts are cached by line id (ids change on
//! every edit) and evicted when the line scrolls out of view.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::buffer::{byte_to_utf16, order, utf16_to_byte, Buffer, Pos};
use super::undo::{Edit, EditKind, UndoStack};
use crate::event::{CursorShape, Event, Key, Modifiers, MouseButton};
use crate::geom::{PointF, RectF};
use crate::panel::Cx;
use crate::sys::clipboard;
use crate::sys::gfx::DrawCtx;
use crate::sys::text::{TextLayout, TextStyle, TextSystem};
use crate::theme::Theme;

/// Files larger than this are refused rather than loaded.
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const TEXT_PAD: f32 = 6.0;
const GUTTER_PAD: f32 = 14.0;
const TAB_WIDTH: usize = 4;
const WHEEL_LINES: f32 = 3.0;

pub struct Document {
    path: PathBuf,
    name: String,
    buffer: Buffer,
    undo: UndoStack,
    saved_state: u64,
    read_only: bool,
    caret: Pos,
    anchor: Pos,
    /// Remembered x for vertical movement.
    preferred_x: Option<f32>,
    scroll: PointF,
    rect: RectF,
    text_rect: RectF,
    gutter_w: f32,
    line_h: f32,
    layouts: HashMap<u64, TextLayout>,
    dragging: bool,
}

impl Document {
    pub fn open(path: &Path) -> Result<Self, String> {
        let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if meta.len() > MAX_FILE_BYTES {
            return Err(format!(
                "{}: file is larger than {} MB",
                path.display(),
                MAX_FILE_BYTES / (1024 * 1024)
            ));
        }
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let (text, read_only) = match String::from_utf8(bytes) {
            Ok(s) => (s, false),
            Err(e) => (String::from_utf8_lossy(e.as_bytes()).into_owned(), true),
        };
        Ok(Self::from_text(path, &text, read_only))
    }

    fn from_text(path: &Path, text: &str, read_only: bool) -> Self {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        Self {
            path: path.to_path_buf(),
            name,
            buffer: Buffer::from_text(text),
            undo: UndoStack::default(),
            saved_state: 0,
            read_only,
            caret: Pos::default(),
            anchor: Pos::default(),
            preferred_x: None,
            scroll: PointF::default(),
            rect: RectF::default(),
            text_rect: RectF::default(),
            gutter_w: 0.0,
            line_h: 18.0,
            layouts: HashMap::new(),
            dragging: false,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is_dirty(&self) -> bool {
        self.undo.state_id() != self.saved_state
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    pub fn save(&mut self) -> Result<(), String> {
        if self.read_only {
            return Err(format!(
                "{}: opened read-only (not valid UTF-8)",
                self.path.display()
            ));
        }
        std::fs::write(&self.path, self.buffer.to_file_text())
            .map_err(|e| format!("{}: {e}", self.path.display()))?;
        self.saved_state = self.undo.state_id();
        self.undo.seal();
        Ok(())
    }

    /// Puts the caret at the start of `line` (1-based) and scrolls it to
    /// about a third of the way down the view.
    pub fn goto_line(&mut self, line: u32) {
        let last = self.buffer.line_count().saturating_sub(1);
        let line = (line.max(1) as usize - 1).min(last);
        self.caret = Pos::new(line, 0);
        self.anchor = self.caret;
        self.preferred_x = None;
        self.scroll = PointF::new(0.0, line as f32 * self.line_h - self.text_rect.h / 3.0);
        self.clamp_scroll();
    }

    /// Drops cached layouts (DPI change).
    pub fn clear_layouts(&mut self) {
        self.layouts.clear();
    }

    pub fn layout(&mut self, rect: RectF, text: &TextSystem) {
        self.rect = rect;
        self.line_h = (text.line_height(TextStyle::Mono) * 1.35).round();
        let digits = self.buffer.line_count().to_string().len().max(3) as f32;
        self.gutter_w = digits * text.cell_size().w + 2.0 * GUTTER_PAD;
        let (_, text_rect) = rect.split_left(self.gutter_w);
        self.text_rect = text_rect;
        self.clamp_scroll();
    }

    // ---- geometry -------------------------------------------------------

    fn line_layout(&mut self, text: &TextSystem, line: usize) -> Option<&TextLayout> {
        let id = self.buffer.line_id(line);
        if !self.layouts.contains_key(&id) {
            let layout = text.layout(TextStyle::Mono, self.buffer.line(line)).ok()?;
            self.layouts.insert(id, layout);
        }
        self.layouts.get(&id)
    }

    fn x_of(&mut self, text: &TextSystem, p: Pos) -> f32 {
        let u16_col = byte_to_utf16(self.buffer.line(p.line), p.col);
        self.line_layout(text, p.line)
            .map_or(0.0, |l| l.x_of(u16_col))
    }

    fn col_at_x(&mut self, text: &TextSystem, line: usize, x: f32) -> usize {
        let u16_col = self.line_layout(text, line).map_or(0, |l| l.hit_test_x(x));
        utf16_to_byte(self.buffer.line(line), u16_col)
    }

    fn pos_at_point(&mut self, text: &TextSystem, p: PointF) -> Pos {
        let y = p.y - self.text_rect.y + self.scroll.y;
        let line = if y < 0.0 {
            0
        } else {
            ((y / self.line_h) as usize).min(self.buffer.line_count() - 1)
        };
        let x = p.x - self.text_rect.x - TEXT_PAD + self.scroll.x;
        Pos::new(line, self.col_at_x(text, line, x.max(0.0)))
    }

    fn visible_lines(&self) -> usize {
        ((self.text_rect.h / self.line_h).floor() as usize).max(1)
    }

    fn clamp_scroll(&mut self) {
        let max_y = (self.buffer.line_count().saturating_sub(1)) as f32 * self.line_h;
        self.scroll.y = self.scroll.y.clamp(0.0, max_y.max(0.0));
        self.scroll.x = self.scroll.x.max(0.0);
    }

    fn ensure_caret_visible(&mut self, text: &TextSystem) {
        let y = self.caret.line as f32 * self.line_h;
        if y < self.scroll.y {
            self.scroll.y = y;
        } else if y + self.line_h > self.scroll.y + self.text_rect.h {
            self.scroll.y = y + self.line_h - self.text_rect.h;
        }
        let x = self.x_of(text, self.caret);
        let view_w = (self.text_rect.w - 2.0 * TEXT_PAD).max(0.0);
        let margin = (4.0 * text.cell_size().w).min(view_w / 3.0);
        if x < self.scroll.x + margin {
            self.scroll.x = (x - margin).max(0.0);
        } else if x > self.scroll.x + view_w - margin {
            self.scroll.x = x - view_w + margin;
        }
        self.clamp_scroll();
    }

    // ---- editing --------------------------------------------------------

    fn selection(&self) -> (Pos, Pos) {
        order(self.anchor, self.caret)
    }

    fn has_selection(&self) -> bool {
        self.anchor != self.caret
    }

    /// Replaces `start..end` with `insert` and records it for undo.
    fn replace(&mut self, start: Pos, end: Pos, insert: &str, kind: EditKind, cx: &mut Cx) {
        if self.read_only {
            cx.log_error(format!("{} is read-only", self.name));
            return;
        }
        let before = (self.caret, self.anchor);
        let (start, end) = order(start, end);
        let removed = self.buffer.delete(start, end);
        let new_end = self.buffer.insert(start, insert);
        if removed.is_empty() && insert.is_empty() {
            return;
        }
        self.caret = new_end;
        self.anchor = new_end;
        self.preferred_x = None;
        self.undo.record(
            Edit {
                start,
                removed,
                inserted: super::buffer::normalize_newlines(insert),
            },
            kind,
            before,
            (self.caret, self.anchor),
        );
        self.ensure_caret_visible(cx.text);
        cx.repaint();
    }

    fn replace_selection(&mut self, insert: &str, kind: EditKind, cx: &mut Cx) {
        let (s, e) = self.selection();
        self.replace(s, e, insert, kind, cx);
    }

    fn move_caret(&mut self, to: Pos, extend: bool, cx: &mut Cx) {
        self.caret = self.buffer.clamp(to);
        if !extend {
            self.anchor = self.caret;
        }
        self.ensure_caret_visible(cx.text);
        cx.repaint();
    }

    fn vertical(&mut self, delta: isize, extend: bool, cx: &mut Cx) {
        let x = match self.preferred_x {
            Some(x) => x,
            None => self.x_of(cx.text, self.caret),
        };
        let last = self.buffer.line_count() as isize - 1;
        let target = self.caret.line as isize + delta;
        let to = if target < 0 {
            Pos::new(0, 0)
        } else if target > last {
            self.buffer.end()
        } else {
            let line = target as usize;
            Pos::new(line, self.col_at_x(cx.text, line, x))
        };
        self.move_caret(to, extend, cx);
        self.preferred_x = Some(x);
    }

    fn undo_redo(&mut self, redo: bool, cx: &mut Cx) {
        let sel = if redo {
            self.undo.redo(&mut self.buffer)
        } else {
            self.undo.undo(&mut self.buffer)
        };
        if let Some((caret, anchor)) = sel {
            self.caret = self.buffer.clamp(caret);
            self.anchor = self.buffer.clamp(anchor);
            self.preferred_x = None;
            self.ensure_caret_visible(cx.text);
            cx.repaint();
        }
    }

    fn copy(&self, cx: &mut Cx) -> bool {
        if !self.has_selection() {
            return false;
        }
        let (s, e) = self.selection();
        let text = self.buffer.text_range(s, e).replace('\n', "\r\n");
        if let Err(err) = clipboard::set_text(cx.window, &text) {
            cx.log_error(format!("Copy failed: {err}"));
            return false;
        }
        true
    }

    fn on_key(&mut self, key: Key, mods: Modifiers, cx: &mut Cx) -> bool {
        let extend = mods.shift;
        match key {
            Key::Left if mods.ctrl => {
                self.move_caret(self.buffer.word_left(self.caret), extend, cx)
            }
            Key::Right if mods.ctrl => {
                self.move_caret(self.buffer.word_right(self.caret), extend, cx);
            }
            Key::Left => {
                let to = if self.has_selection() && !extend {
                    self.selection().0
                } else {
                    self.buffer.prev_pos(self.caret)
                };
                self.move_caret(to, extend, cx);
            }
            Key::Right => {
                let to = if self.has_selection() && !extend {
                    self.selection().1
                } else {
                    self.buffer.next_pos(self.caret)
                };
                self.move_caret(to, extend, cx);
            }
            Key::Up => self.vertical(-1, extend, cx),
            Key::Down => self.vertical(1, extend, cx),
            Key::PageUp | Key::PageDown => {
                let page = self.visible_lines().saturating_sub(1).max(1);
                let delta = if key == Key::PageUp {
                    -(page as isize)
                } else {
                    page as isize
                };
                self.scroll.y += delta as f32 * self.line_h;
                self.vertical(delta, extend, cx);
            }
            Key::Home if mods.ctrl => self.move_caret(Pos::new(0, 0), extend, cx),
            Key::End if mods.ctrl => self.move_caret(self.buffer.end(), extend, cx),
            Key::Home => {
                let indent = self.buffer.indent_len(self.caret.line);
                let col = if self.caret.col == indent { 0 } else { indent };
                self.move_caret(Pos::new(self.caret.line, col), extend, cx);
            }
            Key::End => {
                let line = self.caret.line;
                self.move_caret(Pos::new(line, self.buffer.line_len(line)), extend, cx);
            }
            Key::Backspace => {
                if self.has_selection() {
                    self.replace_selection("", EditKind::Other, cx);
                } else {
                    let from = if mods.ctrl {
                        self.buffer.word_left(self.caret)
                    } else {
                        self.buffer.prev_pos(self.caret)
                    };
                    self.replace(from, self.caret, "", EditKind::Backspace, cx);
                }
            }
            Key::Delete => {
                if self.has_selection() {
                    self.replace_selection("", EditKind::Other, cx);
                } else {
                    let to = if mods.ctrl {
                        self.buffer.word_right(self.caret)
                    } else {
                        self.buffer.next_pos(self.caret)
                    };
                    self.replace(self.caret, to, "", EditKind::DeleteForward, cx);
                }
            }
            Key::Enter => {
                let (start, _) = self.selection();
                let indent_len = self.buffer.indent_len(start.line).min(start.col);
                let indent = self.buffer.line(start.line)[..indent_len].to_owned();
                self.replace_selection(&format!("\n{indent}"), EditKind::Other, cx);
            }
            Key::Tab if !mods.ctrl => {
                let chars = self.buffer.line(self.caret.line)[..self.caret.col]
                    .chars()
                    .count();
                let n = TAB_WIDTH - chars % TAB_WIDTH;
                self.replace_selection(&" ".repeat(n), EditKind::Typing, cx);
            }
            Key::Letter('A') if mods.ctrl => {
                self.anchor = Pos::new(0, 0);
                self.caret = self.buffer.end();
                cx.repaint();
            }
            Key::Letter('C') if mods.ctrl => {
                self.copy(cx);
            }
            Key::Letter('X') if mods.ctrl => {
                if !self.read_only && self.copy(cx) {
                    self.replace_selection("", EditKind::Other, cx);
                }
            }
            Key::Letter('V') if mods.ctrl => {
                if let Some(text) = clipboard::get_text(cx.window) {
                    self.replace_selection(&text, EditKind::Other, cx);
                }
            }
            Key::Letter('Z') if mods.ctrl => self.undo_redo(mods.shift, cx),
            Key::Letter('Y') if mods.ctrl => self.undo_redo(true, cx),
            Key::Letter('S') if mods.ctrl => match self.save() {
                Ok(()) => {
                    cx.log(format!("Saved {}", self.path.display()));
                    cx.repaint();
                }
                Err(e) => cx.log_error(format!("Save failed: {e}")),
            },
            _ => return false,
        }
        if !matches!(key, Key::Up | Key::Down | Key::PageUp | Key::PageDown) {
            self.preferred_x = None;
        }
        true
    }

    pub fn on_event(&mut self, e: &Event, cx: &mut Cx) -> bool {
        match *e {
            Event::KeyDown { key, mods } => self.on_key(key, mods, cx),
            Event::Char(c) if !c.is_control() => {
                let mut buf = [0u8; 4];
                self.replace_selection(c.encode_utf8(&mut buf), EditKind::Typing, cx);
                true
            }
            Event::MouseDown {
                pos,
                button: MouseButton::Left,
                mods,
                clicks,
            } if self.rect.contains(pos) => {
                let p = self.pos_at_point(cx.text, pos);
                if clicks >= 2 {
                    let (s, e) = self.buffer.word_at(p);
                    self.anchor = s;
                    self.caret = e;
                } else {
                    self.caret = p;
                    if !mods.shift {
                        self.anchor = p;
                    }
                }
                self.preferred_x = None;
                self.dragging = true;
                cx.repaint();
                true
            }
            Event::MouseMove { pos, left_down, .. } => {
                if self.dragging && left_down {
                    if pos.y < self.text_rect.y {
                        self.scroll.y -= self.line_h;
                    } else if pos.y > self.text_rect.bottom() {
                        self.scroll.y += self.line_h;
                    }
                    self.clamp_scroll();
                    self.caret = self.pos_at_point(cx.text, pos);
                    cx.repaint();
                }
                if self.text_rect.contains(pos) || self.dragging {
                    cx.cursor = CursorShape::IBeam;
                }
                true
            }
            Event::MouseUp {
                button: MouseButton::Left,
                ..
            } => {
                self.dragging = false;
                true
            }
            Event::Wheel {
                lines,
                horizontal,
                mods,
                ..
            } => {
                if horizontal || mods.shift {
                    self.scroll.x -= lines * WHEEL_LINES * cx.text.cell_size().w * 2.0;
                } else {
                    self.scroll.y -= lines * WHEEL_LINES * self.line_h;
                }
                self.clamp_scroll();
                cx.repaint();
                true
            }
            _ => false,
        }
    }

    // ---- painting -------------------------------------------------------

    pub fn paint(&mut self, d: &mut DrawCtx, theme: &Theme, focused: bool) {
        let text = d.text_system();
        let rect = self.rect;
        d.fill_rect(rect, theme.editor_bg);
        let first = (self.scroll.y / self.line_h).floor() as usize;
        let count = (rect.h / self.line_h).ceil() as usize + 1;
        let last = (first + count).min(self.buffer.line_count());
        let cell = text.cell_size();
        let text_dy = ((self.line_h - text.line_height(TextStyle::Mono)) / 2.0).round();
        let (sel_start, sel_end) = self.selection();
        let origin_x = self.text_rect.x + TEXT_PAD - self.scroll.x;

        // Resolve all geometry first (needs &mut self for the cache).
        let mut rows = Vec::with_capacity(last.saturating_sub(first));
        for line in first..last {
            // Builds (or reuses) the layout drawn below.
            self.line_layout(text, line);
            let y = self.text_rect.y + line as f32 * self.line_h - self.scroll.y;
            let sel = if self.has_selection() && line >= sel_start.line && line <= sel_end.line {
                let x0 = if line == sel_start.line {
                    self.x_of(text, sel_start)
                } else {
                    0.0
                };
                let x1 = if line == sel_end.line {
                    self.x_of(text, sel_end)
                } else {
                    self.line_layout(text, line).map_or(0.0, TextLayout::width) + cell.w * 0.6
                };
                Some((x0, x1))
            } else {
                None
            };
            rows.push((line, y, sel));
        }
        let caret_x = self.x_of(text, self.caret);
        let caret_line = self.caret.line;
        let has_selection = self.has_selection();

        d.clip(rect, |d| {
            // Gutter.
            let gutter = RectF::new(rect.x, rect.y, self.gutter_w, rect.h);
            d.fill_rect(gutter, theme.editor_bg);
            for &(line, y, _) in &rows {
                let num = (line + 1).to_string();
                let x = gutter.right() - GUTTER_PAD - num.len() as f32 * cell.w;
                let color = if line == caret_line {
                    theme.gutter_text_active
                } else {
                    theme.gutter_text
                };
                d.text_at(TextStyle::Mono, &num, PointF::new(x, y + text_dy), color);
            }

            d.clip(self.text_rect, |d| {
                for &(line, y, sel) in &rows {
                    let row = RectF::new(self.text_rect.x, y, self.text_rect.w, self.line_h);
                    if line == caret_line && !has_selection && focused {
                        d.fill_rect(row, theme.current_line_bg);
                    }
                    if let Some((x0, x1)) = sel {
                        let color = if focused {
                            theme.selection_bg
                        } else {
                            theme.selection_inactive_bg
                        };
                        d.fill_rect(RectF::new(origin_x + x0, y, x1 - x0, self.line_h), color);
                    }
                    let id = self.buffer.line_id(line);
                    if let Some(layout) = self.layouts.get(&id) {
                        d.draw_layout(layout, PointF::new(origin_x, y + text_dy), theme.text);
                    }
                }
                if focused {
                    let y = self.text_rect.y + caret_line as f32 * self.line_h - self.scroll.y;
                    d.fill_rect(
                        RectF::new(origin_x + caret_x - 1.0, y, 2.0, self.line_h),
                        theme.caret,
                    );
                }
            });
            self.paint_scrollbar(d, theme);
        });

        // Evict layouts for lines no longer visible.
        let visible: Vec<u64> = (first..last).map(|l| self.buffer.line_id(l)).collect();
        self.layouts.retain(|id, _| visible.contains(id));
    }

    fn paint_scrollbar(&self, d: &mut DrawCtx, theme: &Theme) {
        let content =
            self.buffer.line_count() as f32 * self.line_h + self.text_rect.h - self.line_h;
        if content <= self.text_rect.h || self.text_rect.h <= 0.0 {
            return;
        }
        let track = RectF::new(self.rect.right() - 10.0, self.rect.y, 10.0, self.rect.h);
        let h = (track.h * self.text_rect.h / content).max(20.0);
        let y = track.y
            + (track.h - h) * (self.scroll.y / (content - self.text_rect.h)).clamp(0.0, 1.0);
        d.fill_rect(RectF::new(track.x + 2.0, y, 6.0, h), theme.scrollbar_thumb);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_utf8_opens_read_only() {
        let dir = std::env::temp_dir().join(format!("lattice-doc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bin.dat");
        std::fs::write(&path, [0x66, 0x6f, 0xff, 0x6f]).unwrap();
        let mut doc = Document::open(&path).unwrap();
        assert!(doc.is_read_only());
        assert!(doc.save().is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_round_trips_and_clears_dirty() {
        let dir = std::env::temp_dir().join(format!("lattice-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        std::fs::write(&path, "one\r\ntwo\r\n").unwrap();
        let mut doc = Document::open(&path).unwrap();
        assert!(!doc.is_dirty());
        let end = doc.buffer.insert(Pos::new(1, 3), "!");
        doc.undo.record(
            Edit {
                start: Pos::new(1, 3),
                removed: String::new(),
                inserted: "!".into(),
            },
            EditKind::Typing,
            (Pos::new(1, 3), Pos::new(1, 3)),
            (end, end),
        );
        assert!(doc.is_dirty());
        doc.save().unwrap();
        assert!(!doc.is_dirty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\r\ntwo!\r\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
