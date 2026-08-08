use anyhow::{Context as _, Result};
use collections::HashMap;
use serde::{Deserialize, Serialize};

use crate::{NodeId, NodeKey, SemanticGraph};

/// User-pinned canvas positions, keyed by stable [`NodeKey`].
///
/// Distinct from repo `semantic_map.toml` subsystem membership pins
/// ([`crate::PinConfig`]). These override auto-layout coordinates only.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CanvasPins {
    pub positions: HashMap<NodeKey, (f32, f32)>,
}

impl CanvasPins {
    pub fn pin(&mut self, key: NodeKey, position: (f32, f32)) {
        self.positions.insert(key, position);
    }

    pub fn unpin(&mut self, key: &NodeKey) {
        self.positions.remove(key);
    }

    pub fn get(&self, key: &NodeKey) -> Option<(f32, f32)> {
        self.positions.get(key).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// Overlay pinned coordinates onto an auto-layout map (matched by [`NodeId`]).
    ///
    /// Pins for nodes absent from `positions` are ignored so removed nodes do not
    /// reappear as orphans.
    pub fn apply_overrides(&self, positions: &mut HashMap<NodeId, (f32, f32)>) {
        for (key, position) in &self.positions {
            let id = NodeId::from_key(key);
            if positions.contains_key(&id) {
                positions.insert(id, *position);
            }
        }
    }

    /// Serialize for workspace KVP storage (`semantic-map-pins:{workspace_id}`).
    ///
    /// Entries are stored by [`NodeId`] (stable hash of [`NodeKey`]) because
    /// `NodeKey` is not fully deserializable across RelPath / SharedString.
    pub fn to_json(&self) -> Result<String> {
        let stored = StoredCanvasPins {
            positions: self
                .positions
                .iter()
                .map(|(key, &(x, y))| StoredPin {
                    node_id: NodeId::from_key(key).0,
                    x,
                    y,
                })
                .collect(),
        };
        serde_json::to_string(&stored).context("serialize canvas pins")
    }

    /// Restore pins by resolving stored [`NodeId`]s against the current graph.
    pub fn from_json(json: &str, graph: &SemanticGraph) -> Result<Self> {
        let stored: StoredCanvasPins =
            serde_json::from_str(json).context("deserialize canvas pins")?;
        let by_id: HashMap<NodeId, (f32, f32)> = stored
            .positions
            .into_iter()
            .map(|pin| (NodeId(pin.node_id), (pin.x, pin.y)))
            .collect();

        let mut positions = HashMap::default();
        for node in graph.nodes.values() {
            if let Some(position) = by_id.get(&node.id) {
                positions.insert(node.key.clone(), *position);
            }
        }
        Ok(Self { positions })
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredCanvasPins {
    positions: Vec<StoredPin>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredPin {
    node_id: u64,
    x: f32,
    y: f32,
}

/// Workspace KVP key for canvas layout pins.
pub fn canvas_pins_kvp_key(workspace_id: impl std::fmt::Display) -> String {
    format!("semantic-map-pins:{workspace_id}")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use pretty_assertions::assert_eq;
    use util::rel_path::RelPath;
    use worktree::WorktreeId;

    use super::*;
    use crate::layout::hierarchy::{self, CELL_HEIGHT, CELL_WIDTH};
    use crate::{
        Edge, GraphPatch, GraphRevision, Lens, ModuleKind, ModulePayload, ModuleRef, Node,
        NodeFlags, SubsystemPayload,
    };

    fn module_node(worktree_id: WorktreeId, name: &str) -> Node {
        let key = NodeKey::Module {
            worktree_id,
            module_ref: ModuleRef::CargoPackage {
                package_name: name.into(),
                manifest_dir: Arc::from(
                    RelPath::from_unix_str(&format!("crates/{name}")).expect("valid path"),
                ),
            },
        };
        let id = NodeId::from_key(&key);
        Node::module(
            id,
            key,
            name,
            None,
            ModulePayload {
                language: Some("rust".into()),
                module_kind: ModuleKind::CrateLib,
                public_exports: Vec::new(),
                deps_out_count: 0,
                deps_in_count: 0,
                loc_estimate: None,
            },
            NodeFlags::default(),
        )
    }

    fn fixture_graph() -> (SemanticGraph, Node, Node) {
        let worktree_id = WorktreeId::from_usize(1);
        let project_key = NodeKey::Project { worktree_id };
        let project_id = NodeId::from_key(&project_key);

        let alpha_key = NodeKey::Subsystem {
            project: project_key.clone().into(),
            slug: "alpha".into(),
        };
        let alpha_id = NodeId::from_key(&alpha_key);
        let alpha = Node::subsystem(
            alpha_id,
            alpha_key,
            "alpha",
            SubsystemPayload {
                member_count: 1,
                cluster_score: 1.0,
                pinned: false,
            },
            NodeFlags::default(),
        );

        let apple = module_node(worktree_id, "apple");
        let apple_id = apple.id;

        let mut graph = SemanticGraph::default();
        graph
            .apply_patch(GraphPatch {
                base: GraphRevision(0),
                removed_nodes: Vec::new(),
                removed_edges: Vec::new(),
                upsert_nodes: vec![
                    Node::project(project_id, project_key, "demo"),
                    alpha.clone(),
                    apple.clone(),
                ],
                upsert_edges: vec![
                    Edge::contains(project_id, alpha_id),
                    Edge::contains(alpha_id, apple_id),
                ],
            })
            .expect("fixture patch applies");

        (graph, alpha, apple)
    }

    #[test]
    fn canvas_pin_overrides_hierarchy_layout() {
        let (graph, alpha, apple) = fixture_graph();
        let auto = hierarchy::layout(&graph, &Lens::default());
        assert_eq!(auto.get(&alpha.id), Some(&(0.0, 0.0)));
        assert_eq!(auto.get(&apple.id), Some(&(0.0, CELL_HEIGHT)));

        let mut pins = CanvasPins::default();
        pins.pin(apple.key.clone(), (42.0, 99.0));
        pins.pin(alpha.key.clone(), (CELL_WIDTH, CELL_HEIGHT));

        let positioned = hierarchy::layout_with_pins(&graph, &Lens::default(), Some(&pins));
        assert_eq!(positioned.get(&apple.id), Some(&(42.0, 99.0)));
        assert_eq!(positioned.get(&alpha.id), Some(&(CELL_WIDTH, CELL_HEIGHT)));
    }

    #[test]
    fn canvas_pins_json_roundtrip_resolves_keys_from_graph() {
        let (graph, _alpha, apple) = fixture_graph();
        let mut pins = CanvasPins::default();
        pins.pin(apple.key.clone(), (12.5, -3.0));

        let json = pins.to_json().expect("serialize");
        let restored = CanvasPins::from_json(&json, &graph).expect("deserialize");
        assert_eq!(restored.get(&apple.key), Some((12.5, -3.0)));
        assert_eq!(restored.positions.len(), 1);
    }

    #[test]
    fn canvas_pins_kvp_key_matches_workspace_convention() {
        assert_eq!(canvas_pins_kvp_key(7), "semantic-map-pins:7");
    }
}
