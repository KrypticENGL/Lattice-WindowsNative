//! The word completion list under the caret. It offers the words of the open
//! files and the language's keywords.
//!
//! shortcut: matches text, not meaning, so it knows nothing of scopes, members
//! after `.`/`->` or unopened headers; talk to a language server (clangd) if
//! that is wanted.

use std::collections::BTreeSet;

use crate::geom::{PointF, RectF};
use crate::syntax::{self, is_word};
use crate::sys::gfx::DrawCtx;
use crate::sys::text::{TextStyle, TextSystem};
use crate::theme::Theme;

const MAX_ITEMS: usize = 50;
const MAX_ROWS: usize = 8;
const PAD: f32 = 8.0;

/// The words in `text`; ones starting with a digit are numbers.
pub fn words_in(text: impl Iterator<Item = impl AsRef<str>>) -> BTreeSet<String> {
    let mut words = BTreeSet::new();
    for line in text {
        for w in line
            .as_ref()
            .split(|c: char| c.is_ascii() && !is_word(c as u8))
        {
            if w.len() >= 2 && !w.as_bytes()[0].is_ascii_digit() {
                words.insert(w.to_string());
            }
        }
    }
    words
}

/// What to offer for `prefix`: exact-case matches first, then the others,
/// each alphabetically. The prefix itself is not offered.
pub fn candidates<'a>(
    prefix: &str,
    keywords: bool,
    sets: impl Iterator<Item = &'a BTreeSet<String>>,
) -> Vec<String> {
    let lower = prefix.to_ascii_lowercase();
    let mut found = BTreeSet::new();
    let mut consider = |w: &str| {
        let matches = w.len() >= prefix.len()
            && w.is_char_boundary(prefix.len())
            && w[..prefix.len()].eq_ignore_ascii_case(&lower);
        if matches && w != prefix {
            found.insert((
                !w.starts_with(prefix),
                w.to_ascii_lowercase(),
                w.to_string(),
            ));
        }
    };
    if keywords {
        syntax::KEYWORDS
            .iter()
            .chain(syntax::TYPES)
            .for_each(|w| consider(w));
    }
    for set in sets {
        set.iter().for_each(|w| consider(w));
    }
    found
        .into_iter()
        .take(MAX_ITEMS)
        .map(|(_, _, w)| w)
        .collect()
}

pub struct Completion {
    items: Vec<String>,
    selected: usize,
    /// Where the prefix being completed starts on the caret's line.
    pub start_col: usize,
    rect: RectF,
    row_h: f32,
}

impl Completion {
    /// A list by the caret, inside `body`; `None` if there is nothing to offer.
    pub fn new(
        items: Vec<String>,
        start_col: usize,
        caret: RectF,
        body: RectF,
        text: &TextSystem,
    ) -> Option<Self> {
        if items.is_empty() {
            return None;
        }
        let cell = text.cell_size();
        let row_h = caret.h;
        let longest = items.iter().map(|w| w.chars().count()).max().unwrap_or(0);
        let w = (longest as f32 * cell.w + 2.0 * PAD).min(body.w);
        let h = items.len().min(MAX_ROWS) as f32 * row_h;
        // Below the caret, or above it if only that fits.
        let y = if caret.bottom() + h <= body.bottom() {
            caret.bottom()
        } else {
            (caret.y - h).max(body.y)
        };
        let x = caret.x.min(body.right() - w).max(body.x);
        Some(Self {
            items,
            selected: 0,
            start_col,
            rect: RectF::new(x, y, w, h),
            row_h,
        })
    }

    pub fn selected_word(&self) -> &str {
        &self.items[self.selected]
    }

    /// Moves the selection, wrapping at the ends.
    pub fn move_by(&mut self, delta: isize) {
        let n = self.items.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
    }

    /// The first row shown: the list scrolls to keep the selection in view.
    fn first(&self) -> usize {
        (self.selected + 1).saturating_sub(MAX_ROWS)
    }

    pub fn contains(&self, p: PointF) -> bool {
        self.rect.contains(p)
    }

    /// Selects the row at `p`.
    pub fn select_at(&mut self, p: PointF) {
        let row = ((p.y - self.rect.y) / self.row_h) as usize;
        self.selected = (self.first() + row).min(self.items.len() - 1);
    }

    pub fn paint(&self, d: &mut DrawCtx, theme: &Theme) {
        d.fill_round_rect(self.rect, 4.0, theme.panel_bg);
        let first = self.first();
        d.clip(self.rect, |d| {
            for (i, word) in self.items.iter().enumerate().skip(first).take(MAX_ROWS) {
                let y = self.rect.y + (i - first) as f32 * self.row_h;
                let row = RectF::new(self.rect.x, y, self.rect.w, self.row_h);
                let selected = i == self.selected;
                if selected {
                    d.fill_rect(row, theme.selection_bg);
                }
                let ink = if selected {
                    theme.text_bright
                } else {
                    theme.text
                };
                let text = RectF::new(row.x + PAD, y, row.w - PAD, row.h);
                d.text(TextStyle::Mono, word, text, ink);
            }
        });
        d.stroke_rect(self.rect, theme.border, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_words() {
        let set = words_in(["int counter = 10 + x2;", "é_y"].iter());
        let got: Vec<&str> = set.iter().map(String::as_str).collect();
        assert_eq!(got, ["counter", "int", "x2", "\u{e9}_y"]);
    }

    #[test]
    fn ranks_matches() {
        let set = words_in(["Count counter count country other"].iter());
        assert_eq!(
            candidates("cou", false, [&set].into_iter()),
            ["count", "counter", "country", "Count"]
        );
        // The word being typed is not offered back.
        assert_eq!(
            candidates("count", false, [&set].into_iter()),
            ["counter", "country", "Count"]
        );
        assert_eq!(
            candidates("whi", true, [&set].into_iter()),
            ["while"],
            "keywords"
        );
        assert!(candidates("whi", false, [&set].into_iter()).is_empty());
    }

    #[test]
    fn non_ascii_words_do_not_split_mid_character() {
        let set = BTreeSet::from(["éa".to_string(), "ab".to_string()]);
        assert_eq!(candidates("a", false, [&set].into_iter()), ["ab"]);
    }

    #[test]
    fn selection_wraps_and_scrolls() {
        let text = TextSystem::new().expect("DirectWrite");
        let items: Vec<String> = (0..20).map(|i| format!("w{i}")).collect();
        let caret = RectF::new(10.0, 10.0, 2.0, 18.0);
        let body = RectF::new(0.0, 0.0, 800.0, 600.0);
        let mut c = Completion::new(items, 0, caret, body, &text).unwrap();
        c.move_by(-1);
        assert_eq!(c.selected_word(), "w19");
        assert_eq!(c.first(), 12);
        c.select_at(PointF::new(20.0, c.rect.y + 1.5 * c.row_h));
        assert_eq!(c.selected_word(), "w13");
        assert!(Completion::new(Vec::new(), 0, caret, body, &text).is_none());
    }

    #[test]
    fn stays_inside_the_editor() {
        let text = TextSystem::new().expect("DirectWrite");
        let body = RectF::new(0.0, 0.0, 300.0, 100.0);
        let caret = RectF::new(290.0, 80.0, 2.0, 18.0);
        let c = Completion::new(vec!["word".into()], 0, caret, body, &text).unwrap();
        assert!(c.rect.right() <= body.right());
        assert!(c.rect.bottom() <= caret.y, "flips above the caret");
    }
}
