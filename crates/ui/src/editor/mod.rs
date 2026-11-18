//! Tabbed editor area: a tab strip over the active [`Document`].

mod buffer;
mod complete;
mod undo;
mod view;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use complete::Completion;
use debugger::Snapshot;
use view::Document;

use crate::event::{CursorShape, Event, Key, MouseButton};
use crate::geom::{PointF, RectF};
use crate::hovercard::{self, HoverCard};
use crate::memory::{self, MemTarget};
use crate::menubar::Entry;
use crate::panel::{Action, Cx, Panel};
use crate::syntax::is_word;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FindField {
    Query,
    Replacement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FindMode {
    Find,
    GoToLine,
}

struct FindBar {
    query: String,
    query_cursor: usize,
    line_query: String,
    line_cursor: usize,
    replacement: String,
    replacement_cursor: usize,
    replace_mode: bool,
    mode: FindMode,
    field: FindField,
}

impl FindBar {
    fn active_value_mut(&mut self) -> (&mut String, &mut usize) {
        if self.mode == FindMode::GoToLine {
            return (&mut self.line_query, &mut self.line_cursor);
        }
        match self.field {
            FindField::Query => (&mut self.query, &mut self.query_cursor),
            FindField::Replacement => (&mut self.replacement, &mut self.replacement_cursor),
        }
    }
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
    /// The last stop, for the "View in memory" card on variables.
    snap: Option<Arc<Snapshot>>,
    card: Option<HoverCard>,
    /// The open completion list.
    completion: Option<Completion>,
    /// Find and replace bar layered over the editor.
    find_bar: Option<FindBar>,
}

impl EditorArea {
    pub fn set_snapshot(&mut self, snap: Arc<Snapshot>) {
        self.snap = Some(snap);
    }

    pub fn has_documents(&self) -> bool {
        !self.docs.is_empty()
    }

    pub fn can_undo(&self) -> bool {
        self.docs.get(self.active).is_some_and(Document::can_undo)
    }

    pub fn can_redo(&self) -> bool {
        self.docs.get(self.active).is_some_and(Document::can_redo)
    }

    pub fn has_selection(&self) -> bool {
        self.docs
            .get(self.active)
            .is_some_and(Document::has_selection)
    }

    pub fn is_read_only(&self) -> bool {
        self.docs
            .get(self.active)
            .is_some_and(Document::is_read_only)
    }

    /// Runs an editor command against its active document, including when a
    /// menu rather than the editor surface supplied the command.
    pub fn command(&mut self, key: Key, mods: crate::event::Modifiers, cx: &mut Cx) -> bool {
        self.completion = None;
        self.docs
            .get_mut(self.active)
            .is_some_and(|doc| doc.on_key(key, mods, cx))
    }

    pub fn open_find(&mut self, replace_mode: bool, cx: &mut Cx) {
        let Some(doc) = self.docs.get(self.active) else {
            cx.log("Open a file first to search it");
            return;
        };
        let selected = doc.selected_text();
        let find = self.find_bar.get_or_insert_with(|| FindBar {
            query: String::new(),
            query_cursor: 0,
            line_query: String::new(),
            line_cursor: 0,
            replacement: String::new(),
            replacement_cursor: 0,
            replace_mode,
            mode: FindMode::Find,
            field: FindField::Query,
        });
        find.mode = FindMode::Find;
        find.replace_mode = replace_mode;
        find.field = FindField::Query;
        if find.query.is_empty() && !selected.contains('\n') {
            find.query = selected;
        }
        find.query_cursor = find.query.len();
        self.completion = None;
        cx.repaint();
    }

    pub fn open_goto_line(&mut self, cx: &mut Cx) {
        let Some(doc) = self.docs.get(self.active) else {
            cx.log("Open a file first to go to a line");
            return;
        };
        let find = self.find_bar.get_or_insert_with(|| FindBar {
            query: String::new(),
            query_cursor: 0,
            line_query: String::new(),
            line_cursor: 0,
            replacement: String::new(),
            replacement_cursor: 0,
            replace_mode: false,
            mode: FindMode::GoToLine,
            field: FindField::Query,
        });
        find.mode = FindMode::GoToLine;
        find.replace_mode = false;
        find.field = FindField::Query;
        find.line_query = doc.caret_line().to_string();
        find.line_cursor = find.line_query.len();
        cx.repaint();
    }

    fn find_bar_rect(&self) -> RectF {
        let width = (self.body.w - 16.0).clamp(0.0, 440.0);
        let height = if self.find_bar.as_ref().is_some_and(|bar| bar.replace_mode) {
            62.0
        } else {
            56.0
        };
        RectF::new(
            self.body.right() - width - 8.0,
            self.body.y + 8.0,
            width,
            height,
        )
    }

    fn paint_find_bar(&self, d: &mut DrawCtx, theme: &Theme) {
        let Some(find) = &self.find_bar else {
            return;
        };
        let bar = self.find_bar_rect();
        if bar.w < 190.0 {
            return;
        }
        d.fill_round_rect(bar, 4.0, theme.panel_bg);
        let query = RectF::new(bar.x + 6.0, bar.y + 6.0, bar.w - 12.0, 24.0);
        let (label, value, active) = if find.mode == FindMode::GoToLine {
            ("Line", find.line_query.as_str(), true)
        } else if find.field == FindField::Query {
            ("Find", find.query.as_str(), true)
        } else {
            ("Find", find.query.as_str(), false)
        };
        self.paint_find_field(d, theme, query, label, value, active);
        if find.replace_mode {
            let replacement = RectF::new(bar.x + 6.0, bar.y + 32.0, bar.w - 12.0, 24.0);
            self.paint_find_field(
                d,
                theme,
                replacement,
                "Replace",
                &find.replacement,
                find.field == FindField::Replacement,
            );
        } else {
            d.text(
                TextStyle::UiSmall,
                "Enter next  ·  Shift+Enter previous  ·  Esc close",
                RectF::new(bar.x + 10.0, bar.y + 34.0, bar.w - 20.0, 18.0),
                theme.text_dim,
            );
        }
    }

    fn paint_find_field(
        &self,
        d: &mut DrawCtx,
        theme: &Theme,
        rect: RectF,
        label: &str,
        value: &str,
        active: bool,
    ) {
        d.fill_round_rect(rect, 3.0, theme.editor_bg);
        let edge = if active { theme.accent } else { theme.border };
        d.fill_rect(RectF::new(rect.x, rect.y, rect.w, 1.0), edge);
        d.fill_rect(RectF::new(rect.x, rect.bottom() - 1.0, rect.w, 1.0), edge);
        d.fill_rect(RectF::new(rect.x, rect.y, 1.0, rect.h), edge);
        d.fill_rect(RectF::new(rect.right() - 1.0, rect.y, 1.0, rect.h), edge);
        let label_rect = RectF::new(rect.x + 7.0, rect.y, 54.0, rect.h);
        let value_rect = RectF::new(rect.x + 63.0, rect.y, rect.w - 70.0, rect.h);
        d.text(TextStyle::UiSmall, label, label_rect, theme.text_dim);
        d.text(
            TextStyle::UiSmall,
            if value.is_empty() {
                if label == "Line" {
                    "Line number"
                } else {
                    "Type to search"
                }
            } else {
                value
            },
            value_rect,
            if value.is_empty() {
                theme.text_dim
            } else {
                theme.text
            },
        );
    }

    fn find_bar_event(&mut self, e: &Event, cx: &mut Cx) -> bool {
        let bar_rect = self.find_bar_rect();
        if bar_rect.w < 190.0 {
            return false;
        }
        let Some(find) = self.find_bar.as_mut() else {
            return false;
        };
        let mut search = None;
        let mut replace_current = false;
        let mut replace_all = false;
        let mut go_to_line = false;
        match *e {
            Event::KeyDown {
                key: Key::Escape, ..
            } => {
                self.find_bar = None;
                cx.repaint();
                return true;
            }
            Event::KeyDown {
                key: Key::Enter,
                mods,
            } => {
                if find.mode == FindMode::GoToLine {
                    go_to_line = true;
                } else if find.replace_mode && find.field == FindField::Replacement {
                    replace_all = mods.ctrl;
                    replace_current = !mods.ctrl;
                } else {
                    search = Some((mods.shift, true));
                }
            }
            Event::KeyDown {
                key: Key::Tab,
                mods,
            } if mods.ctrl => return false,
            Event::KeyDown { key: Key::Tab, .. } if find.replace_mode => {
                find.field = match find.field {
                    FindField::Query => FindField::Replacement,
                    FindField::Replacement => FindField::Query,
                };
                let (value, cursor) = find.active_value_mut();
                *cursor = value.len();
                cx.repaint();
                return true;
            }
            Event::KeyDown { key: Key::Left, .. } => {
                let (value, cursor) = find.active_value_mut();
                if *cursor > 0 {
                    *cursor = value[..*cursor]
                        .char_indices()
                        .next_back()
                        .map_or(0, |(i, _)| i);
                }
                cx.repaint();
                return true;
            }
            Event::KeyDown {
                key: Key::Right, ..
            } => {
                let (value, cursor) = find.active_value_mut();
                if *cursor < value.len() {
                    *cursor += value[*cursor..].chars().next().map_or(0, char::len_utf8);
                }
                cx.repaint();
                return true;
            }
            Event::KeyDown { key: Key::Home, .. } => {
                *find.active_value_mut().1 = 0;
                cx.repaint();
                return true;
            }
            Event::KeyDown { key: Key::End, .. } => {
                let (value, cursor) = find.active_value_mut();
                *cursor = value.len();
                cx.repaint();
                return true;
            }
            Event::KeyDown {
                key: Key::Backspace,
                ..
            } => {
                let (value, cursor) = find.active_value_mut();
                if *cursor > 0 {
                    let previous = value[..*cursor]
                        .char_indices()
                        .next_back()
                        .map_or(0, |(i, _)| i);
                    value.replace_range(previous..*cursor, "");
                    *cursor = previous;
                }
                if find.field == FindField::Query && find.mode == FindMode::Find {
                    search = Some((false, false));
                }
            }
            Event::KeyDown {
                key: Key::Delete, ..
            } => {
                let (value, cursor) = find.active_value_mut();
                if *cursor < value.len() {
                    let next = *cursor + value[*cursor..].chars().next().map_or(0, char::len_utf8);
                    value.replace_range(*cursor..next, "");
                }
                if find.field == FindField::Query && find.mode == FindMode::Find {
                    search = Some((false, false));
                }
            }
            Event::KeyDown {
                key: Key::Letter('A'),
                mods,
            } if mods.ctrl => {
                let (value, cursor) = find.active_value_mut();
                value.clear();
                *cursor = 0;
                if find.field == FindField::Query && find.mode == FindMode::Find {
                    search = Some((false, false));
                }
            }
            Event::KeyDown {
                key: Key::Letter('C'),
                mods,
            } if mods.ctrl => {
                let (value, _) = find.active_value_mut();
                if let Err(error) = crate::sys::clipboard::set_text(cx.window, value) {
                    cx.log_error(format!("Copy failed: {error}"));
                }
            }
            Event::KeyDown {
                key: Key::Letter('X'),
                mods,
            } if mods.ctrl => {
                let (value, cursor) = find.active_value_mut();
                if let Err(error) = crate::sys::clipboard::set_text(cx.window, value) {
                    cx.log_error(format!("Cut failed: {error}"));
                } else {
                    value.clear();
                    *cursor = 0;
                    if find.field == FindField::Query && find.mode == FindMode::Find {
                        search = Some((false, false));
                    }
                }
            }
            Event::KeyDown {
                key: Key::Letter('V'),
                mods,
            } if mods.ctrl => {
                if let Some(text) = crate::sys::clipboard::get_text(cx.window) {
                    let text = buffer::normalize_newlines(&text);
                    let (value, cursor) = find.active_value_mut();
                    value.insert_str(*cursor, &text);
                    *cursor += text.len();
                    if find.field == FindField::Query && find.mode == FindMode::Find {
                        search = Some((false, false));
                    }
                }
            }
            Event::Char(ch) if !ch.is_control() => {
                let (value, cursor) = find.active_value_mut();
                value.insert(*cursor, ch);
                *cursor += ch.len_utf8();
                if find.field == FindField::Query && find.mode == FindMode::Find {
                    search = Some((false, false));
                }
            }
            Event::MouseDown { pos, .. } if bar_rect.contains(pos) => {
                if find.replace_mode && pos.y >= bar_rect.y + 32.0 {
                    find.field = FindField::Replacement;
                } else {
                    find.field = FindField::Query;
                }
                let (value, cursor) = find.active_value_mut();
                *cursor = value.len();
                cx.repaint();
                return true;
            }
            Event::MouseUp { pos, .. } if bar_rect.contains(pos) => return true,
            Event::MouseDown { .. } => {
                self.find_bar = None;
                cx.repaint();
                return false;
            }
            Event::KeyDown { .. } => return false,
            _ => return false,
        }
        let query = find.query.clone();
        let line_query = find.line_query.clone();
        let replacement = find.replacement.clone();
        if go_to_line {
            if let (Ok(line), Some(doc)) =
                (line_query.parse::<u32>(), self.docs.get_mut(self.active))
            {
                doc.goto_line(line);
                self.find_bar = None;
                cx.repaint();
                return true;
            } else {
                cx.log("Enter a valid line number");
            }
        } else if replace_all {
            if let Some(doc) = self.docs.get_mut(self.active) {
                let count = doc.replace_all(&query, &replacement, cx);
                cx.log(format!("Replaced {count} occurrence(s)"));
            }
        } else if replace_current {
            if let Some(doc) = self.docs.get_mut(self.active) {
                doc.replace_find_match(&query, &replacement, cx);
                if !doc.find_text(&query, false, true, cx) {
                    cx.log(format!("No match for {query:?}"));
                }
            }
        }
        if let Some((reverse, repeat)) = search {
            if let Some(doc) = self.docs.get_mut(self.active) {
                let found = doc.find_text(&query, reverse, repeat, cx);
                if repeat && !found {
                    cx.log(format!("No match for {query:?}"));
                }
            }
        }
        cx.repaint();
        true
    }

    /// Opens `path`, or activates it if already open.
    pub fn open(&mut self, path: &Path, cx: &mut Cx) {
        self.completion = None;
        if let Some(i) = self.docs.iter().position(|d| d.path() == path) {
            if self.active != i {
                self.find_bar = None;
            }
            self.active = i;
            cx.repaint();
            return;
        }
        match Document::open(path) {
            Ok(mut doc) => {
                self.find_bar = None;
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
        self.completion = None;
        let Some(doc) = self.docs.get_mut(i) else {
            return true;
        };
        if doc.is_dirty() && !resolve_unsaved(doc, cx) {
            return false;
        }
        self.docs.remove(i);
        self.find_bar = None;
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

    pub fn close_tab(&mut self, i: usize, cx: &mut Cx) {
        self.close(i, cx);
    }

    pub fn close_active(&mut self, cx: &mut Cx) {
        if !self.docs.is_empty() {
            self.close(self.active, cx);
        }
    }

    /// The file in the active tab.
    pub fn active_path(&self) -> Option<&Path> {
        self.docs.get(self.active).map(Document::path)
    }

    fn save_doc(doc: &mut Document, cx: &mut Cx) {
        match doc.save() {
            Ok(()) => cx.log(format!("Saved {}", doc.path().display())),
            Err(e) => cx.log_error(format!("Save failed: {e}")),
        }
        cx.repaint();
    }

    pub fn save_active(&mut self, cx: &mut Cx) {
        if let Some(doc) = self.docs.get_mut(self.active) {
            Self::save_doc(doc, cx);
        }
    }

    pub fn save_all(&mut self, cx: &mut Cx) {
        for doc in self.docs.iter_mut().filter(|d| d.is_dirty()) {
            Self::save_doc(doc, cx);
        }
    }

    /// Saves the active tab to `path`, which no other tab may have open: two
    /// tabs on one file would overwrite each other.
    pub fn save_active_as(&mut self, path: &Path, cx: &mut Cx) {
        let other = self
            .docs
            .iter()
            .enumerate()
            .any(|(i, d)| i != self.active && d.path() == path);
        if other {
            cx.log_error(format!(
                "Save failed: {} is open in another tab",
                path.display()
            ));
            return;
        }
        let Some(doc) = self.docs.get_mut(self.active) else {
            return;
        };
        match doc.save_as(path) {
            Ok(()) => cx.log(format!("Saved {}", path.display())),
            Err(e) => cx.log_error(format!("Save failed: {e}")),
        }
        cx.repaint();
    }

    /// `from` (a file or a folder) was renamed: open files follow it.
    pub fn path_renamed(&mut self, from: &Path, to: &Path, cx: &mut Cx) {
        for doc in &mut self.docs {
            if let Some(new) = moved_path(doc.path(), from, to) {
                doc.set_path(&new);
            }
        }
        cx.repaint();
    }

    /// An open file at or under `path` has unsaved changes.
    pub fn has_unsaved_under(&self, path: &Path) -> bool {
        self.docs
            .iter()
            .any(|d| d.is_dirty() && d.path().starts_with(path))
    }

    /// `path` (a file or a folder) was deleted: closes its tabs unprompted.
    pub fn forget_under(&mut self, path: &Path, cx: &mut Cx) {
        let active = self.active_path().map(Path::to_path_buf);
        self.docs.retain(|d| !d.path().starts_with(path));
        self.active = active
            .and_then(|a| self.docs.iter().position(|d| d.path() == a))
            .unwrap_or_else(|| self.active.min(self.docs.len().saturating_sub(1)));
        self.completion = None;
        self.hover = None;
        self.hover_close = None;
        cx.repaint();
    }

    /// Closes every tab but `keep`, stopping if an unsaved prompt is
    /// cancelled.
    pub fn close_tabs_except(&mut self, keep: Option<usize>, cx: &mut Cx) {
        for i in (0..self.docs.len()).rev() {
            if Some(i) != keep && !self.close(i, cx) {
                return;
            }
        }
        self.active = 0;
    }

    /// Rebuilds the list for the word at the caret, offering the words of
    /// every open file. Without `forced`, only C and C++ files offer anything
    /// and only from the second letter.
    fn refresh_completion(&mut self, forced: bool, cx: &mut Cx) {
        self.completion = None;
        let Some(doc) = self.docs.get_mut(self.active) else {
            return;
        };
        let include_prefix = doc.include_completion_prefix();
        let Some((start, prefix)) = include_prefix.clone().or_else(|| doc.completion_prefix())
        else {
            return;
        };
        let code = doc.is_code();
        let is_include_path = include_prefix.is_some();
        let member_access = doc.is_member_access(start);
        if !forced
            && !is_include_path
            && (!code || (prefix.len() < 2 && (!member_access || prefix.is_empty())))
        {
            return;
        }
        let caret = doc.caret_rect(cx.text);
        let directive = doc.is_directive(start);
        for d in &mut self.docs {
            d.update_words();
        }
        let mut own = Vec::with_capacity(self.docs.len());
        if let Some(words) = self.docs[self.active].own_words() {
            own.push(words);
        }
        own.extend(
            self.docs
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != self.active)
                .filter_map(|(_, doc)| doc.own_words()),
        );
        let included: Vec<_> = if prefix.len() >= 3 {
            self.docs.iter().map(Document::included_words).collect()
        } else {
            Vec::new()
        };
        let functions: std::collections::BTreeSet<String> = self
            .docs
            .iter()
            .flat_map(|doc| {
                doc.function_words()
                    .iter()
                    .chain(doc.included_function_words())
                    .cloned()
            })
            .collect();
        let items = if is_include_path {
            complete::include_candidates(&prefix, self.docs[self.active].include_paths())
        } else if directive {
            complete::directives(&prefix)
        } else if member_access {
            let mut members = Vec::with_capacity(self.docs.len());
            members.push(self.docs[self.active].member_words());
            members.extend(
                self.docs
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != self.active)
                    .map(|(_, doc)| doc.member_words()),
            );
            let matches = complete::candidates(
                &prefix,
                false,
                members.iter().copied(),
                std::iter::empty::<&std::collections::BTreeSet<String>>(),
            );
            if matches.is_empty() {
                complete::candidates(
                    &prefix,
                    false,
                    own.iter().copied(),
                    included.iter().copied(),
                )
            } else {
                matches
            }
        } else {
            complete::candidates(&prefix, code, own.iter().copied(), included.iter().copied())
        };
        self.completion = Completion::new(items, start, caret, self.body, cx.text);
        if let Some(c) = &mut self.completion {
            c.include_path = is_include_path;
            c.set_functions(functions);
            c.enter_makes_newline =
                complete::is_complete(&prefix, own.iter().chain(included.iter()).copied());
        }
    }

    fn accept_completion(&mut self, cx: &mut Cx) {
        if let (Some(c), Some(doc)) = (self.completion.take(), self.docs.get_mut(self.active)) {
            doc.accept_completion(c.start_col, c.include_path, c.selected_word(), cx);
        }
    }

    /// Input for the open list. Returns `true` if the list used it; anything
    /// else that isn't editing the word closes it.
    fn completion_event(&mut self, e: &Event, cx: &mut Cx) -> bool {
        let Some(c) = &mut self.completion else {
            return false;
        };
        match *e {
            Event::KeyDown { key: Key::Up, .. } => c.move_by(-1),
            Event::KeyDown { key: Key::Down, .. } => c.move_by(1),
            Event::KeyDown {
                key: Key::Enter, ..
            } if c.enter_makes_newline && !c.has_choice() => {
                self.completion = None;
                cx.repaint();
                return false;
            }
            Event::KeyDown {
                key: Key::Enter, ..
            } if !c.has_choice() => {
                self.completion = None;
                cx.repaint();
                return false;
            }
            Event::KeyDown {
                key: Key::Tab,
                mods,
            } if !mods.ctrl && !mods.alt => self.accept_completion(cx),
            Event::KeyDown {
                key: Key::Enter,
                mods,
            } if !mods.ctrl && !mods.alt => self.accept_completion(cx),
            Event::KeyDown {
                key: Key::Escape, ..
            } => self.completion = None,
            Event::MouseDown {
                pos,
                button: MouseButton::Left,
                ..
            } if c.contains(pos) => {
                c.select_at(pos);
                self.accept_completion(cx);
            }
            // The press's release, and hovering the list, are the list's.
            Event::MouseUp { pos, .. } | Event::MouseMove { pos, .. } if c.contains(pos) => {
                return true;
            }
            Event::KeyDown {
                key: Key::Backspace,
                ..
            }
            | Event::Char(_)
            | Event::MouseUp { .. }
            | Event::MouseMove { .. }
            | Event::MouseLeave
            | Event::Focus(_) => return false,
            _ => {
                self.completion = None;
                cx.repaint();
                return false;
            }
        }
        cx.repaint();
        true
    }

    fn tab_menu(&self, i: usize) -> Vec<Entry<Action>> {
        let path = self.docs[i].path().to_path_buf();
        vec![
            Entry::item(Action::CloseTab(i), "Close", "Ctrl+W", true),
            Entry::item(
                Action::CloseTabsExcept(Some(i)),
                "Close others",
                "",
                self.docs.len() > 1,
            ),
            Entry::item(Action::CloseTabsExcept(None), "Close all", "", true),
            Entry::Separator,
            Entry::item(
                Action::CopyText(path.display().to_string()),
                "Copy path",
                "",
                true,
            ),
            Entry::item(Action::Reveal(path), "Reveal in File Explorer", "", true),
        ]
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

    /// The active file and caret line (1-based).
    pub fn caret_location(&self) -> Option<(std::path::PathBuf, u32)> {
        let doc = self.docs.get(self.active)?;
        Some((doc.path().to_path_buf(), doc.caret_line()))
    }

    /// Updates every open document's debugger marks. Returns `true` if any
    /// changed.
    pub fn set_marks(&mut self, marks: impl Fn(&Path) -> (Vec<u32>, Option<u32>)) -> bool {
        let mut changed = false;
        for doc in &mut self.docs {
            let (bps, exec) = marks(doc.path());
            changed |= doc.set_marks(bps, exec);
        }
        changed
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

/// Where `path` is now that `from` is called `to`, if it was affected.
fn moved_path(path: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    let rest = path.strip_prefix(from).ok()?;
    Some(if rest.as_os_str().is_empty() {
        to.to_path_buf()
    } else {
        to.join(rest)
    })
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
        if self.body != body {
            self.completion = None;
        }
        self.body = body;
        if body.w < 206.0 {
            self.find_bar = None;
        }
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
            Some(doc) => {
                doc.paint(d, theme, focused);
                if let Some(card) = &self.card {
                    card.paint(d, theme);
                }
                if let Some(list) = self.completion.as_ref().filter(|_| focused) {
                    list.paint(d, theme);
                }
            }
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
        self.paint_find_bar(d, theme);
    }

    fn on_event(&mut self, e: &Event, cx: &mut Cx) -> bool {
        if self.completion_event(e, cx) {
            return true;
        }
        if self.find_bar_event(e, cx) {
            return true;
        }
        match *e {
            Event::KeyDown {
                key: Key::Space,
                mods,
            } if mods.ctrl => {
                self.refresh_completion(true, cx);
                cx.repaint();
                return true;
            }
            Event::KeyDown {
                key: Key::Tab,
                mods,
            } if mods.ctrl => {
                if !self.docs.is_empty() {
                    self.find_bar = None;
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
                        if self.active != i {
                            self.find_bar = None;
                        }
                        self.active = i;
                        cx.repaint();
                    }
                }
                return true;
            }
            Event::MouseUp {
                pos,
                button: MouseButton::Right,
                ..
            } if self.strip.contains(pos) => {
                if let Some((i, _)) = self.hit(pos) {
                    cx.act(Action::ContextMenu {
                        pos,
                        items: self.tab_menu(i),
                    });
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
        let snap = self.snap.clone();
        let mut card = self.card.take();
        let doc = self.docs.get_mut(self.active);
        let taken = hovercard::track(&mut card, e, cx, |pos| {
            // Only names live at the last stop get a card.
            let (snap, doc) = (snap.as_deref()?, doc?);
            let (path, anchor) = doc.expr_at_point(text, pos)?;
            let target = MemTarget::Expr {
                file: doc.path().to_path_buf(),
                path,
            };
            if memory::resolve(snap, &target).is_empty() {
                return None;
            }
            Some(HoverCard::new(target, anchor, body, Some(snap), text))
        });
        self.card = card;
        if taken {
            return true;
        }
        let handled = match self.docs.get_mut(self.active) {
            Some(doc) => {
                doc.layout(body, text);
                doc.on_event(e, cx)
            }
            None => false,
        };
        // Typing a word opens or narrows the list; anything else typed ends it.
        match *e {
            Event::Char(_)
                if self
                    .docs
                    .get(self.active)
                    .is_some_and(|doc| doc.include_completion_prefix().is_some()) =>
            {
                self.refresh_completion(false, cx);
            }
            Event::Char(c)
                if c.is_ascii() && (is_word(c as u8) || matches!(c, '.' | '>' | ':')) =>
            {
                self.refresh_completion(false, cx);
            }
            Event::Char(_) => self.completion = None,
            Event::KeyDown {
                key: Key::Backspace,
                ..
            } if self.completion.is_some() => self.refresh_completion(false, cx),
            _ => {}
        }
        handled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Modifiers;
    use crate::sys::text::TextSystem;
    use crate::sys::NativeWindow;

    /// An editor on one file, driven by the events the window would send.
    struct Harness {
        area: EditorArea,
        text: TextSystem,
    }

    impl Harness {
        fn new(name: &str, content: &str) -> Self {
            let text = TextSystem::new().expect("DirectWrite");
            let mut area = EditorArea::default();
            area.docs
                .push(Document::from_text(Path::new(name), content, false));
            area.layout(RectF::new(0.0, 0.0, 800.0, 600.0), &text);
            Self { area, text }
        }

        fn send(&mut self, e: Event) {
            let mut cx = Cx::new(&self.text, NativeWindow::none());
            self.area.on_event(&e, &mut cx);
        }

        fn key(&mut self, key: Key, mods: Modifiers) {
            self.send(Event::KeyDown { key, mods });
        }

        /// A key press and the character it types, as Windows sends them.
        fn type_text(&mut self, s: &str) {
            for c in s.chars() {
                if c == '\n' {
                    self.key(Key::Enter, Modifiers::NONE);
                    continue;
                }
                if c.is_ascii_alphabetic() {
                    let key = Key::Letter(c.to_ascii_uppercase());
                    self.key(key, Modifiers::NONE);
                }
                self.send(Event::Char(c));
            }
        }

        fn items(&self) -> Vec<&str> {
            self.area
                .completion
                .iter()
                .flat_map(|c| c.items())
                .map(String::as_str)
                .collect()
        }

        fn doc(&self) -> String {
            self.area.docs[0].text_for_test()
        }
    }

    #[test]
    fn typing_offers_your_words_and_tab_takes_one() {
        let mut h = Harness::new("a.cpp", "");
        h.type_text("int counter;\nco");
        assert_eq!(h.items()[0], "counter", "{:?}", h.items());
        h.type_text("u");
        assert_eq!(h.items(), ["counter"], "narrowed");
        h.key(Key::Tab, Modifiers::NONE);
        assert!(h.area.completion.is_none());
        assert_eq!(h.doc(), "int counter;\ncounter");
    }

    #[test]
    fn enter_after_a_whole_word_starts_a_new_line() {
        let mut h = Harness::new("a.cpp", "");
        h.type_text("do");
        assert!(h.items().contains(&"double"));
        h.key(Key::Enter, Modifiers::NONE);
        assert_eq!(h.doc(), "do\n", "not double");
        assert!(h.area.completion.is_none());

        // A word declared earlier counts as whole too.
        let mut h = Harness::new("a.cpp", "int count, counter;\n");
        h.key(Key::End, Modifiers::CTRL);
        h.type_text("count");
        assert_eq!(h.items(), ["counter"]);
        h.key(Key::Enter, Modifiers::NONE);
        assert_eq!(h.doc(), "int count, counter;\ncount\n");

        // But Enter takes a suggestion for a word that is only partly typed.
        let mut h = Harness::new("a.cpp", "int counter;\n");
        h.key(Key::End, Modifiers::CTRL);
        h.type_text("cou");
        h.key(Key::Enter, Modifiers::NONE);
        assert_eq!(h.doc(), "int counter;\ncounter");
    }

    #[test]
    fn a_half_typed_word_is_not_offered_back() {
        let mut h = Harness::new("a.cpp", "");
        h.type_text("zebra zeb");
        h.key(Key::Backspace, Modifiers::NONE);
        assert_eq!(h.items(), ["zebra"], "not `zeb`, just deleted");
        // Forcing the list on one letter, the unfinished word still isn't there.
        h.key(Key::Backspace, Modifiers::NONE);
        h.key(Key::Space, Modifiers::CTRL);
        assert_eq!(h.items(), ["zebra"]);
    }

    #[test]
    fn accepting_inside_a_word_replaces_all_of_it() {
        let mut h = Harness::new("a.cpp", "format fobar");
        // Caret inside the second word: fo|bar.
        for _ in 0..9 {
            h.key(Key::Right, Modifiers::NONE);
        }
        h.key(Key::Space, Modifiers::CTRL);
        let at = h.items().iter().position(|w| *w == "format").unwrap() as isize;
        h.area.completion.as_mut().unwrap().move_by(at);
        h.key(Key::Tab, Modifiers::NONE);
        assert_eq!(h.doc(), "format format");
    }

    #[test]
    fn nothing_offered_in_comments_strings_or_from_them() {
        let mut h = Harness::new("a.cpp", "// secretword\n");
        h.key(Key::End, Modifiers::CTRL);
        h.type_text("sec");
        assert!(!h.items().contains(&"secretword"), "{:?}", h.items());
        h.type_text("\n// co");
        assert!(h.area.completion.is_none(), "in a comment");
        h.type_text("\ns = \"co");
        assert!(h.area.completion.is_none(), "in a string");
    }

    #[test]
    fn directives_complete_after_a_hash() {
        let mut h = Harness::new("a.cpp", "");
        h.type_text("#inc");
        assert_eq!(h.items(), ["include"]);
        h.key(Key::Tab, Modifiers::NONE);
        assert_eq!(h.doc(), "#include");
    }

    #[test]
    fn keys_drive_the_list() {
        let mut h = Harness::new("a.cpp", "zzalpha zzalps zzalto\n");
        h.key(Key::End, Modifiers::CTRL);
        h.type_text("zz");
        assert_eq!(h.items(), ["zzalpha", "zzalps", "zzalto"]);
        h.key(Key::Down, Modifiers::NONE);
        h.key(Key::Enter, Modifiers::NONE);
        assert_eq!(h.doc(), "zzalpha zzalps zzalto\nzzalps");

        h.type_text(" zz");
        h.key(Key::Up, Modifiers::NONE);
        let selected = h.area.completion.as_ref().unwrap().selected_word();
        assert_eq!(selected, "zzalto", "wraps to the last");
        h.key(Key::Escape, Modifiers::NONE);
        assert!(h.area.completion.is_none());
        assert_eq!(
            h.doc(),
            "zzalpha zzalps zzalto\nzzalps zz",
            "Esc types nothing"
        );

        // Other kinds of file only complete on request.
        let mut h = Harness::new("notes.txt", "apple ");
        h.key(Key::End, Modifiers::CTRL);
        h.type_text("ap");
        assert!(h.area.completion.is_none());
        h.key(Key::Space, Modifiers::CTRL);
        assert_eq!(h.items(), ["apple"]);
    }

    #[test]
    fn open_files_follow_a_rename() {
        let (from, to) = (Path::new(r"C:\p\old"), Path::new(r"C:\p\new"));
        assert_eq!(moved_path(from, from, to).unwrap(), to);
        assert_eq!(
            moved_path(Path::new(r"C:\p\old\a\b.c"), from, to).unwrap(),
            Path::new(r"C:\p\new\a\b.c")
        );
        // `old2` is not inside `old`.
        assert_eq!(moved_path(Path::new(r"C:\p\old2\b.c"), from, to), None);
    }
}
