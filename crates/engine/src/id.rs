//! Stable node ids. FNV-1a is used instead of `DefaultHasher` because its
//! output must not change between Rust releases.

use crate::graph::{NodeId, NodeKind};

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a(hash: u64, bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(hash, |h, &b| (h ^ u64::from(b)).wrapping_mul(FNV_PRIME))
}

fn kind_tag(kind: NodeKind) -> u8 {
    match kind {
        NodeKind::Function => 1,
        NodeKind::Variable => 2,
        NodeKind::Type => 3,
        NodeKind::Section => 4,
        NodeKind::External => 5,
    }
}

pub(crate) fn node_id(kind: NodeKind, linkage: &str, object: &str) -> NodeId {
    let mut h = fnv1a(FNV_OFFSET, &[kind_tag(kind), 0xff]);
    h = fnv1a(h, linkage.as_bytes());
    h = fnv1a(h, &[0xff]);
    NodeId(fnv1a(h, object.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vector_is_stable() {
        // Guards against accidental changes to the hash, which would break
        // id stability across versions.
        assert_eq!(fnv1a(FNV_OFFSET, b""), FNV_OFFSET);
        assert_eq!(fnv1a(FNV_OFFSET, b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn kind_path_and_name_all_matter() {
        let a = node_id(NodeKind::Function, "f", "x.o");
        assert_eq!(a, node_id(NodeKind::Function, "f", "x.o"));
        assert_ne!(a, node_id(NodeKind::Variable, "f", "x.o"));
        assert_ne!(a, node_id(NodeKind::Function, "g", "x.o"));
        assert_ne!(a, node_id(NodeKind::Function, "f", "y.o"));
        // The separator keeps ("ab", "c") distinct from ("a", "bc").
        assert_ne!(
            node_id(NodeKind::Function, "ab", "c"),
            node_id(NodeKind::Function, "a", "bc")
        );
    }
}
