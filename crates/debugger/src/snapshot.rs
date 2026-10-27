//! The data `-lattice-snapshot` returns (see `snapshot.py`).

use std::collections::BTreeMap;

use serde::Deserialize;

/// The stopped program's frames and the objects reachable from them.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Snapshot {
    /// Outermost first (`main` at index 0).
    pub frames: Vec<Frame>,
    /// Keyed by `address:type` (or `vN:type` for values without an
    /// address), so a heap block keeps its id from one stop to the next.
    #[serde(deserialize_with = "objects")]
    pub objects: BTreeMap<String, Object>,
    /// The object cap was hit; some pointers were not followed.
    #[serde(default)]
    pub truncated: bool,
    /// File-level variables of the source files on the stack. Their
    /// targets are not walked; struct globals list their fields as text.
    #[serde(default)]
    pub globals: Vec<Var>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Frame {
    pub level: u32,
    pub func: String,
    pub file: String,
    pub line: u32,
    pub vars: Vec<Var>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Var {
    pub name: String,
    #[serde(default)]
    pub arg: bool,
    #[serde(rename = "type", default)]
    pub ty: String,
    pub value: Cell,
    /// Where the variable itself lives (for a pointer, the pointer).
    #[serde(default)]
    pub addr: Option<String>,
    #[serde(default)]
    pub size: u64,
    /// Its first bytes (see [`Object::bytes`]).
    #[serde(default, deserialize_with = "hex")]
    pub bytes: Vec<u8>,
    /// Globals only: a by-value struct's fields.
    #[serde(default)]
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObjectKind {
    /// Named fields (structs, classes, unions, smart-pointer-like wrappers).
    Struct,
    /// Indexed elements (C arrays, `std::vector`, `std::list`, …).
    Array,
    /// Key/value entries (`std::map`, `std::unordered_map`, …).
    Map,
    /// A single value behind a pointer to a scalar.
    Cell,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Object {
    pub addr: Option<String>,
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub size: u64,
    pub kind: ObjectKind,
    pub fields: Vec<Field>,
    /// Elements not included: a count, or -1 when unknown (a container
    /// whose length GDB did not report).
    #[serde(default)]
    pub more: i64,
    /// A pretty-printer's one-line description, e.g.
    /// `std::vector of length 4, capacity 4`.
    #[serde(default)]
    pub summary: Option<String>,
    /// The first `min(size, 256)` bytes; empty when unreadable or without
    /// an address.
    #[serde(default, deserialize_with = "hex")]
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Field {
    /// Field name, element index, or map key.
    pub name: String,
    #[serde(rename = "type", default)]
    pub ty: String,
    pub value: Cell,
    /// Byte offset in the object: struct fields and C array elements
    /// (not pretty-printed container entries, which live elsewhere).
    #[serde(default)]
    pub off: Option<u64>,
    #[serde(default)]
    pub size: Option<u64>,
}

/// One value slot.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "k")]
pub enum Cell {
    /// Text: numbers, strings, enums, function pointers.
    #[serde(rename = "s")]
    Scalar { v: String },
    /// A pointer or reference. `to` is the target object, `None` for null,
    /// unreadable (`bad`) or beyond the object cap (`cut`).
    #[serde(rename = "p")]
    Pointer {
        to: Option<String>,
        v: String,
        #[serde(default)]
        bad: bool,
        #[serde(default)]
        cut: bool,
    },
    /// A by-value aggregate (nested struct, array, container) drawn as its
    /// own object attached to this slot.
    #[serde(rename = "o")]
    Embedded { to: String },
}

impl Cell {
    /// The object this slot leads to, if any.
    pub fn target(&self) -> Option<&str> {
        match self {
            Cell::Pointer { to, .. } => to.as_deref(),
            Cell::Embedded { to } => Some(to),
            Cell::Scalar { .. } => None,
        }
    }
}

/// Objects the script could not finish come through as `null`; drop them.
fn objects<'de, D>(d: D) -> Result<BTreeMap<String, Object>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = BTreeMap::<String, Option<Object>>::deserialize(d)?;
    Ok(raw.into_iter().filter_map(|(k, v)| Some((k, v?))).collect())
}

/// Hex text (or `null`) as bytes; malformed text gives no bytes.
fn hex<'de, D>(d: D) -> Result<Vec<u8>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = Option::<String>::deserialize(d)?.unwrap_or_default();
    let digits = s.as_bytes();
    if digits.len() % 2 != 0 {
        return Ok(Vec::new());
    }
    Ok(digits
        .chunks(2)
        .map(|p| {
            std::str::from_utf8(p)
                .ok()
                .and_then(|p| u8::from_str_radix(p, 16).ok())
        })
        .collect::<Option<Vec<u8>>>()
        .unwrap_or_default())
}

impl Snapshot {
    pub fn parse(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| format!("bad snapshot: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"frames":[{"level":0,"func":"main","file":"C:\\x\\t.cpp","line":7,
        "vars":[{"name":"root","arg":false,"type":"Node *","value":{"k":"p","to":"a0:Node","v":"0xa0"}},
                {"name":"v","type":"std::vector<int>","value":{"k":"o","to":"5f:std::vector<int>"}},
                {"name":"n","value":{"k":"s","v":"3"}}]}],
        "objects":{"a0:Node":{"addr":"0xa0","type":"Node","size":24,"kind":"struct",
                     "bytes":"32000000ff",
                     "fields":[{"name":"key","type":"int","off":0,"size":4,"value":{"k":"s","v":"50"}},
                               {"name":"left","type":"Node *","value":{"k":"p","to":null,"v":"nullptr"}},
                               {"name":"right","type":"Node *","value":{"k":"p","to":null,"v":"0x1","bad":true}}]},
                   "5f:std::vector<int>":{"addr":"0x5f","type":"std::vector<int>","kind":"array",
                     "fields":[{"name":"0","type":"","value":{"k":"s","v":"4"}}],"more":-1,
                     "summary":"std::vector of length 9"},
                   "dead:X":null},
        "truncated":false,
        "globals":[{"name":"g","type":"int","value":{"k":"s","v":"7"},"addr":"0x40","size":4,"bytes":"07000000"}]}"#;

    #[test]
    fn parses_the_script_output() {
        let s = Snapshot::parse(SAMPLE).unwrap();
        assert_eq!(s.frames.len(), 1);
        assert_eq!(s.frames[0].vars[0].value.target(), Some("a0:Node"));
        assert_eq!(
            s.frames[0].vars[1].value.target(),
            Some("5f:std::vector<int>")
        );
        assert!(!s.frames[0].vars[2].arg);
        assert_eq!(s.objects.len(), 2, "null objects are dropped");
        let node = &s.objects["a0:Node"];
        assert_eq!(
            (node.kind, node.size, node.fields.len()),
            (ObjectKind::Struct, 24, 3)
        );
        assert!(matches!(
            node.fields[2].value,
            Cell::Pointer { bad: true, .. }
        ));
        let v = &s.objects["5f:std::vector<int>"];
        assert_eq!((v.kind, v.more), (ObjectKind::Array, -1));
        // Memory: bytes, offsets, globals.
        assert_eq!(node.bytes, [0x32, 0, 0, 0, 0xff]);
        assert_eq!(
            (node.fields[0].off, node.fields[0].size),
            (Some(0), Some(4))
        );
        assert_eq!(node.fields[1].off, None);
        assert!(v.bytes.is_empty());
        assert_eq!(s.globals[0].bytes, [7, 0, 0, 0]);
        assert_eq!(s.globals[0].addr.as_deref(), Some("0x40"));
    }

    #[test]
    fn rejects_garbage() {
        assert!(Snapshot::parse("{").is_err());
        assert!(Snapshot::parse(r#"{"frames":[{"level":0}],"objects":{}}"#).is_err());
        let odd = r#"{"frames":[],"objects":{"1:T":{"addr":"0x1","type":"T","kind":"cell","fields":[],"bytes":"abc"}}}"#;
        assert!(Snapshot::parse(odd).unwrap().objects["1:T"]
            .bytes
            .is_empty());
    }
}
