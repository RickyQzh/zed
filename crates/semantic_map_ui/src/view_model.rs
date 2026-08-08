use gpui::{point, size, Bounds, Point, SharedString};
use semantic_graph::{
    EdgeId, EdgeKind, IntentIndex, Lens, Node, NodeId, NodeKind, SemanticGraph,
    SemanticGraphSnapshot,
};

const CELL_WIDTH: f32 = 240.0;
const CELL_HEIGHT: f32 = 120.0;
const NODE_WIDTH: f32 = 200.0;
const NODE_HEIGHT: f32 = 80.0;
const GRID_COLUMNS: usize = 4;

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
        let Some(root_id) = resolve_root(&snapshot.graph, lens) else {
            return Self { rows };
        };
        collect_panel_rows(
            root_id,
            0,
            &snapshot.graph,
            snapshot.intents.as_ref(),
            lens,
            &mut rows,
        );
        Self { rows }
    }
}

impl CanvasViewModel {
    pub fn from_snapshot(snapshot: &SemanticGraphSnapshot, lens: &Lens) -> Self {
        let mut canvas_nodes = Vec::new();
        let Some(root_id) = resolve_root(&snapshot.graph, lens) else {
            return Self::default();
        };
        collect_canvas_nodes(
            root_id,
            0,
            &snapshot.graph,
            snapshot.intents.as_ref(),
            lens,
            &mut canvas_nodes,
        );

        let mut nodes = Vec::with_capacity(canvas_nodes.len());
        for (index, (node_id, node, subtitle)) in canvas_nodes.into_iter().enumerate() {
            let column = (index % GRID_COLUMNS) as f32;
            let row = (index / GRID_COLUMNS) as f32;
            nodes.push(SceneNode {
                id: node_id,
                kind: node.kind,
                rect: Bounds {
                    origin: point(column * CELL_WIDTH, row * CELL_HEIGHT),
                    size: size(NODE_WIDTH, NODE_HEIGHT),
                },
                title: node.display_name.clone(),
                subtitle,
            });
        }

        let visible: Vec<NodeId> = nodes.iter().map(|node| node.id).collect();
        let edge_kinds = &lens.edge_kinds;
        let mut edges = Vec::new();
        for edge in snapshot.graph.edges.values() {
            if !edge_kinds.contains(&edge.kind) {
                continue;
            }
            if !visible.contains(&edge.from) || !visible.contains(&edge.to) {
                continue;
            }
            edges.push(SceneEdge {
                id: edge.id,
                from: edge.from,
                to: edge.to,
                kind: edge.kind,
                routed_path: Vec::new(),
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

fn collect_canvas_nodes(
    node_id: NodeId,
    depth: u32,
    graph: &SemanticGraph,
    intents: &IntentIndex,
    lens: &Lens,
    out: &mut Vec<(NodeId, Node, Option<SharedString>)>,
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

    if matches!(node.kind, NodeKind::Module | NodeKind::Subsystem) {
        out.push((
            node_id,
            node.clone(),
            intent_summary(intents, node_id),
        ));
    }

    if let Some(max_depth) = lens.max_depth {
        if depth == max_depth {
            return;
        }
    }

    for child_id in sorted_children(graph, node_id) {
        collect_canvas_nodes(child_id, depth + 1, graph, intents, lens, out);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use pretty_assertions::assert_eq;
    use semantic_graph::{
        Edge, EcosystemKind, ExternalPayload, GraphPatch, GraphRevision, GraphStatus, Node,
        NodeFlags, NodeId, NodeKey, NodeKind, NodePayload, SemanticGraph, SubsystemPayload,
    };
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
}
