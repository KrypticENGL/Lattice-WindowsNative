//! Terminal screen model driven by `vte`: a cell grid with scrollback,
//! scroll region, alternate screen and the VT subset ConPTY emits.

use std::collections::VecDeque;

use vte::{Params, Perform};

pub const SCROLLBACK_LINES: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TermColor {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Attr {
    pub fg: TermColor,
    pub bg: TermColor,
    pub bold: bool,
    pub inverse: bool,
    pub underline: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub attr: Attr,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            attr: Attr::default(),
        }
    }
}

type Row = Vec<Cell>;

#[derive(Debug, Clone, Copy, Default)]
struct SavedCursor {
    row: usize,
    col: usize,
    attr: Attr,
}

/// The main screen while the alternate screen is active.
#[derive(Debug, Clone)]
struct SavedScreen {
    rows: Vec<Row>,
    cursor: SavedCursor,
}

#[derive(Debug, Clone)]
pub struct Grid {
    cols: usize,
    rows: usize,
    screen: Vec<Row>,
    scrollback: VecDeque<Row>,
    alt: Option<SavedScreen>,
    cursor_row: usize,
    cursor_col: usize,
    /// Set after printing in the last column; the next print wraps.
    wrap_pending: bool,
    attr: Attr,
    saved: SavedCursor,
    top: usize,
    bottom: usize,
    cursor_visible: bool,
    app_cursor_keys: bool,
    bracketed_paste: bool,
    /// Replies to device queries, to be written back to the PTY.
    responses: Vec<u8>,
}

impl Grid {
    pub fn new(cols: usize, rows: usize) -> Self {
        let (cols, rows) = (cols.max(1), rows.max(1));
        Self {
            cols,
            rows,
            screen: vec![vec![Cell::default(); cols]; rows],
            scrollback: VecDeque::new(),
            alt: None,
            cursor_row: 0,
            cursor_col: 0,
            wrap_pending: false,
            attr: Attr::default(),
            saved: SavedCursor::default(),
            top: 0,
            bottom: rows - 1,
            cursor_visible: true,
            app_cursor_keys: false,
            bracketed_paste: false,
            responses: Vec::new(),
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn cursor(&self) -> (usize, usize) {
        (self.cursor_row, self.cursor_col)
    }

    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    pub fn app_cursor_keys(&self) -> bool {
        self.app_cursor_keys
    }

    pub fn bracketed_paste(&self) -> bool {
        self.bracketed_paste
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    pub fn take_responses(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.responses)
    }

    /// Row `view_row` of the viewport scrolled `offset` lines into history.
    pub fn view_row(&self, view_row: usize, offset: usize) -> &[Cell] {
        let offset = offset.min(self.scrollback.len());
        if view_row < offset {
            &self.scrollback[self.scrollback.len() - offset + view_row]
        } else {
            &self.screen[view_row - offset]
        }
    }

    /// Plain text of screen row `row` (trailing spaces trimmed); for tests.
    #[cfg(test)]
    pub fn row_text(&self, row: usize) -> String {
        let s: String = self.screen[row].iter().map(|c| c.ch).collect();
        s.trim_end().to_owned()
    }

    fn blank(&self) -> Cell {
        Cell {
            ch: ' ',
            attr: Attr {
                bg: self.attr.bg,
                ..Attr::default()
            },
        }
    }

    fn blank_row(&self) -> Row {
        vec![self.blank(); self.cols]
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if cols == self.cols && rows == self.rows {
            return;
        }
        // Keep the cursor on screen: push surplus top rows into history.
        if self.cursor_row >= rows {
            let excess = self.cursor_row + 1 - rows;
            for row in self.screen.drain(..excess) {
                if self.alt.is_none() {
                    self.scrollback.push_back(row);
                }
            }
            self.cursor_row -= excess;
        }
        self.screen.truncate(rows);
        for row in &mut self.screen {
            row.resize(cols, Cell::default());
        }
        while self.screen.len() < rows {
            self.screen.push(vec![Cell::default(); cols]);
        }
        if let Some(alt) = &mut self.alt {
            alt.rows.truncate(rows);
            for row in &mut alt.rows {
                row.resize(cols, Cell::default());
            }
            while alt.rows.len() < rows {
                alt.rows.push(vec![Cell::default(); cols]);
            }
        }
        while self.scrollback.len() > SCROLLBACK_LINES {
            self.scrollback.pop_front();
        }
        self.cols = cols;
        self.rows = rows;
        self.top = 0;
        self.bottom = rows - 1;
        self.cursor_row = self.cursor_row.min(rows - 1);
        self.cursor_col = self.cursor_col.min(cols - 1);
        self.wrap_pending = false;
    }

    fn scroll_up(&mut self, n: usize) {
        for _ in 0..n.min(self.bottom - self.top + 1) {
            let row = self.screen.remove(self.top);
            if self.top == 0 && self.alt.is_none() {
                self.scrollback.push_back(row);
                if self.scrollback.len() > SCROLLBACK_LINES {
                    self.scrollback.pop_front();
                }
            }
            let blank = self.blank_row();
            self.screen.insert(self.bottom, blank);
        }
    }

    fn scroll_down(&mut self, n: usize) {
        for _ in 0..n.min(self.bottom - self.top + 1) {
            self.screen.remove(self.bottom);
            let blank = self.blank_row();
            self.screen.insert(self.top, blank);
        }
    }

    fn linefeed(&mut self) {
        if self.cursor_row == self.bottom {
            self.scroll_up(1);
        } else if self.cursor_row + 1 < self.rows {
            self.cursor_row += 1;
        }
    }

    fn reverse_index(&mut self) {
        if self.cursor_row == self.top {
            self.scroll_down(1);
        } else {
            self.cursor_row = self.cursor_row.saturating_sub(1);
        }
    }

    fn goto(&mut self, row: usize, col: usize) {
        self.cursor_row = row.min(self.rows - 1);
        self.cursor_col = col.min(self.cols - 1);
        self.wrap_pending = false;
    }

    fn erase_cells(&mut self, row: usize, from: usize, to: usize) {
        let blank = self.blank();
        let to = to.min(self.cols);
        for cell in &mut self.screen[row][from.min(to)..to] {
            *cell = blank;
        }
    }

    fn erase_display(&mut self, mode: u16) {
        let (row, col) = (self.cursor_row, self.cursor_col);
        match mode {
            0 => {
                self.erase_cells(row, col, self.cols);
                for r in row + 1..self.rows {
                    self.erase_cells(r, 0, self.cols);
                }
            }
            1 => {
                for r in 0..row {
                    self.erase_cells(r, 0, self.cols);
                }
                self.erase_cells(row, 0, col + 1);
            }
            2 | 3 => {
                for r in 0..self.rows {
                    self.erase_cells(r, 0, self.cols);
                }
                if mode == 3 {
                    self.scrollback.clear();
                }
            }
            _ => {}
        }
    }

    fn erase_line(&mut self, mode: u16) {
        let (row, col) = (self.cursor_row, self.cursor_col);
        match mode {
            0 => self.erase_cells(row, col, self.cols),
            1 => self.erase_cells(row, 0, col + 1),
            2 => self.erase_cells(row, 0, self.cols),
            _ => {}
        }
    }

    fn insert_chars(&mut self, n: usize) {
        let (row, col) = (self.cursor_row, self.cursor_col);
        let blank = self.blank();
        let line = &mut self.screen[row];
        for _ in 0..n.min(self.cols - col) {
            line.insert(col, blank);
            line.pop();
        }
    }

    fn delete_chars(&mut self, n: usize) {
        let (row, col) = (self.cursor_row, self.cursor_col);
        let blank = self.blank();
        let line = &mut self.screen[row];
        for _ in 0..n.min(self.cols - col) {
            line.remove(col);
            line.push(blank);
        }
    }

    fn insert_lines(&mut self, n: usize) {
        if self.cursor_row < self.top || self.cursor_row > self.bottom {
            return;
        }
        for _ in 0..n.min(self.bottom - self.cursor_row + 1) {
            self.screen.remove(self.bottom);
            let blank = self.blank_row();
            self.screen.insert(self.cursor_row, blank);
        }
        self.cursor_col = 0;
    }

    fn delete_lines(&mut self, n: usize) {
        if self.cursor_row < self.top || self.cursor_row > self.bottom {
            return;
        }
        for _ in 0..n.min(self.bottom - self.cursor_row + 1) {
            self.screen.remove(self.cursor_row);
            let blank = self.blank_row();
            self.screen.insert(self.bottom, blank);
        }
        self.cursor_col = 0;
    }

    fn save_cursor(&mut self) {
        self.saved = SavedCursor {
            row: self.cursor_row,
            col: self.cursor_col,
            attr: self.attr,
        };
    }

    fn restore_cursor(&mut self) {
        let s = self.saved;
        self.attr = s.attr;
        self.goto(s.row, s.col);
    }

    fn set_alt_screen(&mut self, on: bool) {
        if on && self.alt.is_none() {
            self.save_cursor();
            let blank = vec![vec![Cell::default(); self.cols]; self.rows];
            let main = std::mem::replace(&mut self.screen, blank);
            self.alt = Some(SavedScreen {
                rows: main,
                cursor: self.saved,
            });
            self.goto(0, 0);
        } else if !on {
            if let Some(main) = self.alt.take() {
                self.screen = main.rows;
                self.saved = main.cursor;
                self.restore_cursor();
            }
        }
    }

    fn set_private_mode(&mut self, mode: u16, on: bool) {
        match mode {
            1 => self.app_cursor_keys = on,
            25 => self.cursor_visible = on,
            47 | 1047 | 1049 => self.set_alt_screen(on),
            2004 => self.bracketed_paste = on,
            _ => {}
        }
    }

    fn sgr(&mut self, params: &[u16]) {
        if params.is_empty() {
            self.attr = Attr::default();
            return;
        }
        let mut i = 0;
        while i < params.len() {
            let p = params[i];
            match p {
                0 => self.attr = Attr::default(),
                1 => self.attr.bold = true,
                4 => self.attr.underline = true,
                7 => self.attr.inverse = true,
                22 => self.attr.bold = false,
                24 => self.attr.underline = false,
                27 => self.attr.inverse = false,
                30..=37 => self.attr.fg = TermColor::Indexed((p - 30) as u8),
                39 => self.attr.fg = TermColor::Default,
                40..=47 => self.attr.bg = TermColor::Indexed((p - 40) as u8),
                49 => self.attr.bg = TermColor::Default,
                90..=97 => self.attr.fg = TermColor::Indexed((p - 90 + 8) as u8),
                100..=107 => self.attr.bg = TermColor::Indexed((p - 100 + 8) as u8),
                38 | 48 => {
                    let (color, used) = extended_color(&params[i + 1..]);
                    if let Some(c) = color {
                        if p == 38 {
                            self.attr.fg = c;
                        } else {
                            self.attr.bg = c;
                        }
                    }
                    i += used;
                }
                _ => {}
            }
            i += 1;
        }
    }
}

/// Parses `5;n` or `2;r;g;b` after a 38/48. Returns the colour and how many
/// parameters were consumed.
fn extended_color(rest: &[u16]) -> (Option<TermColor>, usize) {
    match rest.first() {
        Some(5) if rest.len() >= 2 => (Some(TermColor::Indexed(rest[1] as u8)), 2),
        Some(2) if rest.len() >= 4 => (
            Some(TermColor::Rgb(rest[1] as u8, rest[2] as u8, rest[3] as u8)),
            4,
        ),
        _ => (None, rest.len()),
    }
}

/// Flattens params; `38:5:n` (sub-params) and `38;5;n` read the same.
fn flat_params(params: &Params) -> Vec<u16> {
    params.iter().flat_map(|p| p.iter().copied()).collect()
}

/// First value of each parameter, with 0 meaning "default".
fn first_params(params: &Params) -> Vec<u16> {
    params
        .iter()
        .map(|p| p.first().copied().unwrap_or(0))
        .collect()
}

impl Perform for Grid {
    fn print(&mut self, c: char) {
        if self.wrap_pending {
            self.cursor_col = 0;
            self.linefeed();
            self.wrap_pending = false;
        }
        let attr = self.attr;
        self.screen[self.cursor_row][self.cursor_col] = Cell { ch: c, attr };
        if self.cursor_col + 1 >= self.cols {
            self.wrap_pending = true;
        } else {
            self.cursor_col += 1;
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x08 => {
                self.cursor_col = self.cursor_col.saturating_sub(1);
                self.wrap_pending = false;
            }
            0x09 => {
                let next = (self.cursor_col / 8 + 1) * 8;
                self.cursor_col = next.min(self.cols - 1);
                self.wrap_pending = false;
            }
            0x0A..=0x0C => {
                self.linefeed();
                self.wrap_pending = false;
            }
            0x0D => {
                self.cursor_col = 0;
                self.wrap_pending = false;
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        let private = intermediates.first() == Some(&b'?');
        let ps = first_params(params);
        let arg = |i: usize, default: u16| -> usize {
            usize::from(ps.get(i).copied().filter(|&v| v != 0).unwrap_or(default))
        };
        let raw = |i: usize| ps.get(i).copied().unwrap_or(0);
        let (row, col) = (self.cursor_row, self.cursor_col);
        match (action, private) {
            ('A', false) => self.goto(row.saturating_sub(arg(0, 1)), col),
            ('B', false) => self.goto(row + arg(0, 1), col),
            ('C', false) => self.goto(row, col + arg(0, 1)),
            ('D', false) => self.goto(row, col.saturating_sub(arg(0, 1))),
            ('E', false) => self.goto(row + arg(0, 1), 0),
            ('F', false) => self.goto(row.saturating_sub(arg(0, 1)), 0),
            ('G', false) | ('`', false) => self.goto(row, arg(0, 1) - 1),
            ('d', false) => self.goto(arg(0, 1) - 1, col),
            ('H', false) | ('f', false) => self.goto(arg(0, 1) - 1, arg(1, 1) - 1),
            ('J', false) => self.erase_display(raw(0)),
            ('K', false) => self.erase_line(raw(0)),
            ('X', false) => {
                let n = arg(0, 1);
                self.erase_cells(row, col, col + n);
            }
            ('@', false) => self.insert_chars(arg(0, 1)),
            ('P', false) => self.delete_chars(arg(0, 1)),
            ('L', false) => self.insert_lines(arg(0, 1)),
            ('M', false) => self.delete_lines(arg(0, 1)),
            ('S', false) => self.scroll_up(arg(0, 1)),
            ('T', false) => self.scroll_down(arg(0, 1)),
            ('m', false) => self.sgr(&flat_params(params)),
            ('r', false) => {
                let top = arg(0, 1) - 1;
                let bottom = arg(1, self.rows as u16).min(self.rows) - 1;
                if top < bottom {
                    self.top = top;
                    self.bottom = bottom;
                    self.goto(0, 0);
                }
            }
            ('h', true) | ('l', true) => {
                for &mode in &ps {
                    self.set_private_mode(mode, action == 'h');
                }
            }
            ('s', false) => self.save_cursor(),
            ('u', false) => self.restore_cursor(),
            ('n', false) => match raw(0) {
                5 => self.responses.extend_from_slice(b"\x1b[0n"),
                6 => {
                    let reply = format!("\x1b[{};{}R", row + 1, col + 1);
                    self.responses.extend_from_slice(reply.as_bytes());
                }
                _ => {}
            },
            ('c', false) if intermediates.is_empty() => {
                self.responses.extend_from_slice(b"\x1b[?1;0c");
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if ignore || !intermediates.is_empty() {
            return;
        }
        match byte {
            b'7' => self.save_cursor(),
            b'8' => self.restore_cursor(),
            b'D' => self.linefeed(),
            b'E' => {
                self.cursor_col = 0;
                self.linefeed();
            }
            b'M' => self.reverse_index(),
            b'c' => {
                let (cols, rows) = (self.cols, self.rows);
                *self = Grid::new(cols, rows);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(grid: &mut Grid, s: &str) {
        let mut parser = vte::Parser::new();
        parser.advance(grid, s.as_bytes());
    }

    #[test]
    fn prints_and_wraps() {
        let mut g = Grid::new(5, 3);
        feed(&mut g, "hello world");
        assert_eq!(g.row_text(0), "hello");
        assert_eq!(g.row_text(1), " worl");
        assert_eq!(g.row_text(2), "d");
    }

    #[test]
    fn crlf_and_scroll_into_history() {
        let mut g = Grid::new(10, 2);
        feed(&mut g, "a\r\nb\r\nc");
        assert_eq!(g.row_text(0), "b");
        assert_eq!(g.row_text(1), "c");
        assert_eq!(g.scrollback_len(), 1);
        assert_eq!(g.view_row(0, 1)[0].ch, 'a');
    }

    #[test]
    fn cursor_position_and_erase() {
        let mut g = Grid::new(10, 3);
        feed(&mut g, "xxxxxxxxxx\x1b[1;4H\x1b[K");
        assert_eq!(g.row_text(0), "xxx");
        feed(&mut g, "\x1b[2;2HZ");
        assert_eq!(g.row_text(1), " Z");
        assert_eq!(g.cursor(), (1, 2));
        feed(&mut g, "\x1b[2J");
        assert_eq!(g.row_text(0), "");
        assert_eq!(g.row_text(1), "");
    }

    #[test]
    fn sgr_colors() {
        let mut g = Grid::new(10, 1);
        feed(
            &mut g,
            "\x1b[1;31mA\x1b[38;5;200mB\x1b[38;2;1;2;3;44mC\x1b[0mD",
        );
        let row = g.view_row(0, 0);
        assert_eq!(row[0].attr.fg, TermColor::Indexed(1));
        assert!(row[0].attr.bold);
        assert_eq!(row[1].attr.fg, TermColor::Indexed(200));
        assert_eq!(row[2].attr.fg, TermColor::Rgb(1, 2, 3));
        assert_eq!(row[2].attr.bg, TermColor::Indexed(4));
        assert_eq!(row[3].attr, Attr::default());
    }

    #[test]
    fn scroll_region_keeps_outside_rows() {
        let mut g = Grid::new(5, 4);
        feed(&mut g, "top\r\n1\r\n2\r\nbot");
        feed(&mut g, "\x1b[2;3r\x1b[3;1H\nX");
        assert_eq!(g.row_text(0), "top");
        assert_eq!(g.row_text(1), "2");
        assert_eq!(g.row_text(2), "X");
        assert_eq!(g.row_text(3), "bot");
        assert_eq!(g.scrollback_len(), 0);
    }

    #[test]
    fn alternate_screen_restores_main() {
        let mut g = Grid::new(5, 2);
        feed(&mut g, "main");
        feed(&mut g, "\x1b[?1049hALT");
        assert_eq!(g.row_text(0), "ALT");
        feed(&mut g, "\x1b[?1049l");
        assert_eq!(g.row_text(0), "main");
        assert_eq!(g.cursor(), (0, 4));
    }

    #[test]
    fn insert_delete_chars_and_lines() {
        let mut g = Grid::new(6, 3);
        feed(&mut g, "abcdef\x1b[1;2H\x1b[2@");
        assert_eq!(g.row_text(0), "a  bcd");
        feed(&mut g, "\x1b[3P");
        assert_eq!(g.row_text(0), "acd");
        feed(&mut g, "\x1b[2;1Hline2\x1b[1;1H\x1b[L");
        assert_eq!(g.row_text(0), "");
        assert_eq!(g.row_text(1), "acd");
        assert_eq!(g.row_text(2), "line2");
    }

    #[test]
    fn device_status_report_replies() {
        let mut g = Grid::new(10, 5);
        feed(&mut g, "\x1b[3;4H\x1b[6n");
        assert_eq!(g.take_responses(), b"\x1b[3;4R");
    }

    #[test]
    fn resize_keeps_cursor_row_visible() {
        let mut g = Grid::new(10, 5);
        feed(&mut g, "1\r\n2\r\n3\r\n4\r\n5");
        g.resize(8, 2);
        assert_eq!(g.rows(), 2);
        assert_eq!(g.row_text(1), "5");
        assert_eq!(g.cursor().0, 1);
        assert_eq!(g.scrollback_len(), 3);
    }

    #[test]
    fn private_modes() {
        let mut g = Grid::new(10, 2);
        feed(&mut g, "\x1b[?25l\x1b[?1h\x1b[?2004h");
        assert!(!g.cursor_visible());
        assert!(g.app_cursor_keys());
        assert!(g.bracketed_paste());
    }
}
