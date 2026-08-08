use std::hash::{Hash, Hasher};

use collections::FxHasher;
use serde::Serialize;

/// Stable identifier derived from a [`crate::NodeKey`] hash.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct NodeId(pub u64);

/// Stable identifier for an edge.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct EdgeId(pub u64);

impl NodeId {
    /// Hash a node key into a stable [`NodeId`].
    ///
    /// Uses [`FxHasher`]: deterministic within a process and across identical
    /// Rustc/`rustc-hash` versions, but not a cryptographic or cross-language
    /// stable digest. Prefer this over `DefaultHasher` (which is intentionally
    /// randomized per process in some std versions).
    pub fn from_key(key: &impl Hash) -> Self {
        let mut hasher = FxHasher::default();
        key.hash(&mut hasher);
        Self(hasher.finish())
    }
}

impl EdgeId {
    /// Derive an edge id from its structural identity `(kind discriminant, from, to)`.
    pub fn from_endpoints(kind_disc: u8, from: NodeId, to: NodeId) -> Self {
        let mut hasher = FxHasher::default();
        kind_disc.hash(&mut hasher);
        from.hash(&mut hasher);
        to.hash(&mut hasher);
        Self(hasher.finish())
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use util::rel_path::RelPath;
    use worktree::WorktreeId;

    use super::*;
    use crate::{ModuleRef, NodeKey};

    #[test]
    fn node_key_hash_is_stable_for_cargo_package() {
        let key = NodeKey::Module {
            worktree_id: WorktreeId::from_usize(1),
            module_ref: ModuleRef::CargoPackage {
                package_name: "editor".into(),
                manifest_dir: RelPath::from_unix_str("crates/editor").unwrap().into(),
            },
        };
        let a = NodeId::from_key(&key);
        let b = NodeId::from_key(&key);
        assert_eq!(a, b);
    }
}
