//! The word completion list under the caret. It offers the words of the open
//! files, the language's keywords, and what those files `#include` (read by
//! running the compiler's preprocessor, so `printf` and `cout` are there).
//!
//! shortcut: matches text, not meaning, so it knows nothing of scopes or which
//! members follow `.`/`->`; talk to a language server (clangd) if that is
//! wanted.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::geom::{PointF, RectF};
use crate::syntax::{self, is_word, Kind};
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

/// The words of C or C++ source, leaving out comments and strings.
pub fn code_words(text: impl Iterator<Item = impl AsRef<str>>) -> BTreeSet<String> {
    let mut words = BTreeSet::new();
    let mut in_comment = false;
    for line in text {
        let line = line.as_ref();
        let (spans, open) = syntax::scan_line(line, in_comment);
        in_comment = open;
        let code = spans.iter().filter(|s| {
            !matches!(
                s.kind,
                Kind::Comment | Kind::Str | Kind::Preproc | Kind::Number
            )
        });
        words.extend(words_in(code.map(|s| &line[s.start..s.end])));
    }
    words
}

pub fn function_words(text: impl Iterator<Item = impl AsRef<str>>) -> BTreeSet<String> {
    let mut words = BTreeSet::new();
    let mut in_comment = false;
    for line in text {
        let line = line.as_ref();
        let (spans, open) = syntax::scan_line(line, in_comment);
        in_comment = open;
        words.extend(
            spans
                .iter()
                .filter(|span| span.kind == Kind::Function)
                .map(|span| line[span.start..span.end].to_owned()),
        );
    }
    words
}

/// Identifiers used after `.`, `->`, or `::` in the source. This is lexical
/// context only; it provides member names without claiming to resolve types.
pub fn member_words(text: impl Iterator<Item = impl AsRef<str>>) -> BTreeSet<String> {
    let mut words = BTreeSet::new();
    let mut in_comment = false;
    for line in text {
        let line = line.as_ref();
        let (spans, open) = syntax::scan_line(line, in_comment);
        in_comment = open;
        let mut visible = line.as_bytes().to_vec();
        for span in spans.iter().filter(|span| {
            matches!(
                span.kind,
                Kind::Comment | Kind::Str | Kind::Preproc | Kind::Number
            )
        }) {
            visible[span.start..span.end].fill(b' ');
        }
        let visible = String::from_utf8(visible).unwrap_or_default();
        let bytes = visible.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if !is_word(bytes[i]) || bytes[i].is_ascii_digit() {
                i += 1;
                continue;
            }
            let start = i;
            i += 1;
            while i < bytes.len() && is_word(bytes[i]) {
                i += 1;
            }
            let before = visible[..start].trim_end();
            if (before.ends_with('.') || before.ends_with("->") || before.ends_with("::"))
                && i - start >= 2
            {
                words.insert(visible[start..i].to_owned());
            }
        }
    }
    words
}

/// The public names in preprocessor output. Leading underscores are the
/// standard library's private names, thousands of them.
fn public_words(output: &str) -> BTreeSet<String> {
    let mut words = code_words(output.lines());
    words.retain(|w| !w.starts_with('_'));
    words
}

/// Everything `path` includes, by asking the compiler what the file expands
/// to. Empty if there is no compiler; a missing include still gives the rest.
#[cfg(test)]
pub fn included_words(path: &Path) -> BTreeSet<String> {
    included_index(path).0
}

pub fn included_index(path: &Path) -> (BTreeSet<String>, BTreeSet<String>) {
    let c = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("c"));
    let Some(tool) = debugger::find_tool(if c { "gcc" } else { "g++" }) else {
        return (BTreeSet::new(), BTreeSet::new());
    };
    let mut cmd = Command::new(tool);
    // -dD keeps macros; the language is spelled out as `.h` would mean C.
    cmd.args(["-E", "-dD", "-w", "-x", if c { "c" } else { "c++" }])
        .arg(path)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    crate::build::no_console_window(&mut cmd);
    cmd.output().map_or_else(
        |_| (BTreeSet::new(), BTreeSet::new()),
        |out| {
            let text = String::from_utf8_lossy(&out.stdout);
            (
                public_words(&text),
                function_words(text.lines())
                    .into_iter()
                    .filter(|w| !w.starts_with('_'))
                    .collect(),
            )
        },
    )
}

fn has_prefix(word: &str, prefix: &str) -> bool {
    word.len() >= prefix.len()
        && word.is_char_boundary(prefix.len())
        && word[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// What to offer after `#`.
pub fn directives(prefix: &str) -> Vec<String> {
    syntax::DIRECTIVES
        .iter()
        .copied()
        .filter(|w| has_prefix(w, prefix) && *w != prefix)
        .map(str::to_string)
        .collect()
}

/// Recursively indexes project headers for `#include` completion. Paths are
/// relative to the nearest repository root, with directory names ending in `/`.
pub fn include_paths(path: &Path) -> BTreeSet<String> {
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    let root = base
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .unwrap_or(base);
    let c = path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("c"));
    let mut roots = vec![(root.to_path_buf(), root.to_path_buf())];
    if let Some(tool) = debugger::find_tool(if c { "gcc" } else { "g++" }) {
        let mut cmd = Command::new(tool);
        cmd.args(["-E", "-x", if c { "c" } else { "c++" }, "-", "-v"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        crate::build::no_console_window(&mut cmd);
        if let Ok(output) = cmd.output() {
            let mut in_search = false;
            for line in String::from_utf8_lossy(&output.stderr).lines() {
                let line = line.trim();
                if line == "#include <...> search starts here:" {
                    in_search = true;
                } else if line == "End of search list." {
                    break;
                } else if in_search {
                    let dir = line.strip_suffix(" (framework directory)").unwrap_or(line);
                    let dir = PathBuf::from(dir);
                    if dir.is_dir() {
                        roots.push((dir.clone(), dir));
                    }
                }
            }
        }
    }
    let mut pending: Vec<_> = roots
        .into_iter()
        .map(|(dir, display_root)| (dir, display_root, 0usize))
        .collect();
    pending.reverse();
    let mut dirs = BTreeSet::new();
    let mut files = BTreeSet::new();
    let mut seen = 0usize;
    while let Some((dir, display_root, depth)) = pending.pop() {
        if depth > 12 || seen >= 50_000 {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || matches!(name.as_ref(), "target" | "build" | "out") {
                continue;
            }
            let full = entry.path();
            if full.is_dir() {
                pending.push((full, display_root.clone(), depth + 1));
            } else if full.extension().is_some_and(|ext| {
                ["h", "hh", "hpp", "hxx", "inc", "ipp", "tpp"]
                    .iter()
                    .any(|candidate| ext.eq_ignore_ascii_case(candidate))
            }) {
                let relative = full
                    .strip_prefix(&display_root)
                    .or_else(|_| full.strip_prefix(root));
                let Ok(relative) = relative else {
                    continue;
                };
                let relative = relative.to_string_lossy().replace('\\', "/");
                files.insert(relative.clone());
                let mut parent = PathBuf::from(&relative);
                parent.pop();
                let mut chain = Vec::new();
                while parent.components().count() > 0 {
                    chain.push(format!("{}/", parent.to_string_lossy().replace('\\', "/")));
                    if !parent.pop() {
                        break;
                    }
                }
                dirs.extend(chain);
            }
            if seen >= 50_000 {
                break;
            }
        }
    }
    dirs.extend(files);
    dirs
}

pub fn include_candidates(prefix: &str, paths: &BTreeSet<String>) -> Vec<String> {
    paths
        .iter()
        .filter(|path| path.starts_with(prefix) && path != &prefix)
        .take(MAX_ITEMS)
        .cloned()
        .collect()
}

fn glyph(word: &str, function: bool, include_path: bool) -> (&'static str, u8, &'static str) {
    if word.ends_with('/') {
        return (">", 0, "folder");
    }
    if include_path {
        return ("H", 1, "header");
    }
    let lower = word.to_ascii_lowercase();
    if ["h", "hh", "hpp", "hxx", "inc", "ipp", "tpp"]
        .iter()
        .any(|ext| lower.ends_with(&format!(".{ext}")))
    {
        return ("H", 1, "header");
    }
    if syntax::KEYWORDS.contains(&word) || syntax::DIRECTIVES.contains(&word) {
        ("K", 2, "keyword")
    } else if syntax::TYPES.contains(&word) || word.chars().next().is_some_and(char::is_uppercase) {
        ("T", 3, "type")
    } else if word.chars().all(|c| !c.is_ascii_lowercase()) {
        ("#", 4, "macro")
    } else if function {
        ("ƒ", 5, "function")
    } else {
        ("v", 5, "variable")
    }
}

/// What to offer for `prefix`: exact-case matches first; then the active file,
/// other open files, keywords, and included headers; alphabetically within
/// each source. The prefix itself is not offered.
pub fn candidates<'a>(
    prefix: &str,
    keywords: bool,
    own: impl Iterator<Item = &'a BTreeSet<String>>,
    included: impl Iterator<Item = &'a BTreeSet<String>>,
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut found = BTreeSet::new();
    let mut consider = |rank: u8, w: &str| {
        if has_prefix(w, prefix) && w != prefix && seen.insert(w.to_string()) {
            found.insert((
                !w.starts_with(prefix),
                rank,
                w.to_ascii_lowercase(),
                w.to_string(),
            ));
        }
    };
    for (rank, set) in own.enumerate() {
        set.iter().for_each(|w| consider(rank.min(1) as u8, w));
    }
    if keywords {
        syntax::KEYWORDS
            .iter()
            .chain(syntax::TYPES)
            .chain(syntax::LITERALS)
            .for_each(|w| consider(2, w));
    }
    for set in included {
        set.iter().for_each(|w| consider(3, w));
    }
    found
        .into_iter()
        .take(MAX_ITEMS)
        .map(|(_, _, _, w)| w)
        .collect()
}

/// `word` is a whole word already, so Enter after it should start a new line
/// rather than turn `do` into `double`.
pub fn is_complete(
    word: &str,
    mut sets: impl Iterator<Item = impl std::borrow::Borrow<BTreeSet<String>>>,
) -> bool {
    syntax::KEYWORDS.contains(&word)
        || syntax::TYPES.contains(&word)
        || syntax::LITERALS.contains(&word)
        || syntax::DIRECTIVES.contains(&word)
        || sets.any(|s| s.borrow().contains(word))
}

pub struct Completion {
    items: Vec<String>,
    selected: usize,
    chosen: bool,
    pub include_path: bool,
    functions: BTreeSet<String>,
    /// Where the prefix being completed starts on the caret's line.
    pub start_col: usize,
    /// What is typed is a word already: Enter starts a new line (Tab still
    /// takes the selected word).
    pub enter_makes_newline: bool,
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
        let w = (longest as f32 * cell.w + 2.0 * PAD + 88.0).min(body.w);
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
            chosen: false,
            include_path: false,
            functions: BTreeSet::new(),
            start_col,
            enter_makes_newline: false,
            rect: RectF::new(x, y, w, h),
            row_h,
        })
    }

    #[cfg(test)]
    pub fn items(&self) -> &[String] {
        &self.items
    }

    pub fn selected_word(&self) -> &str {
        &self.items[self.selected]
    }

    pub fn has_choice(&self) -> bool {
        self.chosen
    }

    pub fn set_functions(&mut self, functions: BTreeSet<String>) {
        self.functions = functions;
    }

    /// Moves the selection, wrapping at the ends.
    pub fn move_by(&mut self, delta: isize) {
        let n = self.items.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
        self.chosen = true;
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
        self.chosen = true;
    }

    pub fn paint(&self, d: &mut DrawCtx, theme: &Theme) {
        d.fill_round_rect(self.rect, 4.0, theme.panel_bg);
        let first = self.first();
        d.clip(self.rect, |d| {
            for (i, word) in self.items.iter().enumerate().skip(first).take(MAX_ROWS) {
                let y = self.rect.y + (i - first) as f32 * self.row_h;
                let row = RectF::new(self.rect.x, y, self.rect.w, self.row_h);
                let selected = self.chosen && i == self.selected;
                if selected {
                    d.fill_rect(row, theme.selection_bg);
                }
                let ink = if selected {
                    theme.text_bright
                } else {
                    theme.text
                };
                let (mark, kind, detail) =
                    glyph(word, self.functions.contains(word), self.include_path);
                let color = match kind {
                    0 => theme.dirty,
                    1 => theme.syntax_preproc,
                    2 => theme.syntax_keyword,
                    3 => theme.syntax_type,
                    4 => theme.syntax_number,
                    _ => theme.syntax_function,
                };
                let icon = RectF::new(row.x + 5.0, y + 3.0, self.row_h - 6.0, self.row_h - 6.0);
                d.fill_round_rect(icon, 2.0, color.with_alpha(0.22));
                d.text(TextStyle::Mono, mark, icon, color);
                let text = RectF::new(row.x + PAD + 16.0, y, (row.w - 104.0).max(0.0), row.h);
                d.text(TextStyle::Mono, word, text, ink);
                let detail_rect = RectF::new(row.right() - 78.0, y, 72.0, row.h);
                d.text(TextStyle::Mono, detail, detail_rect, theme.text_dim);
            }
        });
        d.stroke_rect(self.rect, theme.border, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none<'a>() -> std::iter::Empty<&'a BTreeSet<String>> {
        std::iter::empty()
    }

    #[test]
    fn your_words_rank_above_keywords_above_headers() {
        let own = words_in(["counter"].iter());
        let included = words_in(["codecvt counter"].iter());
        assert_eq!(
            candidates("co", true, [&own].into_iter(), [&included].into_iter())[..2],
            ["counter", "co_await"]
        );
        let all = candidates("co", true, [&own].into_iter(), [&included].into_iter());
        assert_eq!(all.last().unwrap(), "codecvt");
        assert_eq!(all.iter().filter(|w| *w == "counter").count(), 1, "once");
    }

    #[test]
    fn code_words_skip_comments_strings_and_directive_names() {
        let lines = [
            "#define LIMIT 10 // noted",
            "int total = \"quoted\"; /* opened",
            "still hidden */ int shown;",
        ];
        let got = code_words(lines.iter());
        let got: Vec<&str> = got.iter().map(String::as_str).collect();
        assert_eq!(got, ["LIMIT", "int", "shown", "total"]);
    }

    #[test]
    fn preprocessor_output_keeps_public_names() {
        let out = "# 1 \"x.h\" 3\n#define EOF (-1)\nint printf(const char *__fmt);\n";
        let got = public_words(out);
        let got: Vec<&str> = got.iter().map(String::as_str).collect();
        assert_eq!(got, ["EOF", "char", "const", "int", "printf"]);
    }

    /// Runs the real compiler over a file with a standard and a local header.
    #[test]
    fn includes_are_read_through_the_compiler() {
        if debugger::find_tool("g++").is_none() {
            eprintln!("skipped: no g++");
            return;
        }
        let dir = std::env::temp_dir().join(format!("lattice-incl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("my.h"), "#define MY_LIMIT 3\nint my_helper();\n").unwrap();
        let main = dir.join("main.cpp");
        std::fs::write(
            &main,
            "#include <cstdio>\n#include <vector>\n#include \"my.h\"\n",
        )
        .unwrap();
        let words = included_words(&main);
        for name in [
            "printf",
            "vector",
            "push_back",
            "my_helper",
            "MY_LIMIT",
            "EOF",
        ] {
            assert!(words.contains(name), "{name} missing of {}", words.len());
        }
        assert!(!words.iter().any(|w| w.starts_with('_')));
        std::fs::remove_dir_all(&dir).unwrap();
    }

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
            candidates("cou", false, [&set].into_iter(), none()),
            ["count", "counter", "country", "Count"]
        );
        // The word being typed is not offered back.
        assert_eq!(
            candidates("count", false, [&set].into_iter(), none()),
            ["counter", "country", "Count"]
        );
        assert_eq!(
            candidates("whi", true, [&set].into_iter(), none()),
            ["while"],
            "keywords"
        );
        assert!(candidates("whi", false, [&set].into_iter(), none()).is_empty());
    }

    #[test]
    fn non_ascii_words_do_not_split_mid_character() {
        let set = BTreeSet::from(["éa".to_string(), "ab".to_string()]);
        assert_eq!(candidates("a", false, [&set].into_iter(), none()), ["ab"]);
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
