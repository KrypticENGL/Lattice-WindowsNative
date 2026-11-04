//! C and C++ syntax colouring: a line-at-a-time scanner, no parsing. The only
//! state carried between lines is "inside a block comment".
//!
//! shortcut: strings continued with `\` and multi-line raw strings end at the
//! line's end, so a raw string's later lines colour as code; carry more state
//! between lines if real sources show this often.

use std::path::Path;

use crate::theme::{Color, Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Plain,
    Keyword,
    Type,
    Function,
    Str,
    Number,
    Comment,
    Preproc,
}

/// A run of one kind, in byte offsets. A line's spans cover all of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub kind: Kind,
}

pub const KEYWORDS: &[&str] = &[
    "alignas",
    "alignof",
    "asm",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "consteval",
    "constexpr",
    "constinit",
    "const_cast",
    "continue",
    "co_await",
    "co_return",
    "co_yield",
    "decltype",
    "default",
    "delete",
    "do",
    "dynamic_cast",
    "else",
    "enum",
    "explicit",
    "export",
    "extern",
    "final",
    "for",
    "friend",
    "goto",
    "if",
    "inline",
    "mutable",
    "namespace",
    "new",
    "noexcept",
    "operator",
    "override",
    "private",
    "protected",
    "public",
    "register",
    "reinterpret_cast",
    "requires",
    "restrict",
    "return",
    "sizeof",
    "static",
    "static_assert",
    "static_cast",
    "struct",
    "switch",
    "template",
    "this",
    "thread_local",
    "throw",
    "try",
    "typedef",
    "typeid",
    "typename",
    "union",
    "using",
    "virtual",
    "volatile",
    "while",
];

pub const TYPES: &[&str] = &[
    "auto",
    "bool",
    "char",
    "char8_t",
    "char16_t",
    "char32_t",
    "double",
    "float",
    "int",
    "int8_t",
    "int16_t",
    "int32_t",
    "int64_t",
    "intptr_t",
    "long",
    "ptrdiff_t",
    "short",
    "signed",
    "size_t",
    "ssize_t",
    "uint8_t",
    "uint16_t",
    "uint32_t",
    "uint64_t",
    "uintptr_t",
    "unsigned",
    "void",
    "wchar_t",
];

const LITERALS: &[&str] = &["true", "false", "nullptr", "NULL"];

/// A file this scanner understands.
pub fn is_c_like(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        ["c", "h", "cc", "cpp", "cxx", "hh", "hpp", "hxx", "inl"]
            .iter()
            .any(|x| e.eq_ignore_ascii_case(x))
    })
}

/// Part of an identifier. Bytes of non-ASCII characters count, so spans
/// always end on character boundaries.
pub fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

pub fn color(theme: &Theme, kind: Kind) -> Color {
    match kind {
        Kind::Plain => theme.text,
        Kind::Keyword => theme.syntax_keyword,
        Kind::Type => theme.syntax_type,
        Kind::Function => theme.syntax_function,
        Kind::Str => theme.syntax_string,
        Kind::Number => theme.syntax_number,
        Kind::Comment => theme.syntax_comment,
        Kind::Preproc => theme.syntax_preproc,
    }
}

/// Spans of `line`, and whether a block comment is still open at its end.
pub fn scan_line(line: &str, in_comment: bool) -> (Vec<Span>, bool) {
    let b = line.as_bytes();
    let mut out = Vec::new();
    // Everything since `plain` that no span has claimed yet is plain text.
    let mut plain = 0;
    let mut emit = |start: usize, end: usize, kind: Kind| {
        if plain < start {
            out.push(Span {
                start: plain,
                end: start,
                kind: Kind::Plain,
            });
        }
        if start < end {
            out.push(Span { start, end, kind });
        }
        plain = end;
    };

    let mut i = 0;
    if in_comment {
        match line.find("*/") {
            Some(e) => {
                emit(0, e + 2, Kind::Comment);
                i = e + 2;
            }
            None => {
                emit(0, b.len(), Kind::Comment);
                return (out, true);
            }
        }
    }

    // A directive's name, and an `#include`'s <file>.
    let first = (i..b.len()).find(|&k| !b[k].is_ascii_whitespace());
    if let Some(start) = first.filter(|&k| b[k] == b'#') {
        let mut k = start + 1;
        while k < b.len() && b[k].is_ascii_whitespace() {
            k += 1;
        }
        let name = k;
        while k < b.len() && is_word(b[k]) {
            k += 1;
        }
        emit(start, k, Kind::Preproc);
        i = k;
        if &line[name..k] == "include" {
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if b.get(i) == Some(&b'<') {
                let end = line[i..].find('>').map_or(b.len(), |e| i + e + 1);
                emit(i, end, Kind::Str);
                i = end;
            }
        }
    }

    let mut after_type_word = false;
    let mut open = false;
    while i < b.len() {
        let c = b[i];
        let next = b.get(i + 1).copied();
        if c == b'/' && next == Some(b'/') {
            emit(i, b.len(), Kind::Comment);
            i = b.len();
        } else if c == b'/' && next == Some(b'*') {
            match line[i + 2..].find("*/") {
                Some(e) => {
                    let end = i + 2 + e + 2;
                    emit(i, end, Kind::Comment);
                    i = end;
                }
                None => {
                    emit(i, b.len(), Kind::Comment);
                    i = b.len();
                    open = true;
                }
            }
        } else if c == b'"' || c == b'\'' {
            let mut k = i + 1;
            while k < b.len() && b[k] != c {
                k += if b[k] == b'\\' { 2 } else { 1 };
            }
            let end = (k + 1).min(b.len());
            emit(i, end, Kind::Str);
            i = end;
        } else if c.is_ascii_digit() || (c == b'.' && next.is_some_and(|n| n.is_ascii_digit())) {
            let mut k = i + 1;
            while k < b.len() {
                let d = b[k];
                let exponent = matches!(b[k - 1], b'e' | b'E' | b'p' | b'P');
                if is_word(d) || d == b'.' || d == b'\'' || (exponent && matches!(d, b'+' | b'-')) {
                    k += 1;
                } else {
                    break;
                }
            }
            emit(i, k, Kind::Number);
            i = k;
        } else if is_word(c) {
            let mut k = i + 1;
            while k < b.len() && is_word(b[k]) {
                k += 1;
            }
            let word = &line[i..k];
            let called = b[k..].iter().find(|c| !c.is_ascii_whitespace()) == Some(&b'(');
            let kind = classify(word, called, after_type_word);
            after_type_word = matches!(word, "class" | "struct" | "enum" | "union" | "typename");
            if kind != Kind::Plain {
                emit(i, k, kind);
            }
            i = k;
        } else {
            i += 1;
        }
    }
    emit(b.len(), b.len(), Kind::Plain);
    (out, open)
}

fn classify(word: &str, called: bool, after_type_word: bool) -> Kind {
    if KEYWORDS.contains(&word) {
        Kind::Keyword
    } else if TYPES.contains(&word) {
        Kind::Type
    } else if LITERALS.contains(&word) {
        Kind::Number
    } else if called {
        Kind::Function
    } else if after_type_word || is_pascal_case(word) {
        Kind::Type
    } else {
        Kind::Plain
    }
}

/// `CodeGraph` looks like a type; `MAX_LINES` and `x` do not.
fn is_pascal_case(word: &str) -> bool {
    let mut chars = word.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase()) && chars.any(|c| c.is_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The non-plain tokens of a line, as text.
    fn tokens(line: &str, in_comment: bool) -> Vec<(&str, Kind)> {
        let (spans, _) = scan_line(line, in_comment);
        let mut at = 0;
        for s in &spans {
            assert_eq!(s.start, at, "spans cover the line in order: {line}");
            at = s.end;
        }
        assert_eq!(at, line.len(), "spans reach the end: {line}");
        spans
            .iter()
            .filter(|s| s.kind != Kind::Plain)
            .map(|s| (&line[s.start..s.end], s.kind))
            .collect()
    }

    #[test]
    fn classifies_code() {
        assert_eq!(
            tokens("static int foo(Bar x, double y = 1.5e-3) {", false),
            [
                ("static", Kind::Keyword),
                ("int", Kind::Type),
                ("foo", Kind::Function),
                ("Bar", Kind::Type),
                ("double", Kind::Type),
                ("1.5e-3", Kind::Number),
            ]
        );
        assert_eq!(
            tokens("struct node *p = nullptr;", false),
            [
                ("struct", Kind::Keyword),
                ("node", Kind::Type),
                ("nullptr", Kind::Number),
            ]
        );
        assert_eq!(tokens("x = 1'000;", false), [("1'000", Kind::Number)]);
    }

    #[test]
    fn strings_and_comments_hide_their_contents() {
        assert_eq!(
            tokens(r#"s = "a\"//b"; // int"#, false),
            [(r#""a\"//b""#, Kind::Str), ("// int", Kind::Comment)]
        );
        assert_eq!(tokens("c = '\\'';", false), [("'\\''", Kind::Str)]);
        assert_eq!(tokens(r#"s = "open"#, false), [(r#""open"#, Kind::Str)]);
        // A backslash at the very end must not run past the line.
        assert_eq!(tokens(r#"s = "a\"#, false), [(r#""a\"#, Kind::Str)]);
    }

    #[test]
    fn block_comments_carry_across_lines() {
        let (_, open) = scan_line("int a; /* start", false);
        assert!(open);
        let (spans, open) = scan_line("still int", true);
        assert!(open);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].kind, Kind::Comment);
        assert_eq!(
            tokens("end */ int b;", true),
            [("end */", Kind::Comment), ("int", Kind::Type)]
        );
        assert!(!scan_line("end */ int b;", true).1);
        assert!(!scan_line("/* one line */", false).1);
    }

    #[test]
    fn directives() {
        assert_eq!(
            tokens("  #  include <stdio.h> // io", false),
            [
                ("#  include", Kind::Preproc),
                ("<stdio.h>", Kind::Str),
                ("// io", Kind::Comment),
            ]
        );
        assert_eq!(
            tokens("#define N 10", false),
            [("#define", Kind::Preproc), ("10", Kind::Number)]
        );
    }

    #[test]
    fn non_ascii_stays_on_char_boundaries() {
        assert_eq!(
            tokens("é = \"ü\"; // ñ", false),
            [("\"ü\"", Kind::Str), ("// ñ", Kind::Comment)]
        );
        assert!(tokens("", false).is_empty());
    }

    #[test]
    fn file_types() {
        assert!(is_c_like(Path::new("a.CPP")) && is_c_like(Path::new("b.h")));
        assert!(!is_c_like(Path::new("a.rs")) && !is_c_like(Path::new("Makefile")));
    }
}
