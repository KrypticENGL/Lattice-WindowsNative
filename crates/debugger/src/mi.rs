//! GDB/MI3 output records.
//!
//! Grammar (GDB manual, "GDB/MI Output Syntax"), one record per line:
//!
//! ```text
//! [token] ^class[,results]     result record
//! [token] *class[,results]     exec async (running, stopped)
//! [token] +class[,results]     status async
//! [token] =class[,results]     notify async
//! ~"text"  @"text"  &"text"    console, target and log streams
//! (gdb)                        prompt
//! ```
//!
//! Values are c-strings, `{tuples}` of `name=value`, or `[lists]` of values
//! or of `name=value` results.

/// A value in a result list.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Tuple(Vec<(String, Value)>),
    /// List items; `name=value` items keep their name.
    List(Vec<(Option<String>, Value)>),
}

impl Value {
    /// The field `key` of a tuple.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Tuple(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsyncKind {
    Exec,
    Status,
    Notify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    Console,
    Target,
    Log,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Record {
    Result {
        token: Option<u64>,
        class: String,
        results: Vec<(String, Value)>,
    },
    Async {
        token: Option<u64>,
        kind: AsyncKind,
        class: String,
        results: Vec<(String, Value)>,
    },
    Stream {
        kind: StreamKind,
        text: String,
    },
    Prompt,
    /// Anything else: program output that reached GDB's stdout, or a line
    /// this parser does not understand.
    Other(String),
}

/// The value of `key` in a result list.
pub fn field<'a>(results: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    results.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// The string value of `key` in a result list.
pub fn field_str<'a>(results: &'a [(String, Value)], key: &str) -> Option<&'a str> {
    field(results, key).and_then(Value::as_str)
}

/// Parses one output line (without its line terminator).
pub fn parse_line(line: &str) -> Record {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.trim_end() == "(gdb)" {
        return Record::Prompt;
    }
    parse_record(line).unwrap_or_else(|| Record::Other(line.to_string()))
}

fn parse_record(line: &str) -> Option<Record> {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    let token = if digits > 0 {
        line[..digits].parse().ok()
    } else {
        None
    };
    let rest = &line[digits..];
    let mut p = Parser { s: rest, i: 1 };
    let kind = match rest.as_bytes().first()? {
        b'~' | b'@' | b'&' if digits == 0 => {
            let stream = match rest.as_bytes()[0] {
                b'~' => StreamKind::Console,
                b'@' => StreamKind::Target,
                _ => StreamKind::Log,
            };
            let text = p.cstring()?;
            return p.at_end().then_some(Record::Stream { kind: stream, text });
        }
        b'^' => None,
        b'*' => Some(AsyncKind::Exec),
        b'+' => Some(AsyncKind::Status),
        b'=' => Some(AsyncKind::Notify),
        _ => return None,
    };
    let class = p.word();
    if class.is_empty() {
        return None;
    }
    let mut results = Vec::new();
    while p.eat(b',') {
        results.push(p.result()?);
    }
    if !p.at_end() {
        return None;
    }
    Some(match kind {
        None => Record::Result {
            token,
            class,
            results,
        },
        Some(kind) => Record::Async {
            token,
            kind,
            class,
            results,
        },
    })
}

struct Parser<'a> {
    s: &'a str,
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.as_bytes().get(self.i).copied()
    }

    fn eat(&mut self, b: u8) -> bool {
        if self.peek() == Some(b) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn at_end(&self) -> bool {
        self.i >= self.s.len()
    }

    /// A class or variable name: anything up to `=`, `,` or a bracket.
    fn word(&mut self) -> String {
        let start = self.i;
        while let Some(b) = self.peek() {
            if matches!(b, b'=' | b',' | b'{' | b'}' | b'[' | b']' | b'"') {
                break;
            }
            self.i += 1;
        }
        self.s[start..self.i].to_string()
    }

    fn result(&mut self) -> Option<(String, Value)> {
        let name = self.word();
        if !self.eat(b'=') {
            return None;
        }
        Some((name, self.value()?))
    }

    fn value(&mut self) -> Option<Value> {
        match self.peek()? {
            b'"' => self.cstring().map(Value::Str),
            b'{' => {
                self.i += 1;
                let mut items = Vec::new();
                if self.eat(b'}') {
                    return Some(Value::Tuple(items));
                }
                loop {
                    items.push(self.result()?);
                    if self.eat(b'}') {
                        return Some(Value::Tuple(items));
                    }
                    if !self.eat(b',') {
                        return None;
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut items = Vec::new();
                if self.eat(b']') {
                    return Some(Value::List(items));
                }
                loop {
                    if matches!(self.peek()?, b'"' | b'{' | b'[') {
                        items.push((None, self.value()?));
                    } else {
                        let (k, v) = self.result()?;
                        items.push((Some(k), v));
                    }
                    if self.eat(b']') {
                        return Some(Value::List(items));
                    }
                    if !self.eat(b',') {
                        return None;
                    }
                }
            }
            _ => None,
        }
    }

    /// A C string with GDB's escapes. Octal escapes are bytes, so the result
    /// is assembled as bytes and decoded as UTF-8 (lossily) at the end.
    fn cstring(&mut self) -> Option<String> {
        if !self.eat(b'"') {
            return None;
        }
        let bytes = self.s.as_bytes();
        let mut out = Vec::new();
        while let Some(&b) = bytes.get(self.i) {
            self.i += 1;
            match b {
                b'"' => return Some(String::from_utf8_lossy(&out).into_owned()),
                b'\\' => {
                    let e = *bytes.get(self.i)?;
                    self.i += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => out.push(b'\r'),
                        b'a' => out.push(7),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'v' => out.push(11),
                        b'e' => out.push(27),
                        b'0'..=b'7' => {
                            let mut n = u32::from(e - b'0');
                            for _ in 0..2 {
                                match bytes.get(self.i) {
                                    Some(&d @ b'0'..=b'7') => {
                                        n = n * 8 + u32::from(d - b'0');
                                        self.i += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push(n as u8);
                        }
                        other => out.push(other),
                    }
                }
                _ => out.push(b),
            }
        }
        None
    }
}

/// Quotes `s` as an MI c-string argument.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_and_streams() {
        assert_eq!(parse_line("(gdb) "), Record::Prompt);
        assert_eq!(
            parse_line(r#"~"Reading symbols from ./t.exe...\n""#),
            Record::Stream {
                kind: StreamKind::Console,
                text: "Reading symbols from ./t.exe...\n".into()
            }
        );
        assert_eq!(
            parse_line(r#"&"warning: \"x\"\t\101\n""#),
            Record::Stream {
                kind: StreamKind::Log,
                text: "warning: \"x\"\tA\n".into()
            }
        );
    }

    #[test]
    fn stopped_record() {
        let line = r#"*stopped,reason="breakpoint-hit",disp="keep",bkptno="1",frame={addr="0x00007ff7f817162f",func="main",args=[],file="t.cpp",fullname="C:\\Users\\x\\t.cpp",line="7",arch="i386:x86-64"},thread-id="1",stopped-threads="all""#;
        let Record::Async {
            kind,
            class,
            results,
            token,
        } = parse_line(line)
        else {
            panic!("not async");
        };
        assert_eq!(
            (kind, class.as_str(), token),
            (AsyncKind::Exec, "stopped", None)
        );
        assert_eq!(field_str(&results, "reason"), Some("breakpoint-hit"));
        let frame = field(&results, "frame").unwrap();
        assert_eq!(
            frame.get("fullname").unwrap().as_str(),
            Some(r"C:\Users\x\t.cpp")
        );
        assert_eq!(frame.get("line").unwrap().as_str(), Some("7"));
        assert_eq!(frame.get("args"), Some(&Value::List(vec![])));
    }

    #[test]
    fn result_with_token_and_lists() {
        let line = r#"12^done,variables=[{name="root",type="Node *",value="0xfef20"},{name="keys",type="int [7]"}],ids=[a="1",b="2"]"#;
        let Record::Result {
            token,
            class,
            results,
        } = parse_line(line)
        else {
            panic!("not a result");
        };
        assert_eq!((token, class.as_str()), (Some(12), "done"));
        let Some(Value::List(vars)) = field(&results, "variables") else {
            panic!("no list");
        };
        assert_eq!(vars.len(), 2);
        assert_eq!(vars[1].1.get("type").unwrap().as_str(), Some("int [7]"));
        let Some(Value::List(ids)) = field(&results, "ids") else {
            panic!("no list");
        };
        assert_eq!(ids[1].0.as_deref(), Some("b"));
    }

    #[test]
    fn utf8_through_octal_escapes() {
        // "é" as two octal-escaped bytes.
        let Record::Stream { text, .. } = parse_line(r#"~"\303\251""#) else {
            panic!();
        };
        assert_eq!(text, "é");
    }

    #[test]
    fn junk_is_other() {
        assert_eq!(parse_line("hello"), Record::Other("hello".into()));
        assert_eq!(
            parse_line(r#"^done,x="unterminated"#),
            Record::Other(r#"^done,x="unterminated"#.into())
        );
        assert_eq!(parse_line("^done,x="), Record::Other("^done,x=".into()));
    }

    #[test]
    fn quoting_round_trips() {
        let s = r#"C:\a "b"\c.cpp"#;
        let line = format!("^done,p={}", quote(s));
        let Record::Result { results, .. } = parse_line(&line) else {
            panic!();
        };
        assert_eq!(field_str(&results, "p"), Some(s));
    }
}
