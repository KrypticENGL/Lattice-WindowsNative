//! The text and caret of a one-line name box.

use crate::event::{Key, Modifiers};

pub struct NameField {
    text: String,
    /// Byte offsets, on character boundaries. Equal when nothing is selected.
    caret: usize,
    anchor: usize,
}

impl NameField {
    /// Holds `text` with its first `selected` bytes selected.
    pub fn new(text: &str, selected: usize) -> Self {
        Self {
            text: text.to_owned(),
            caret: selected,
            anchor: 0,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    /// The selected range, in order.
    pub fn selection(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    pub fn selected_text(&self) -> &str {
        let (s, e) = self.selection();
        &self.text[s..e]
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }

    /// Replaces the selection with `s`, minus any control characters (a
    /// pasted newline, say).
    pub fn insert(&mut self, s: &str) {
        let clean: String = s.chars().filter(|c| !c.is_control()).collect();
        let (start, end) = self.selection();
        self.text.replace_range(start..end, &clean);
        self.caret = start + clean.len();
        self.anchor = self.caret;
    }

    fn prev(&self) -> usize {
        self.text[..self.caret]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn next(&self) -> usize {
        self.text[self.caret..]
            .chars()
            .next()
            .map_or(self.caret, |c| self.caret + c.len_utf8())
    }

    /// Editing and caret keys. Returns whether the key was one of them.
    pub fn key(&mut self, key: Key, mods: Modifiers) -> bool {
        let selected = self.caret != self.anchor;
        match key {
            Key::Backspace | Key::Delete if selected => self.insert(""),
            Key::Backspace => {
                self.anchor = self.prev();
                self.insert("");
            }
            Key::Delete => {
                self.anchor = self.next();
                self.insert("");
            }
            Key::Left | Key::Right | Key::Home | Key::End => {
                let (start, end) = self.selection();
                self.caret = match key {
                    Key::Home => 0,
                    Key::End => self.text.len(),
                    Key::Left if selected && !mods.shift => start,
                    Key::Right if selected && !mods.shift => end,
                    Key::Left => self.prev(),
                    _ => self.next(),
                };
                if !mods.shift {
                    self.anchor = self.caret;
                }
            }
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers::NONE;

    #[test]
    fn typing_replaces_the_selected_stem() {
        let mut f = NameField::new("main.cpp", 4);
        assert_eq!(f.selected_text(), "main");
        f.insert("app");
        assert_eq!(f.text(), "app.cpp");
        assert_eq!(f.caret(), 3);
        assert_eq!(f.selected_text(), "");
    }

    #[test]
    fn keys_respect_characters_and_selection() {
        let mut f = NameField::new("é€z", 0);
        f.key(Key::End, NONE);
        f.key(Key::Left, NONE);
        f.key(Key::Backspace, NONE);
        assert_eq!(f.text(), "éz");
        f.key(Key::Home, NONE);
        f.key(Key::Delete, NONE);
        assert_eq!(f.text(), "z");
        // At the ends these do nothing, and don't panic.
        f.key(Key::Home, NONE);
        f.key(Key::Backspace, NONE);
        f.key(Key::End, NONE);
        f.key(Key::Delete, NONE);
        assert_eq!(f.text(), "z");

        let mut f = NameField::new("abcd", 0);
        f.key(Key::Right, Modifiers::SHIFT);
        f.key(Key::Right, Modifiers::SHIFT);
        assert_eq!(f.selected_text(), "ab");
        f.key(Key::Delete, NONE);
        assert_eq!(f.text(), "cd");
        f.select_all();
        f.key(Key::Left, NONE);
        assert_eq!((f.caret(), f.selected_text()), (0, ""));
        assert!(!f.key(Key::Up, NONE));
    }

    #[test]
    fn paste_drops_line_breaks() {
        let mut f = NameField::new("", 0);
        f.insert("a\r\nb\t.c");
        assert_eq!(f.text(), "ab.c");
    }
}
