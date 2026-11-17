//! One open document: buffer + undo + caret/selection + scroll, with input
//! handling and painting. Line layouts are cached by line id (ids change on
//! every edit) and evicted when the line scrolls out of view.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use super::buffer::{byte_to_utf16, order, utf16_to_byte, Buffer, Pos};
use super::complete;
use super::undo::{Edit, EditKind, UndoStack};
use crate::event::{CursorShape, Event, Key, Modifiers, MouseButton};
use crate::geom::{PointF, RectF};
use crate::memory;
use crate::menubar::Entry;
use crate::panel::{Action, Cx};
use crate::syntax::{self, is_word, Kind};
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

/// One coloured token of a line.
struct Run {
    x: f32,
    kind: Kind,
    layout: TextLayout,
}

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
    /// C or C++ source: coloured, and completes as you type.
    code: bool,
    /// Whether each line starts inside a block comment; a prefix of the
    /// file, extended as lines are needed and cut back by edits.
    in_comment: Vec<bool>,
    /// Token layouts by line id, with the comment state they were made for
    /// (an edit further up can change it without touching the line).
    runs: HashMap<u64, (bool, Vec<Run>)>,
    /// The words of the file, for completion; `None` when out of date.
    words: Option<BTreeSet<String>>,
    /// Where a word was last being typed: that word, unfinished, isn't one
    /// to offer.
    typed_at: Option<Pos>,
    /// What the file includes, and the scan for an update while one runs.
    included: BTreeSet<String>,
    included_scan: Option<Receiver<BTreeSet<String>>>,
    dragging: bool,
    /// Breakpoint lines (1-based), drawn as gutter dots.
    breakpoints: Vec<u32>,
    /// The line the debugger is stopped on (1-based).
    exec_line: Option<u32>,
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
        let mut doc = Self::from_text(path, &text, read_only);
        doc.scan_includes();
        Ok(doc)
    }

    /// Reads what the saved file includes, in the background: the preprocessor
    /// takes a moment and typing must not wait for it.
    fn scan_includes(&mut self) {
        if !self.code {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let path = self.path.clone();
        std::thread::spawn(move || {
            // If the document moved on, nobody is listening; that is fine.
            let _ = tx.send(complete::included_words(&path));
        });
        self.included_scan = Some(rx);
    }

    pub(super) fn from_text(path: &Path, text: &str, read_only: bool) -> Self {
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
            code: syntax::is_c_like(path),
            in_comment: Vec::new(),
            runs: HashMap::new(),
            words: None,
            typed_at: None,
            included: BTreeSet::new(),
            included_scan: None,
            dragging: false,
            breakpoints: Vec::new(),
            exec_line: None,
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

    pub(super) fn selected_text(&self) -> String {
        let (start, end) = self.selection();
        self.buffer.text_range(start, end)
    }

    pub(super) fn can_undo(&self) -> bool {
        self.undo.can_undo()
    }

    pub(super) fn can_redo(&self) -> bool {
        self.undo.can_redo()
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
        // Its includes may have changed.
        self.scan_includes();
        Ok(())
    }

    /// Writes to `path`, which becomes this document's file.
    pub fn save_as(&mut self, path: &Path) -> Result<(), String> {
        if self.read_only {
            // The text shown is a lossy copy; writing it would corrupt.
            return Err(format!(
                "{}: opened read-only (not valid UTF-8)",
                self.path.display()
            ));
        }
        std::fs::write(path, self.buffer.to_file_text())
            .map_err(|e| format!("{}: {e}", path.display()))?;
        self.set_path(path);
        self.saved_state = self.undo.state_id();
        self.undo.seal();
        self.scan_includes();
        Ok(())
    }

    /// The file moved, or the document now belongs to another.
    pub fn set_path(&mut self, path: &Path) {
        self.path = path.to_path_buf();
        self.name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        // Its extension may say another language now.
        self.code = syntax::is_c_like(path);
        self.in_comment.clear();
        self.runs.clear();
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

    /// The caret's line (1-based).
    pub fn caret_line(&self) -> u32 {
        self.caret.line as u32 + 1
    }

    /// Debugger marks; returns `true` if they changed.
    pub fn set_marks(&mut self, breakpoints: Vec<u32>, exec_line: Option<u32>) -> bool {
        let changed = self.breakpoints != breakpoints || self.exec_line != exec_line;
        self.breakpoints = breakpoints;
        self.exec_line = exec_line;
        changed
    }

    /// Drops cached layouts (DPI change).
    pub fn clear_layouts(&mut self) {
        self.layouts.clear();
        self.runs.clear();
    }

    #[cfg(test)]
    pub fn text_for_test(&self) -> String {
        self.buffer.text_range(Pos::new(0, 0), self.buffer.end())
    }

    pub fn is_code(&self) -> bool {
        self.code
    }

    /// Whether `line` starts inside a block comment.
    fn comment_state(&mut self, line: usize) -> bool {
        if self.in_comment.is_empty() {
            self.in_comment.push(false);
        }
        while self.in_comment.len() <= line {
            let i = self.in_comment.len() - 1;
            let (_, open) = syntax::scan_line(self.buffer.line(i), self.in_comment[i]);
            self.in_comment.push(open);
        }
        self.in_comment[line]
    }

    /// Builds (or reuses) the coloured tokens of `line`.
    fn line_runs(&mut self, text: &TextSystem, line: usize) {
        let state = self.comment_state(line);
        let id = self.buffer.line_id(line);
        if self.runs.get(&id).is_some_and(|(s, _)| *s == state) {
            return;
        }
        let s = self.buffer.line(line).to_owned();
        let (spans, _) = syntax::scan_line(&s, state);
        let mut runs = Vec::with_capacity(spans.len());
        for span in spans {
            // Split at tabs: a tab stop in a token's own layout would count
            // from the token, not the line.
            let mut at = span.start;
            for piece in s[span.start..span.end].split('\t') {
                if !piece.trim().is_empty() {
                    let x = self.x_of(text, Pos::new(line, at));
                    if let Ok(layout) = text.layout(TextStyle::Mono, piece) {
                        runs.push(Run {
                            x,
                            kind: span.kind,
                            layout,
                        });
                    }
                }
                at += piece.len() + 1;
            }
        }
        self.runs.insert(id, (state, runs));
    }

    // ---- completion -----------------------------------------------------

    /// The word being typed: where it starts on the caret's line and its
    /// text. `None` where nothing should complete: a selection, a read-only
    /// file, a number, or inside a comment or string.
    pub fn completion_prefix(&mut self) -> Option<(usize, String)> {
        if self.read_only || self.has_selection() {
            return None;
        }
        let Pos { line, col } = self.caret;
        if self.code && col > 0 {
            let state = self.comment_state(line);
            let (spans, _) = syntax::scan_line(self.buffer.line(line), state);
            let inside = spans.iter().find(|s| s.start < col && col <= s.end);
            // Just past a closing quote is still "in" the string's span, and
            // not worth completing either.
            if inside.is_some_and(|s| matches!(s.kind, Kind::Comment | Kind::Str)) {
                return None;
            }
        }
        let text = &self.buffer.line(line)[..col];
        let start = text.bytes().rposition(|b| !is_word(b)).map_or(0, |i| i + 1);
        let prefix = &text[start..];
        if prefix.as_bytes().first().is_some_and(u8::is_ascii_digit) {
            return None;
        }
        Some((start, prefix.to_owned()))
    }

    /// The word being typed follows `#`, so it is a directive's name.
    pub fn is_directive(&self, start_col: usize) -> bool {
        self.code
            && self.buffer.line(self.caret.line)[..start_col]
                .trim()
                .ends_with('#')
    }

    /// Brings the word sets up to date; read them with [`Self::own_words`] and
    /// [`Self::included_words`].
    pub fn update_words(&mut self) {
        if let Some(words) = self.included_scan.as_ref().and_then(|r| r.try_recv().ok()) {
            self.included = words;
            self.included_scan = None;
        }
        if self.words.is_some() {
            return;
        }
        // Without the word still being typed, which isn't a word yet.
        let skip = self.typed_at.filter(|&at| at == self.caret).map(|at| {
            let line = self.buffer.line(at.line);
            let end = line[at.col..]
                .bytes()
                .position(|b| !is_word(b))
                .map_or(line.len(), |i| at.col + i);
            let start = line[..at.col]
                .bytes()
                .rposition(|b| !is_word(b))
                .map_or(0, |i| i + 1);
            (at.line, format!("{} {}", &line[..start], &line[end..]))
        });
        let buffer = &self.buffer;
        let lines = (0..buffer.line_count()).map(|i| match &skip {
            Some((line, text)) if *line == i => Cow::Borrowed(text.as_str()),
            _ => Cow::Borrowed(buffer.line(i)),
        });
        self.words = Some(if self.code {
            complete::code_words(lines)
        } else {
            complete::words_in(lines)
        });
    }

    pub fn own_words(&self) -> Option<&BTreeSet<String>> {
        self.words.as_ref()
    }

    pub fn included_words(&self) -> &BTreeSet<String> {
        &self.included
    }

    /// The caret's on-screen rect.
    pub fn caret_rect(&mut self, text: &TextSystem) -> RectF {
        let x = self.x_of(text, self.caret);
        RectF::new(
            self.text_rect.x + TEXT_PAD - self.scroll.x + x,
            self.text_rect.y + self.caret.line as f32 * self.line_h - self.scroll.y,
            2.0,
            self.line_h,
        )
    }

    /// Replaces the word at the caret, all of it: completing inside `fo|obar`
    /// must not leave the old `obar` behind.
    pub fn accept_completion(&mut self, start_col: usize, word: &str, cx: &mut Cx) {
        let Pos { line, col } = self.caret;
        let text = self.buffer.line(line);
        let end = text[col..]
            .bytes()
            .position(|b| !is_word(b))
            .map_or(text.len(), |i| col + i);
        self.replace(
            Pos::new(line, start_col),
            Pos::new(line, end),
            word,
            EditKind::Other,
            cx,
        );
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

    /// The member chain under `p` (see [`memory::expr_at`]) and the
    /// hovered identifier's on-screen rect.
    pub fn expr_at_point(&mut self, text: &TextSystem, p: PointF) -> Option<(Vec<String>, RectF)> {
        if !self.text_rect.contains(p) {
            return None;
        }
        let at = self.pos_at_point(text, p);
        let y = self.text_rect.y + at.line as f32 * self.line_h - self.scroll.y;
        let origin_x = self.text_rect.x + TEXT_PAD - self.scroll.x;
        let line = self.buffer.line(at.line).to_string();
        // The caret position nearest `p` can be just past the identifier.
        for col in [at.col, at.col.saturating_sub(1)] {
            let Some((path, span)) = memory::expr_at(&line, col) else {
                continue;
            };
            let x0 = origin_x + self.x_of(text, Pos::new(at.line, span.start));
            let x1 = origin_x + self.x_of(text, Pos::new(at.line, span.end));
            let r = RectF::new(x0, y, x1 - x0, self.line_h);
            if r.contains(p) {
                return Some((path, r));
            }
        }
        None
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

    pub(super) fn has_selection(&self) -> bool {
        self.anchor != self.caret
    }

    /// Replaces `start..end` with `insert` and records it for undo.
    fn replace(&mut self, start: Pos, end: Pos, insert: &str, kind: EditKind, cx: &mut Cx) {
        self.replace_with_selection(start, end, insert, kind, None, cx);
    }

    /// Replaces text and optionally restores a selection in the new buffer.
    fn replace_with_selection(
        &mut self,
        start: Pos,
        end: Pos,
        insert: &str,
        kind: EditKind,
        selection_after: Option<(Pos, Pos)>,
        cx: &mut Cx,
    ) {
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
        (self.caret, self.anchor) = selection_after
            .map(|(caret, anchor)| (self.buffer.clamp(caret), self.buffer.clamp(anchor)))
            .unwrap_or((new_end, new_end));
        self.preferred_x = None;
        self.in_comment.truncate(start.line + 1);
        // While a word is being typed its letters add nothing worth offering.
        let in_word = kind == EditKind::Typing && insert.bytes().all(is_word);
        if !in_word {
            self.words = None;
        }
        self.typed_at = (in_word || kind == EditKind::Backspace).then_some(new_end);
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

    fn line_span(&self) -> (usize, usize) {
        let (start, end) = self.selection();
        let last = if self.has_selection() && end.col == 0 && end.line > start.line {
            end.line - 1
        } else {
            end.line
        };
        (start.line, last)
    }

    fn indent_lines(&mut self, outdent: bool, cx: &mut Cx) {
        let (first, last) = self.line_span();
        let start = Pos::new(first, 0);
        let end = Pos::new(last, self.buffer.line_len(last));
        let original = self.buffer.text_range(start, end);
        let mut deltas = Vec::with_capacity(last - first + 1);
        let mut lines = Vec::with_capacity(last - first + 1);
        for line in original.split('\n') {
            if outdent {
                let remove = if line.starts_with('\t') {
                    1
                } else {
                    line.bytes().take_while(|&b| b == b' ').count().min(4)
                };
                deltas.push(-(remove as isize));
                lines.push(line[remove..].to_owned());
            } else {
                deltas.push(4);
                lines.push(format!("    {line}"));
            }
        }
        let changed = lines.join("\n");
        if changed == original {
            return;
        }
        let adjust = |pos: Pos| {
            if (first..=last).contains(&pos.line) {
                let delta = deltas[pos.line - first];
                let col = if delta >= 0 {
                    pos.col + delta as usize
                } else {
                    pos.col.saturating_sub((-delta) as usize)
                };
                Pos::new(pos.line, col)
            } else {
                pos
            }
        };
        let after = (adjust(self.caret), adjust(self.anchor));
        self.replace_with_selection(start, end, &changed, EditKind::Other, Some(after), cx);
    }

    fn toggle_comment(&mut self, cx: &mut Cx) {
        let (first, last) = self.line_span();
        let start = Pos::new(first, 0);
        let end = Pos::new(last, self.buffer.line_len(last));
        let original = self.buffer.text_range(start, end);
        let lines: Vec<&str> = original.split('\n').collect();
        let remove = lines.iter().all(|line| {
            let indent = line.len() - line.trim_start().len();
            line[indent..].starts_with("//")
        });
        let mut deltas = Vec::with_capacity(lines.len());
        let mut changed = Vec::with_capacity(lines.len());
        for line in lines {
            let indent = line.len() - line.trim_start().len();
            if remove {
                let tail = &line[indent + 2..];
                let spaces = usize::from(tail.starts_with(' '));
                deltas.push((indent, -((2 + spaces) as isize)));
                changed.push(format!("{}{}", &line[..indent], &tail[spaces..]));
            } else {
                deltas.push((indent, 3));
                changed.push(format!("{}// {}", &line[..indent], &line[indent..]));
            }
        }
        if changed.join("\n") == original {
            return;
        }
        let adjust = |pos: Pos| {
            if (first..=last).contains(&pos.line) {
                let (at, delta) = deltas[pos.line - first];
                let col = if pos.col < at {
                    pos.col
                } else if delta >= 0 {
                    pos.col + delta as usize
                } else {
                    pos.col.saturating_sub((-delta) as usize)
                };
                Pos::new(pos.line, col)
            } else {
                pos
            }
        };
        let after = (adjust(self.caret), adjust(self.anchor));
        self.replace_with_selection(
            start,
            end,
            &changed.join("\n"),
            EditKind::Other,
            Some(after),
            cx,
        );
    }

    fn duplicate_line(&mut self, cx: &mut Cx) {
        let line = self.caret.line;
        let text = self.buffer.line(line).to_owned();
        let (at, inserted) = if line + 1 < self.buffer.line_count() {
            (Pos::new(line + 1, 0), format!("{text}\n"))
        } else {
            (self.buffer.end(), format!("\n{text}"))
        };
        self.replace(at, at, &inserted, EditKind::Other, cx);
    }

    fn delete_line(&mut self, cx: &mut Cx) {
        let line = self.caret.line;
        let (start, end) = if line + 1 < self.buffer.line_count() {
            (Pos::new(line, 0), Pos::new(line + 1, 0))
        } else if line > 0 {
            (
                Pos::new(line - 1, self.buffer.line_len(line - 1)),
                self.buffer.end(),
            )
        } else {
            (Pos::new(0, 0), self.buffer.end())
        };
        self.replace(start, end, "", EditKind::Other, cx);
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

    fn offset_at(&self, pos: Pos) -> usize {
        (0..pos.line.min(self.buffer.line_count()))
            .map(|line| self.buffer.line_len(line) + 1)
            .sum::<usize>()
            + pos.col
    }

    fn pos_at(&self, mut offset: usize) -> Pos {
        for line in 0..self.buffer.line_count() {
            let len = self.buffer.line_len(line);
            if offset <= len {
                return Pos::new(line, offset);
            }
            offset -= len + 1;
        }
        self.buffer.end()
    }

    pub(super) fn find_text(
        &mut self,
        query: &str,
        reverse: bool,
        repeat: bool,
        cx: &mut Cx,
    ) -> bool {
        if query.is_empty() {
            return false;
        }
        let text = self.buffer.text_range(Pos::new(0, 0), self.buffer.end());
        let (selection_start, selection_end) = self.selection();
        let from = if repeat && self.has_selection() {
            if reverse {
                selection_start
            } else {
                selection_end
            }
        } else if self.has_selection() {
            selection_start
        } else {
            self.caret
        };
        let offset = self.offset_at(from).min(text.len());
        let found = if reverse {
            text[..offset].rfind(query).or_else(|| text.rfind(query))
        } else {
            text[offset..]
                .find(query)
                .map(|relative| offset + relative)
                .or_else(|| text[..offset].find(query))
        };
        let Some(found) = found else {
            return false;
        };
        self.anchor = self.pos_at(found);
        self.caret = self.pos_at(found + query.len());
        self.preferred_x = None;
        self.ensure_caret_visible(cx.text);
        cx.repaint();
        true
    }

    pub(super) fn replace_find_match(
        &mut self,
        query: &str,
        replacement: &str,
        cx: &mut Cx,
    ) -> bool {
        if self.read_only {
            cx.log_error(format!("{} is read-only", self.name));
            return false;
        }
        let (start, end) = self.selection();
        if query.is_empty() || self.buffer.text_range(start, end) != query {
            return false;
        }
        if query != replacement {
            self.replace(start, end, replacement, EditKind::Other, cx);
        }
        true
    }

    pub(super) fn replace_all(&mut self, query: &str, replacement: &str, cx: &mut Cx) -> usize {
        if self.read_only {
            cx.log_error(format!("{} is read-only", self.name));
            return 0;
        }
        if query.is_empty() {
            return 0;
        }
        let start = Pos::new(0, 0);
        let end = self.buffer.end();
        let original = self.buffer.text_range(start, end);
        let count = original.matches(query).count();
        if count > 0 {
            let changed = original.replace(query, replacement);
            if changed == original {
                return count;
            }
            self.replace(start, end, &changed, EditKind::Other, cx);
        }
        count
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
            self.in_comment.clear();
            self.words = None;
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

    pub(super) fn on_key(&mut self, key: Key, mods: Modifiers, cx: &mut Cx) -> bool {
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
                if mods.shift {
                    self.indent_lines(true, cx);
                } else if self.has_selection() {
                    self.indent_lines(false, cx);
                } else {
                    let chars = self.buffer.line(self.caret.line)[..self.caret.col]
                        .chars()
                        .count();
                    let n = TAB_WIDTH - chars % TAB_WIDTH;
                    self.replace_selection(&" ".repeat(n), EditKind::Typing, cx);
                }
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
            Key::Slash if mods.ctrl => self.toggle_comment(cx),
            Key::Letter('D') if mods.ctrl => self.duplicate_line(cx),
            Key::Letter('K') if mods.ctrl && mods.shift => self.delete_line(cx),
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

    /// The right-click menu for `line` (1-based); its rows press the
    /// editor's own shortcuts.
    fn context_menu(&self, line: u32) -> Vec<Entry<Action>> {
        let key = |c, mods| Action::Key(Key::Letter(c), mods);
        let ctrl = Modifiers::CTRL;
        let write = !self.read_only;
        let sel = self.has_selection();
        vec![
            Entry::item(
                key('Z', ctrl),
                "Undo",
                "Ctrl+Z",
                write && self.undo.can_undo(),
            ),
            Entry::item(
                key('Y', ctrl),
                "Redo",
                "Ctrl+Y",
                write && self.undo.can_redo(),
            ),
            Entry::Separator,
            Entry::item(key('X', ctrl), "Cut", "Ctrl+X", write && sel),
            Entry::item(key('C', ctrl), "Copy", "Ctrl+C", sel),
            Entry::item(key('V', ctrl), "Paste", "Ctrl+V", write),
            Entry::item(key('A', ctrl), "Select all", "Ctrl+A", true),
            Entry::Separator,
            Entry::item(Action::Find(false), "Find", "Ctrl+F", true),
            Entry::item(
                Action::Find(true),
                "Find and replace",
                "Ctrl+H",
                !self.read_only,
            ),
            Entry::item(Action::GoToLine, "Go to line", "Ctrl+G", true),
            Entry::Separator,
            Entry::item(
                Action::Key(Key::Slash, ctrl),
                "Toggle line comment",
                "Ctrl+/",
                write,
            ),
            Entry::item(key('D', ctrl), "Duplicate line", "Ctrl+D", write),
            Entry::item(
                Action::Key(Key::Letter('K'), Modifiers::CTRL_SHIFT),
                "Delete line",
                "Ctrl+Shift+K",
                write,
            ),
            Entry::Separator,
            Entry::item(key('S', ctrl), "Save", "Ctrl+S", write),
            Entry::item(
                Action::ToggleBreakpoint {
                    path: self.path.clone(),
                    line,
                },
                "Toggle breakpoint",
                "",
                true,
            ),
        ]
    }

    pub fn on_event(&mut self, e: &Event, cx: &mut Cx) -> bool {
        match *e {
            Event::KeyDown { key, mods } => self.on_key(key, mods, cx),
            Event::MouseDown {
                pos,
                button: MouseButton::Right,
                ..
            } if self.rect.contains(pos) => {
                // Inside the selection it stays, for Cut and Copy.
                let p = self.pos_at_point(cx.text, pos);
                let (start, end) = self.selection();
                if !(start <= p && p < end) {
                    self.anchor = p;
                    self.caret = p;
                    self.preferred_x = None;
                    cx.repaint();
                }
                true
            }
            Event::MouseUp {
                pos,
                button: MouseButton::Right,
                ..
            } if self.rect.contains(pos) => {
                let line = self.pos_at_point(cx.text, pos).line as u32 + 1;
                cx.act(Action::ContextMenu {
                    pos,
                    items: self.context_menu(line),
                });
                true
            }
            Event::Char(c) if !c.is_control() => {
                let mut buf = [0u8; 4];
                self.replace_selection(c.encode_utf8(&mut buf), EditKind::Typing, cx);
                true
            }
            Event::MouseDown {
                pos,
                button: MouseButton::Left,
                ..
            } if self.rect.contains(pos) && pos.x < self.text_rect.x => {
                // Gutter: toggle a breakpoint on that line.
                let line = self.pos_at_point(cx.text, pos).line as u32 + 1;
                cx.act(Action::ToggleBreakpoint {
                    path: self.path.clone(),
                    line,
                });
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
            if self.code {
                self.line_runs(text, line);
            }
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
            let exec = self.exec_line.map(|l| l as usize - 1);
            for &(line, y, _) in &rows {
                if exec == Some(line) {
                    d.fill_rect(
                        RectF::new(rect.x, y, rect.w, self.line_h),
                        theme.exec_line_bg,
                    );
                }
                let dot = 9.0f32.min(self.line_h - 4.0);
                let cy = y + self.line_h / 2.0;
                if self.breakpoints.binary_search(&(line as u32 + 1)).is_ok() {
                    d.fill_round_rect(
                        RectF::new(rect.x + 4.0, cy - dot / 2.0, dot, dot),
                        dot / 2.0,
                        theme.breakpoint,
                    );
                }
                if exec == Some(line) {
                    // A chevron pointing at the line, right of any
                    // breakpoint dot.
                    let x = rect.x + 15.0;
                    d.line(
                        PointF::new(x, cy - 4.5),
                        PointF::new(x + 5.0, cy),
                        theme.accent,
                        2.0,
                    );
                    d.line(
                        PointF::new(x + 5.0, cy),
                        PointF::new(x, cy + 4.5),
                        theme.accent,
                        2.0,
                    );
                }
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
                    if line == caret_line && !has_selection && focused && exec != Some(line) {
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
                    if let Some((_, runs)) = self.runs.get(&id) {
                        for run in runs {
                            let at = PointF::new(origin_x + run.x, y + text_dy);
                            d.draw_layout(&run.layout, at, syntax::color(theme, run.kind));
                        }
                    } else if let Some(layout) = self.layouts.get(&id) {
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
        self.runs.retain(|id, _| visible.contains(id));
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

    /// A document holding `text`, as if typed into a C file.
    fn code_doc(text: &str) -> Document {
        Document::from_text(Path::new("a.cpp"), text, false)
    }

    #[test]
    fn comment_state_follows_edits_to_earlier_lines() {
        let mut doc = code_doc("int a;\nint b;\n");
        assert!(!doc.comment_state(1));
        doc.buffer.insert(Pos::new(0, 0), "/* ");
        // What `replace` does after an edit on line 0.
        doc.in_comment.truncate(1);
        assert!(doc.comment_state(1));
        assert!(doc.comment_state(2));
    }

    #[test]
    fn completion_prefix_is_the_word_before_the_caret() {
        let mut doc = code_doc("int count = cou; // cou\ns = \"cou\"; 12");
        let at = |doc: &mut Document, line, col| {
            doc.caret = Pos::new(line, col);
            doc.anchor = doc.caret;
            doc.completion_prefix()
        };
        assert_eq!(at(&mut doc, 0, 15), Some((12, "cou".to_string())));
        assert_eq!(at(&mut doc, 0, 0), Some((0, String::new())));
        assert_eq!(at(&mut doc, 0, 23), None, "comment");
        assert_eq!(at(&mut doc, 1, 8), None, "string");
        assert_eq!(at(&mut doc, 1, 13), None, "number");
        doc.anchor = Pos::new(0, 0);
        assert_eq!(doc.completion_prefix(), None, "selection");
    }

    #[test]
    fn save_as_moves_the_document_to_its_new_file() {
        let dir = std::env::temp_dir().join(format!("lattice-saveas-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (old, new) = (dir.join("a.txt"), dir.join("b.cpp"));
        std::fs::write(&old, "one\n").unwrap();
        let mut doc = Document::open(&old).unwrap();
        assert!(!doc.is_code());
        doc.save_as(&new).unwrap();
        assert_eq!(doc.path(), new);
        assert_eq!(doc.name(), "b.cpp");
        assert!(doc.is_code());
        assert_eq!(std::fs::read_to_string(&new).unwrap(), "one\n");
        assert_eq!(
            std::fs::read_to_string(&old).unwrap(),
            "one\n",
            "old file stays"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

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
