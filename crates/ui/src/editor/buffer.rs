//! Text buffer: a vector of lines. Positions are (line, byte column) and
//! always sit on a char boundary. Every modified line gets a fresh id, so
//! view caches keyed by line id never see stale text.

/// Position in the buffer. `col` is a byte offset into the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    pub const fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
}

impl LineEnding {
    fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
        }
    }
}

#[derive(Debug, Clone)]
struct Line {
    id: u64,
    text: String,
}

#[derive(Debug, Clone)]
pub struct Buffer {
    lines: Vec<Line>,
    next_id: u64,
    ending: LineEnding,
    bom: bool,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::from_text("")
    }
}

/// Converts any newline convention to `\n`.
pub fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Position just after `text` when it is inserted at `start`.
pub fn end_after(start: Pos, text: &str) -> Pos {
    match text.rfind('\n') {
        None => Pos::new(start.line, start.col + text.len()),
        Some(i) => Pos::new(start.line + text.matches('\n').count(), text.len() - i - 1),
    }
}

/// UTF-16 offset of byte offset `byte` in `s`.
pub fn byte_to_utf16(s: &str, byte: usize) -> usize {
    s[..byte.min(s.len())].encode_utf16().count()
}

/// Byte offset of UTF-16 offset `u16_pos` in `s` (rounded down to a char).
pub fn utf16_to_byte(s: &str, u16_pos: usize) -> usize {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        if units >= u16_pos {
            return i;
        }
        units += c.len_utf16();
        if units > u16_pos {
            return i;
        }
    }
    s.len()
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Space,
    Word,
    Punct,
}

fn class(c: char) -> CharClass {
    if c.is_whitespace() {
        CharClass::Space
    } else if is_word(c) {
        CharClass::Word
    } else {
        CharClass::Punct
    }
}

impl Buffer {
    /// Parses file text. A leading BOM is stripped and remembered; the line
    /// ending is taken from the first newline (default LF).
    pub fn from_text(text: &str) -> Self {
        let (bom, text) = match text.strip_prefix('\u{feff}') {
            Some(rest) => (true, rest),
            None => (false, text),
        };
        let ending = match text.find('\n') {
            Some(i) if i > 0 && text.as_bytes()[i - 1] == b'\r' => LineEnding::CrLf,
            _ => LineEnding::Lf,
        };
        let mut buffer = Self {
            lines: Vec::new(),
            next_id: 0,
            ending,
            bom,
        };
        let normalized = normalize_newlines(text);
        let lines: Vec<Line> = normalized
            .split('\n')
            .map(|l| Line {
                id: buffer.fresh_id(),
                text: l.to_owned(),
            })
            .collect();
        buffer.lines = lines;
        buffer
    }

    /// Serialises with the original line ending and BOM.
    pub fn to_file_text(&self) -> String {
        let mut out = String::new();
        if self.bom {
            out.push('\u{feff}');
        }
        for (i, line) in self.lines.iter().enumerate() {
            if i > 0 {
                out.push_str(self.ending.as_str());
            }
            out.push_str(&line.text);
        }
        out
    }

    fn fresh_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, i: usize) -> &str {
        self.lines.get(i).map_or("", |l| l.text.as_str())
    }

    pub fn line_id(&self, i: usize) -> u64 {
        self.lines.get(i).map_or(0, |l| l.id)
    }

    pub fn line_len(&self, i: usize) -> usize {
        self.line(i).len()
    }

    pub fn end(&self) -> Pos {
        let last = self.lines.len() - 1;
        Pos::new(last, self.line_len(last))
    }

    /// Clamps to a valid position on a char boundary.
    pub fn clamp(&self, p: Pos) -> Pos {
        let line = p.line.min(self.lines.len() - 1);
        let text = self.line(line);
        let mut col = p.col.min(text.len());
        while !text.is_char_boundary(col) {
            col -= 1;
        }
        Pos::new(line, col)
    }

    /// Text between two positions (any order), lines joined with `\n`.
    pub fn text_range(&self, a: Pos, b: Pos) -> String {
        let (a, b) = order(self.clamp(a), self.clamp(b));
        if a.line == b.line {
            return self.line(a.line)[a.col..b.col].to_owned();
        }
        let mut out = self.line(a.line)[a.col..].to_owned();
        for i in a.line + 1..b.line {
            out.push('\n');
            out.push_str(self.line(i));
        }
        out.push('\n');
        out.push_str(&self.line(b.line)[..b.col]);
        out
    }

    /// Inserts `text` (newlines normalised) at `pos`; returns the end position.
    pub fn insert(&mut self, pos: Pos, text: &str) -> Pos {
        let pos = self.clamp(pos);
        let text = normalize_newlines(text);
        let mut parts = text.split('\n');
        let first = parts.next().unwrap_or("");
        let rest: Vec<&str> = parts.collect();

        let id = self.fresh_id();
        let line = &mut self.lines[pos.line];
        line.id = id;
        let tail = line.text.split_off(pos.col);
        line.text.push_str(first);
        let Some((last, middle)) = rest.split_last() else {
            line.text.push_str(&tail);
            return Pos::new(pos.line, pos.col + first.len());
        };
        let mut new_lines = Vec::with_capacity(rest.len());
        for m in middle {
            new_lines.push(Line {
                id: self.fresh_id(),
                text: (*m).to_owned(),
            });
        }
        let end = Pos::new(pos.line + rest.len(), last.len());
        new_lines.push(Line {
            id: self.fresh_id(),
            text: format!("{last}{tail}"),
        });
        self.lines.splice(pos.line + 1..pos.line + 1, new_lines);
        end
    }

    /// Deletes between two positions (any order); returns the removed text.
    pub fn delete(&mut self, a: Pos, b: Pos) -> String {
        let (a, b) = order(self.clamp(a), self.clamp(b));
        if a == b {
            return String::new();
        }
        let removed = self.text_range(a, b);
        let id = self.fresh_id();
        if a.line == b.line {
            let line = &mut self.lines[a.line];
            line.text.replace_range(a.col..b.col, "");
            line.id = id;
        } else {
            let tail = self.lines[b.line].text[b.col..].to_owned();
            let first = &mut self.lines[a.line];
            first.text.truncate(a.col);
            first.text.push_str(&tail);
            first.id = id;
            self.lines.drain(a.line + 1..=b.line);
        }
        removed
    }

    /// One char back, crossing to the previous line end.
    pub fn prev_pos(&self, p: Pos) -> Pos {
        let p = self.clamp(p);
        if p.col == 0 {
            return if p.line == 0 {
                p
            } else {
                Pos::new(p.line - 1, self.line_len(p.line - 1))
            };
        }
        let text = self.line(p.line);
        let col = text[..p.col].char_indices().last().map_or(0, |(i, _)| i);
        Pos::new(p.line, col)
    }

    /// One char forward, crossing to the next line start.
    pub fn next_pos(&self, p: Pos) -> Pos {
        let p = self.clamp(p);
        let text = self.line(p.line);
        if p.col >= text.len() {
            return if p.line + 1 >= self.lines.len() {
                p
            } else {
                Pos::new(p.line + 1, 0)
            };
        }
        let c = text[p.col..].chars().next().map_or(1, char::len_utf8);
        Pos::new(p.line, p.col + c)
    }

    /// Start of the previous word (Ctrl+Left).
    pub fn word_left(&self, p: Pos) -> Pos {
        let p = self.clamp(p);
        if p.col == 0 {
            return self.prev_pos(p);
        }
        let text = self.line(p.line);
        let chars: Vec<(usize, char)> = text[..p.col].char_indices().collect();
        let mut i = chars.len();
        while i > 0 && class(chars[i - 1].1) == CharClass::Space {
            i -= 1;
        }
        if i > 0 {
            let k = class(chars[i - 1].1);
            while i > 0 && class(chars[i - 1].1) == k {
                i -= 1;
            }
        }
        Pos::new(p.line, chars.get(i).map_or(0, |&(b, _)| b))
    }

    /// End of the next word (Ctrl+Right).
    pub fn word_right(&self, p: Pos) -> Pos {
        let p = self.clamp(p);
        let text = self.line(p.line);
        if p.col >= text.len() {
            return self.next_pos(p);
        }
        let mut iter = text[p.col..].char_indices().peekable();
        while let Some(&(_, c)) = iter.peek() {
            if class(c) != CharClass::Space {
                break;
            }
            iter.next();
        }
        if let Some(&(_, c)) = iter.peek() {
            let k = class(c);
            while let Some(&(_, c)) = iter.peek() {
                if class(c) != k {
                    break;
                }
                iter.next();
            }
        }
        let off = iter.peek().map_or(text.len() - p.col, |&(i, _)| i);
        Pos::new(p.line, p.col + off)
    }

    /// The word (or run of same-class chars) containing `p`.
    pub fn word_at(&self, p: Pos) -> (Pos, Pos) {
        let p = self.clamp(p);
        let text = self.line(p.line);
        let target = text[p.col..]
            .chars()
            .next()
            .or_else(|| text[..p.col].chars().last());
        let Some(target) = target else {
            return (p, p);
        };
        let k = class(target);
        let start = text[..p.col]
            .char_indices()
            .rev()
            .take_while(|&(_, c)| class(c) == k)
            .last()
            .map_or(p.col, |(i, _)| i);
        let end = text[p.col..]
            .char_indices()
            .find(|&(_, c)| class(c) != k)
            .map_or(text.len(), |(i, _)| p.col + i);
        (Pos::new(p.line, start), Pos::new(p.line, end))
    }

    /// Byte length of the leading whitespace of line `i`.
    pub fn indent_len(&self, i: usize) -> usize {
        let text = self.line(i);
        text.len() - text.trim_start_matches([' ', '\t']).len()
    }
}

pub fn order(a: Pos, b: Pos) -> (Pos, Pos) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_round_trip_and_bom() {
        let text = "\u{feff}one\r\ntwo\r\n";
        let b = Buffer::from_text(text);
        assert_eq!(b.line_count(), 3);
        assert_eq!(b.line(1), "two");
        assert_eq!(b.to_file_text(), text);
    }

    #[test]
    fn lf_round_trip() {
        let text = "a\nb\n\nc";
        assert_eq!(Buffer::from_text(text).to_file_text(), text);
    }

    #[test]
    fn insert_splits_and_returns_end() {
        let mut b = Buffer::from_text("hello world");
        let end = b.insert(Pos::new(0, 5), ",\nnew\nline");
        assert_eq!(end, Pos::new(2, 4));
        assert_eq!(b.line(0), "hello,");
        assert_eq!(b.line(1), "new");
        assert_eq!(b.line(2), "line world");
        assert_eq!(end_after(Pos::new(0, 5), ",\nnew\nline"), end);
    }

    #[test]
    fn delete_joins_lines_and_returns_text() {
        let mut b = Buffer::from_text("abc\ndef\nghi");
        let removed = b.delete(Pos::new(2, 1), Pos::new(0, 1));
        assert_eq!(removed, "bc\ndef\ng");
        assert_eq!(b.line_count(), 1);
        assert_eq!(b.line(0), "ahi");
    }

    #[test]
    fn modified_lines_get_fresh_ids() {
        let mut b = Buffer::from_text("x\ny");
        let (id0, id1) = (b.line_id(0), b.line_id(1));
        b.insert(Pos::new(0, 1), "!");
        assert_ne!(b.line_id(0), id0);
        assert_eq!(b.line_id(1), id1);
    }

    #[test]
    fn movement_respects_multibyte_chars() {
        let b = Buffer::from_text("aé😀b");
        let p = b.next_pos(Pos::new(0, 1));
        assert_eq!(p, Pos::new(0, 3));
        let p = b.next_pos(p);
        assert_eq!(p, Pos::new(0, 7));
        assert_eq!(b.prev_pos(p), Pos::new(0, 3));
        assert_eq!(b.clamp(Pos::new(0, 5)), Pos::new(0, 3));
    }

    #[test]
    fn movement_crosses_lines() {
        let b = Buffer::from_text("ab\ncd");
        assert_eq!(b.next_pos(Pos::new(0, 2)), Pos::new(1, 0));
        assert_eq!(b.prev_pos(Pos::new(1, 0)), Pos::new(0, 2));
        assert_eq!(b.prev_pos(Pos::new(0, 0)), Pos::new(0, 0));
        assert_eq!(b.next_pos(b.end()), b.end());
    }

    #[test]
    fn word_motion() {
        let b = Buffer::from_text("let foo_bar = 42;");
        assert_eq!(b.word_right(Pos::new(0, 0)), Pos::new(0, 3));
        assert_eq!(b.word_right(Pos::new(0, 3)), Pos::new(0, 11));
        assert_eq!(b.word_left(Pos::new(0, 11)), Pos::new(0, 4));
        assert_eq!(b.word_left(Pos::new(0, 4)), Pos::new(0, 0));
        assert_eq!(b.word_at(Pos::new(0, 6)), (Pos::new(0, 4), Pos::new(0, 11)));
    }

    #[test]
    fn utf16_conversions() {
        let s = "a😀b";
        assert_eq!(byte_to_utf16(s, 5), 3);
        assert_eq!(utf16_to_byte(s, 3), 5);
        assert_eq!(utf16_to_byte(s, 2), 1);
        assert_eq!(utf16_to_byte(s, 99), s.len());
    }
}
