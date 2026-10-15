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
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Field {
    /// Field name, element index, or map key.
    pub name: String,
    #[serde(rename = "type", default)]
    pub ty: String,
    pub value: Cell,
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
                     "fields":[{"name":"key","type":"int","value":{"k":"s","v":"50"}},
                               {"name":"left","type":"Node *","value":{"k":"p","to":null,"v":"nullptr"}},
                               {"name":"right","type":"Node *","value":{"k":"p","to":null,"v":"0x1","bad":true}}]},
                   "5f:std::vector<int>":{"addr":"0x5f","type":"std::vector<int>","kind":"array",
                     "fields":[{"name":"0","type":"","value":{"k":"s","v":"4"}}],"more":-1,
                     "summary":"std::vector of length 9"},
                   "dead:X":null},
        "truncated":false}"#;

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
    }

    #[test]
    fn rejects_garbage() {
        assert!(Snapshot::parse("{").is_err());
        assert!(Snapshot::parse(r#"{"frames":[{"level":0}],"objects":{}}"#).is_err());
    }
}
