use gpui::{point, size, Bounds, Point, SharedString};
use semantic_graph::{
    CanvasPins, EdgeId, EdgeKind, IntentIndex, Lens, Node, NodeId, NodeKind, SemanticGraph,
    SemanticGraphSnapshot, hierarchy,
};

const NODE_WIDTH: f32 = 200.0;
const NODE_HEIGHT: f32 = 80.0;

#[derive(Debug, Clone, PartialEq)]
pub struct PanelRow {
    pub node_id: NodeId,
    pub depth: u32,
    pub name: SharedString,
    pub intent_summary: Option<SharedString>,
    pub kind: NodeKind,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PanelViewModel {
    pub rows: Vec<PanelRow>,
    pub status: PanelStatus,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum PanelStatus {
    #[default]
    Ready,
    Indexing,
    Partial {
        reason: SharedString,
    },
    Error {
        message: SharedString,
    },
}

impl PanelStatus {
    pub fn from_graph_status(status: &semantic_graph::GraphStatus) -> Self {
        use semantic_graph::GraphStatus;
        match status {
            GraphStatus::Idle => Self::Ready,
            GraphStatus::Indexing => Self::Indexing,
            GraphStatus::Partial { reason } => Self::Partial {
                reason: reason.clone(),
            },
            GraphStatus::Error { message } => Self::Error {
                message: message.clone(),
            },
        }
    }

    pub fn chip_label(&self) -> SharedString {
        match self {
            Self::Ready => "Ready".into(),
            Self::Indexing => "Indexing…".into(),
            Self::Partial { .. } => "Partial".into(),
            Self::Error { .. } => "Error".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SceneNode {
    pub id: NodeId,
    pub kind: NodeKind,
    pub rect: Bounds<f32>,
    pub title: SharedString,
    pub subtitle: Option<SharedString>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SceneEdge {
    pub id: EdgeId,
    pub from: NodeId,
    pub to: NodeId,
    pub kind: EdgeKind,
    pub routed_path: Vec<Point<f32>>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CanvasViewModel {
    pub nodes: Vec<SceneNode>,
    pub edges: Vec<SceneEdge>,
}

impl PanelViewModel {
    pub fn from_snapshot(snapshot: &SemanticGraphSnapshot, lens: &Lens) -> Self {
        let mut rows = Vec::new();
        let status = PanelStatus::from_graph_status(&snapshot.status);
        let Some(root_id) = resolve_root(&snapshot.graph, lens) else {
            return Self { rows, status };
        };
        collect_panel_rows(
            root_id,
            0,
            &snapshot.graph,
            snapshot.intents.as_ref(),
            lens,
            &mut rows,
        );
        Self { rows, status }
    }
}

impl CanvasViewModel {
    pub fn from_snapshot(snapshot: &SemanticGraphSnapshot, lens: &Lens) -> Self {
        Self::from_snapshot_with_pins(snapshot, lens, None)
    }

    pub fn from_snapshot_with_pins(
        snapshot: &SemanticGraphSnapshot,
        lens: &Lens,
        pins: Option<&CanvasPins>,
    ) -> Self {
        // Only include nodes that hierarchy layout positions. Nested modules that
        // DFS would visit but layout does not place are intentionally omitted.
        let positions = hierarchy::layout_with_pins(&snapshot.graph, lens, pins);
        let mut nodes = Vec::with_capacity(positions.len());
        for (&node_id, &(x, y)) in &positions {
            let Some(node) = snapshot.graph.nodes.get(&node_id) else {
                continue;
            };
            nodes.push(SceneNode {
                id: node_id,
                kind: node.kind,
                rect: Bounds {
                    origin: point(x, y),
                    size: size(NODE_WIDTH, NODE_HEIGHT),
                },
                title: node.display_name.clone(),
                subtitle: intent_summary(snapshot.intents.as_ref(), node_id),
            });
        }
        nodes.sort_by(|a, b| {
            a.rect
                .origin
                .y
                .total_cmp(&b.rect.origin.y)
                .then_with(|| a.rect.origin.x.total_cmp(&b.rect.origin.x))
                .then_with(|| a.id.cmp(&b.id))
        });

        let mut edges = Vec::new();
        for edge in snapshot.graph.edges.values() {
            if edge.kind != EdgeKind::DependsOn {
                continue;
            }
            if !lens.edge_kinds.contains(&edge.kind) {
                continue;
            }
            let Some(from_node) = nodes.iter().find(|node| node.id == edge.from) else {
                continue;
            };
            let Some(to_node) = nodes.iter().find(|node| node.id == edge.to) else {
                continue;
            };
            let from_center = point(
                from_node.rect.origin.x + from_node.rect.size.width / 2.0,
                from_node.rect.origin.y + from_node.rect.size.height / 2.0,
            );
            let to_center = point(
                to_node.rect.origin.x + to_node.rect.size.width / 2.0,
                to_node.rect.origin.y + to_node.rect.size.height / 2.0,
            );
            edges.push(SceneEdge {
                id: edge.id,
                from: edge.from,
                to: edge.to,
                kind: edge.kind,
                routed_path: vec![from_center, to_center],
            });
        }
        edges.sort_by_key(|edge| (edge.from, edge.to, edge.kind as u8));

        Self { nodes, edges }
    }
}

fn resolve_root(graph: &SemanticGraph, lens: &Lens) -> Option<NodeId> {
    if let Some(root) = lens.root {
        return graph.nodes.contains_key(&root).then_some(root);
    }
    graph
        .nodes
        .values()
        .find(|node| node.kind == NodeKind::Project)
        .map(|node| node.id)
}

fn node_passes_lens(node: &Node, lens: &Lens) -> bool {
    if lens.hide_external && (node.kind == NodeKind::External || node.flags.is_external) {
        return false;
    }
    if lens.hide_tests && node.flags.is_test {
        return false;
    }
    if !lens.allowed_kinds.contains(&node.kind) {
        return false;
    }
    // Ignore `lens.focus` for now: per-node checks prune ancestors and hide matching
    // descendants. Proper subtree / ancestor-preserving focus is Task 10+.
    true
}

fn intent_summary(intents: &IntentIndex, node_id: NodeId) -> Option<SharedString> {
    intents.get(&node_id).map(|intent| intent.summary.clone())
}

fn sorted_children(graph: &SemanticGraph, parent: NodeId) -> Vec<NodeId> {
    let mut children = graph.children.get(&parent).cloned().unwrap_or_default();
    children.sort_by(|a, b| {
        let name_a = graph.nodes.get(a).map(|n| n.display_name.as_ref()).unwrap_or("");
        let name_b = graph.nodes.get(b).map(|n| n.display_name.as_ref()).unwrap_or("");
        name_a.cmp(name_b).then_with(|| a.cmp(b))
    });
    children
}

fn collect_panel_rows(
    node_id: NodeId,
    depth: u32,
    graph: &SemanticGraph,
    intents: &IntentIndex,
    lens: &Lens,
    rows: &mut Vec<PanelRow>,
) {
    if let Some(max_depth) = lens.max_depth {
        if depth > max_depth {
            return;
        }
    }
    let Some(node) = graph.nodes.get(&node_id) else {
        return;
    };
    if !node_passes_lens(node, lens) {
        return;
    }

    rows.push(PanelRow {
        node_id,
        depth,
        name: node.display_name.clone(),
        intent_summary: intent_summary(intents, node_id),
        kind: node.kind,
    });

    if let Some(max_depth) = lens.max_depth {
        if depth == max_depth {
            return;
        }
    }

    for child_id in sorted_children(graph, node_id) {
        collect_panel_rows(child_id, depth + 1, graph, intents, lens, rows);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use pretty_assertions::assert_eq;
    use semantic_graph::{
        Edge, EcosystemKind, ExternalPayload, GraphPatch, GraphRevision, GraphStatus, ModuleKind,
        ModulePayload, ModuleRef, Node, NodeFlags, NodeId, NodeKey, NodeKind, NodePayload,
        SemanticGraph, SubsystemPayload,
    };
    use util::rel_path::RelPath;
    use worktree::WorktreeId;

    use super::*;

    fn project_and_external_snapshot() -> (SemanticGraphSnapshot, NodeId, NodeId, NodeId) {
        let worktree_id = WorktreeId::from_usize(1);
        let project_key = NodeKey::Project { worktree_id };
        let project_id = NodeId::from_key(&project_key);
        let project = Node::project(project_id, project_key.clone(), "demo");

        let subsystem_key = NodeKey::Subsystem {
            project: project_key.clone().into(),
            slug: "core".into(),
        };
        let subsystem_id = NodeId::from_key(&subsystem_key);
        let subsystem = Node::subsystem(
            subsystem_id,
            subsystem_key,
            "core",
            SubsystemPayload {
                member_count: 0,
                cluster_score: 1.0,
                pinned: false,
            },
            NodeFlags::default(),
        );

        let external_key = NodeKey::External {
            ecosystem: EcosystemKind::Cargo,
            name: "serde".into(),
            version_req: None,
        };
        let external_id = NodeId::from_key(&external_key);
        let external = Node {
            id: external_id,
            key: external_key,
            kind: NodeKind::External,
            display_name: "serde".into(),
            abbrev: None,
            location: None,
            payload: NodePayload::External(ExternalPayload {
                ecosystem: EcosystemKind::Cargo,
            }),
            flags: NodeFlags {
                is_external: true,
                ..NodeFlags::default()
            },
        };

        let mut graph = SemanticGraph::default();
        graph
            .apply_patch(GraphPatch {
                base: GraphRevision(0),
                removed_nodes: Vec::new(),
                removed_edges: Vec::new(),
                upsert_nodes: vec![project, subsystem, external],
                upsert_edges: vec![
                    Edge::contains(project_id, subsystem_id),
                    Edge::contains(project_id, external_id),
                    Edge::depends_on(subsystem_id, external_id),
                ],
            })
            .expect("patch applies");

        let snapshot = SemanticGraphSnapshot {
            revision: graph.revision(),
            graph: Arc::new(graph),
            intents: Arc::new(IntentIndex::default()),
            status: GraphStatus::Idle,
        };
        (snapshot, project_id, subsystem_id, external_id)
    }

    #[test]
    fn panel_view_hides_external_when_requested() {
        let (snapshot, project_id, subsystem_id, external_id) = project_and_external_snapshot();

        let mut lens = Lens::default();
        lens.hide_external = true;
        let hidden = PanelViewModel::from_snapshot(&snapshot, &lens);
        assert_eq!(
            hidden
                .rows
                .iter()
                .map(|row| (row.node_id, row.kind, row.depth))
                .collect::<Vec<_>>(),
            vec![
                (project_id, NodeKind::Project, 0),
                (subsystem_id, NodeKind::Subsystem, 1),
            ]
        );
        assert!(!hidden.rows.iter().any(|row| row.node_id == external_id));

        lens.hide_external = false;
        let shown = PanelViewModel::from_snapshot(&snapshot, &lens);
        assert!(
            shown
                .rows
                .iter()
                .any(|row| row.node_id == external_id && row.kind == NodeKind::External)
        );
    }

    #[test]
    fn canvas_view_grids_modules_and_subsystems() {
        let (snapshot, _, subsystem_id, external_id) = project_and_external_snapshot();
        let mut lens = Lens::default();
        lens.hide_external = true;

        let canvas = CanvasViewModel::from_snapshot(&snapshot, &lens);
        assert_eq!(canvas.nodes.len(), 1);
        assert_eq!(canvas.nodes[0].id, subsystem_id);
        assert_eq!(
            canvas.nodes[0].rect,
            Bounds {
                origin: point(0.0, 0.0),
                size: size(NODE_WIDTH, NODE_HEIGHT),
            }
        );
        assert!(!canvas.nodes.iter().any(|node| node.id == external_id));
        assert!(canvas.edges.is_empty());
    }

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

    fn simple_workspace_root() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../semantic_graph/test_data/simple_workspace")
    }

    #[test]
    fn panel_and_canvas_show_simple_workspace_modules() {
        let root = simple_workspace_root();
        let snapshot = semantic_graph::GraphIndexer::reindex_cargo_or_generic(
            &root,
            WorktreeId::from_usize(1),
        )
        .expect("fixture snapshot should build without a Project");

        let panel = PanelViewModel::from_snapshot(&snapshot, &Lens::default());
        let canvas = CanvasViewModel::from_snapshot(&snapshot, &Lens::default());

        for expected in ["app", "core_lib"] {
            assert!(
                panel.rows.iter().any(|row| {
                    row.name.as_ref() == expected && row.kind == NodeKind::Module
                }),
                "panel should list module {expected}, rows={:?}",
                panel
                    .rows
                    .iter()
                    .map(|row| (row.name.to_string(), row.kind))
                    .collect::<Vec<_>>()
            );
            assert!(
                canvas.nodes.iter().any(|node| {
                    node.title.as_ref() == expected && node.kind == NodeKind::Module
                }),
                "canvas should show a card for module {expected}, titles={:?}",
                canvas
                    .nodes
                    .iter()
                    .map(|node| (node.title.to_string(), node.kind))
                    .collect::<Vec<_>>()
            );
        }

        let core_lib_row = panel
            .rows
            .iter()
            .find(|row| row.name.as_ref() == "core_lib" && row.kind == NodeKind::Module)
            .expect("core_lib module panel row");
        let core_lib_intent = core_lib_row
            .intent_summary
            .as_ref()
            .map(|summary| summary.to_lowercase())
            .unwrap_or_default();
        assert!(
            core_lib_intent.contains("domain") || core_lib_intent.contains("logic"),
            "core_lib panel row should surface fixture domain/logic intent, got {core_lib_intent:?}"
        );

        assert!(
            panel.rows.iter().any(|row| {
                row.kind == NodeKind::Subsystem
                    && row
                        .intent_summary
                        .as_ref()
                        .is_some_and(|summary| {
                            summary.to_lowercase().contains("application binary")
                        })
            }),
            "panel should show the pin summary as a subsystem intent"
        );
    }

    #[test]
    fn canvas_omits_nested_modules_not_positioned_by_layout() {
        // hierarchy::layout only places project→subsystem→module (and project→module
        // orphans). A module nested under another module is reachable by DFS but has
        // no layout position — the canvas must omit it by design, not silently drop
        // after collecting it.
        let worktree_id = WorktreeId::from_usize(1);
        let project_key = NodeKey::Project { worktree_id };
        let project_id = NodeId::from_key(&project_key);

        let subsystem_key = NodeKey::Subsystem {
            project: project_key.clone().into(),
            slug: "core".into(),
        };
        let subsystem_id = NodeId::from_key(&subsystem_key);
        let subsystem = Node::subsystem(
            subsystem_id,
            subsystem_key,
            "core",
            SubsystemPayload {
                member_count: 1,
                cluster_score: 1.0,
                pinned: false,
            },
            NodeFlags::default(),
        );

        let parent_module = module_node(worktree_id, "parent");
        let parent_id = parent_module.id;
        let nested_module = module_node(worktree_id, "nested");
        let nested_id = nested_module.id;

        let mut graph = SemanticGraph::default();
        graph
            .apply_patch(GraphPatch {
                base: GraphRevision(0),
                removed_nodes: Vec::new(),
                removed_edges: Vec::new(),
                upsert_nodes: vec![
                    Node::project(project_id, project_key, "demo"),
                    subsystem,
                    parent_module,
                    nested_module,
                ],
                upsert_edges: vec![
                    Edge::contains(project_id, subsystem_id),
                    Edge::contains(subsystem_id, parent_id),
                    Edge::contains(parent_id, nested_id),
                    Edge::depends_on(parent_id, nested_id),
                ],
            })
            .expect("patch applies");

        let snapshot = SemanticGraphSnapshot {
            revision: graph.revision(),
            graph: Arc::new(graph),
            intents: Arc::new(IntentIndex::default()),
            status: GraphStatus::Idle,
        };

        let positions = hierarchy::layout(&snapshot.graph, &Lens::default());
        assert!(positions.contains_key(&subsystem_id));
        assert!(positions.contains_key(&parent_id));
        assert!(
            !positions.contains_key(&nested_id),
            "layout must not position nested modules"
        );

        let canvas = CanvasViewModel::from_snapshot(&snapshot, &Lens::default());
        let canvas_ids: Vec<_> = canvas.nodes.iter().map(|node| node.id).collect();
        assert_eq!(canvas_ids, vec![subsystem_id, parent_id]);
        assert!(
            !canvas.nodes.iter().any(|node| node.id == nested_id),
            "canvas must not include nested modules that layout does not place"
        );
        // DependsOn to an unpositioned nested target is omitted; Contains is never drawn.
        assert!(canvas.edges.is_empty());
        assert_eq!(canvas.nodes.len(), positions.len());
    }
}
