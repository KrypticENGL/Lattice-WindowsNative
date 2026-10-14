//! Undo/redo as groups of reversible edits, with typing coalesced.

use super::buffer::{end_after, Buffer, Pos};

/// Replacement of `removed` by `inserted` at `start`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub start: Pos,
    pub removed: String,
    pub inserted: String,
}

impl Edit {
    fn apply(&self, buf: &mut Buffer) {
        buf.delete(self.start, end_after(self.start, &self.removed));
        buf.insert(self.start, &self.inserted);
    }

    fn revert(&self, buf: &mut Buffer) {
        buf.delete(self.start, end_after(self.start, &self.inserted));
        buf.insert(self.start, &self.removed);
    }
}

/// How an edit was made; only like kinds coalesce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    Typing,
    Backspace,
    DeleteForward,
    Other,
}

/// Caret and anchor.
pub type Selection = (Pos, Pos);

#[derive(Debug, Clone)]
struct Group {
    id: u64,
    kind: EditKind,
    edit: Edit,
    before: Selection,
    after: Selection,
}

#[derive(Debug, Default)]
pub struct UndoStack {
    undo: Vec<Group>,
    redo: Vec<Group>,
    next_id: u64,
    sealed: bool,
}

impl UndoStack {
    /// Records an edit already applied to the buffer.
    pub fn record(&mut self, edit: Edit, kind: EditKind, before: Selection, after: Selection) {
        self.redo.clear();
        if !self.sealed {
            if let Some(last) = self.undo.last_mut() {
                if last.kind == kind && merge(&mut last.edit, &edit, kind) {
                    last.after = after;
                    return;
                }
            }
        }
        self.sealed = false;
        self.next_id += 1;
        self.undo.push(Group {
            id: self.next_id,
            kind,
            edit,
            before,
            after,
        });
    }

    /// Stops the next edit from merging into the current group.
    pub fn seal(&mut self) {
        self.sealed = true;
    }

    /// Identifies the current state; equal ids mean equal text.
    pub fn state_id(&self) -> u64 {
        self.undo.last().map_or(0, |g| g.id)
    }

    pub fn undo(&mut self, buf: &mut Buffer) -> Option<Selection> {
        let group = self.undo.pop()?;
        group.edit.revert(buf);
        let sel = group.before;
        self.redo.push(group);
        self.sealed = true;
        Some(sel)
    }

    pub fn redo(&mut self, buf: &mut Buffer) -> Option<Selection> {
        let group = self.redo.pop()?;
        group.edit.apply(buf);
        let sel = group.after;
        self.undo.push(group);
        self.sealed = true;
        Some(sel)
    }
}

/// Folds `next` into `last` when they form one contiguous run.
fn merge(last: &mut Edit, next: &Edit, kind: EditKind) -> bool {
    match kind {
        EditKind::Typing => {
            let contiguous = next.removed.is_empty()
                && end_after(last.start, &last.inserted) == next.start
                && !next.inserted.contains('\n');
            let word_break = last.inserted.ends_with(char::is_whitespace)
                && !next.inserted.starts_with(char::is_whitespace);
            if contiguous && !word_break {
                last.inserted.push_str(&next.inserted);
                return true;
            }
            false
        }
        EditKind::Backspace => {
            if last.inserted.is_empty()
                && next.inserted.is_empty()
                && end_after(next.start, &next.removed) == last.start
            {
                last.start = next.start;
                last.removed.insert_str(0, &next.removed);
                return true;
            }
            false
        }
        EditKind::DeleteForward => {
            if last.inserted.is_empty() && next.inserted.is_empty() && next.start == last.start {
                last.removed.push_str(&next.removed);
                return true;
            }
            false
        }
        EditKind::Other => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_str(buf: &mut Buffer, undo: &mut UndoStack, at: Pos, s: &str) -> Pos {
        let mut p = at;
        for c in s.chars() {
            let t = c.to_string();
            let end = buf.insert(p, &t);
            undo.record(
                Edit {
                    start: p,
                    removed: String::new(),
                    inserted: t,
                },
                EditKind::Typing,
                (p, p),
                (end, end),
            );
            p = end;
        }
        p
    }

    #[test]
    fn typing_coalesces_by_word_and_undoes() {
        let mut buf = Buffer::from_text("");
        let mut undo = UndoStack::default();
        type_str(&mut buf, &mut undo, Pos::new(0, 0), "hello world");
        assert_eq!(buf.line(0), "hello world");
        undo.undo(&mut buf);
        assert_eq!(buf.line(0), "hello ");
        undo.undo(&mut buf);
        assert_eq!(buf.line(0), "");
        assert!(undo.undo(&mut buf).is_none());
        undo.redo(&mut buf);
        undo.redo(&mut buf);
        assert_eq!(buf.line(0), "hello world");
    }

    #[test]
    fn backspace_run_is_one_group() {
        let mut buf = Buffer::from_text("abcd");
        let mut undo = UndoStack::default();
        for col in (1..=4).rev() {
            let start = Pos::new(0, col - 1);
            let removed = buf.delete(start, Pos::new(0, col));
            undo.record(
                Edit {
                    start,
                    removed,
                    inserted: String::new(),
                },
                EditKind::Backspace,
                (Pos::new(0, col), Pos::new(0, col)),
                (start, start),
            );
        }
        assert_eq!(buf.line(0), "");
        let sel = undo.undo(&mut buf);
        assert_eq!(buf.line(0), "abcd");
        assert_eq!(sel, Some((Pos::new(0, 4), Pos::new(0, 4))));
    }

    #[test]
    fn replace_across_lines_round_trips() {
        let mut buf = Buffer::from_text("one\ntwo\nthree");
        let mut undo = UndoStack::default();
        let start = Pos::new(0, 1);
        let removed = buf.delete(start, Pos::new(2, 2));
        buf.insert(start, "X\nY");
        undo.record(
            Edit {
                start,
                removed,
                inserted: "X\nY".into(),
            },
            EditKind::Other,
            (start, start),
            (start, start),
        );
        assert_eq!(buf.to_file_text(), "oX\nYree");
        undo.undo(&mut buf);
        assert_eq!(buf.to_file_text(), "one\ntwo\nthree");
        undo.redo(&mut buf);
        assert_eq!(buf.to_file_text(), "oX\nYree");
    }

    #[test]
    fn state_id_tracks_save_point_and_seal() {
        let mut buf = Buffer::from_text("");
        let mut undo = UndoStack::default();
        let p = type_str(&mut buf, &mut undo, Pos::new(0, 0), "ab");
        let saved = undo.state_id();
        undo.seal();
        type_str(&mut buf, &mut undo, p, "c");
        assert_ne!(undo.state_id(), saved);
        undo.undo(&mut buf);
        assert_eq!(undo.state_id(), saved);
        assert_eq!(buf.line(0), "ab");
    }

    #[test]
    fn new_edit_clears_redo() {
        let mut buf = Buffer::from_text("");
        let mut undo = UndoStack::default();
        type_str(&mut buf, &mut undo, Pos::new(0, 0), "a");
        undo.undo(&mut buf);
        type_str(&mut buf, &mut undo, Pos::new(0, 0), "b");
        assert!(undo.redo(&mut buf).is_none());
        assert_eq!(buf.line(0), "b");
    }
}
