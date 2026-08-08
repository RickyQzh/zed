use collections::HashMap;

use crate::{Lens, Node, NodeId, NodeKind, SemanticGraph};

pub const CELL_WIDTH: f32 = 240.0;
pub const CELL_HEIGHT: f32 = 120.0;

/// Deterministic hierarchical layout for subsystems and modules.
///
/// Subsystems sit on row 0 (ordered by `display_name`); modules belonging to a
/// subsystem stack beneath that subsystem's column on subsequent rows. Modules
/// contained directly by the project (orphans) occupy columns after all
/// subsystems, also ordered by `display_name`.
pub fn layout(graph: &SemanticGraph, lens: &Lens) -> HashMap<NodeId, (f32, f32)> {
    let mut positions = HashMap::default();
    let Some(root_id) = resolve_root(graph, lens) else {
        return positions;
    };

    if !depth_allowed(1, lens) {
        return positions;
    }

    let subsystems = filtered_children(graph, root_id, NodeKind::Subsystem, lens);
    for (column, &subsystem_id) in subsystems.iter().enumerate() {
        let x = column as f32 * CELL_WIDTH;
        positions.insert(subsystem_id, (x, 0.0));

        if !depth_allowed(2, lens) {
            continue;
        }

        let modules = filtered_children(graph, subsystem_id, NodeKind::Module, lens);
        for (row_offset, &module_id) in modules.iter().enumerate() {
            positions.insert(module_id, (x, (1 + row_offset) as f32 * CELL_HEIGHT));
        }
    }

    let orphan_modules = filtered_children(graph, root_id, NodeKind::Module, lens);
    let start_column = subsystems.len();
    for (index, &module_id) in orphan_modules.iter().enumerate() {
        positions.insert(
            module_id,
            ((start_column + index) as f32 * CELL_WIDTH, 0.0),
        );
    }

    positions
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

fn depth_allowed(depth: u32, lens: &Lens) -> bool {
    lens.max_depth.map_or(true, |max| depth <= max)
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
    true
}

fn filtered_children(
    graph: &SemanticGraph,
    parent: NodeId,
    kind: NodeKind,
    lens: &Lens,
) -> Vec<NodeId> {
    let mut children = graph.children.get(&parent).cloned().unwrap_or_default();
    children.retain(|child_id| {
        graph
            .nodes
            .get(child_id)
            .is_some_and(|node| node.kind == kind && node_passes_lens(node, lens))
    });
    children.sort_by(|a, b| {
        let name_a = graph
            .nodes
            .get(a)
            .map(|node| node.display_name.as_ref())
            .unwrap_or("");
        let name_b = graph
            .nodes
            .get(b)
            .map(|node| node.display_name.as_ref())
            .unwrap_or("");
        name_a.cmp(name_b).then_with(|| a.cmp(b))
    });
    children
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use pretty_assertions::assert_eq;
    use util::rel_path::RelPath;
    use worktree::WorktreeId;

    use super::*;
    use crate::{
        Edge, GraphPatch, GraphRevision, ModuleKind, ModulePayload, ModuleRef, Node, NodeFlags,
        NodeKey, SubsystemPayload,
    };

    fn module_node(
        worktree_id: WorktreeId,
        name: &str,
        deps_out: u32,
        deps_in: u32,
    ) -> Node {
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
                deps_out_count: deps_out,
                deps_in_count: deps_in,
                loc_estimate: None,
            },
            NodeFlags::default(),
        )
    }

    fn fixture_graph() -> (SemanticGraph, NodeId, NodeId, NodeId, NodeId, NodeId) {
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
                member_count: 2,
                cluster_score: 1.0,
                pinned: false,
            },
            NodeFlags::default(),
        );

        let beta_key = NodeKey::Subsystem {
            project: project_key.clone().into(),
            slug: "beta".into(),
        };
        let beta_id = NodeId::from_key(&beta_key);
        let beta = Node::subsystem(
            beta_id,
            beta_key,
            "beta",
            SubsystemPayload {
                member_count: 0,
                cluster_score: 1.0,
                pinned: false,
            },
            NodeFlags::default(),
        );

        let apple = module_node(worktree_id, "apple", 1, 0);
        let apple_id = apple.id;
        let zebra = module_node(worktree_id, "zebra", 0, 1);
        let zebra_id = zebra.id;
        let orphan = module_node(worktree_id, "orphan", 0, 0);
        let orphan_id = orphan.id;

        let mut graph = SemanticGraph::default();
        graph
            .apply_patch(GraphPatch {
                base: GraphRevision(0),
                removed_nodes: Vec::new(),
                removed_edges: Vec::new(),
                upsert_nodes: vec![
                    Node::project(project_id, project_key, "demo"),
                    alpha,
                    beta,
                    apple,
                    zebra,
                    orphan,
                ],
                upsert_edges: vec![
                    Edge::contains(project_id, alpha_id),
                    Edge::contains(project_id, beta_id),
                    Edge::contains(project_id, orphan_id),
                    Edge::contains(alpha_id, apple_id),
                    Edge::contains(alpha_id, zebra_id),
                    Edge::depends_on(apple_id, zebra_id),
                ],
            })
            .expect("fixture patch applies");

        (graph, project_id, alpha_id, beta_id, apple_id, zebra_id)
    }

    #[test]
    fn layout_positions_are_deterministic_by_display_name() {
        let (graph, _, alpha_id, beta_id, apple_id, zebra_id) = fixture_graph();
        let orphan_id = graph
            .nodes
            .values()
            .find(|node| node.display_name.as_ref() == "orphan")
            .map(|node| node.id)
            .expect("orphan module");

        let positions = layout(&graph, &Lens::default());

        // Subsystems on row 0, alphabetical: alpha then beta.
        assert_eq!(positions.get(&alpha_id), Some(&(0.0, 0.0)));
        assert_eq!(positions.get(&beta_id), Some(&(CELL_WIDTH, 0.0)));

        // Modules under alpha, alphabetical: apple then zebra.
        assert_eq!(positions.get(&apple_id), Some(&(0.0, CELL_HEIGHT)));
        assert_eq!(positions.get(&zebra_id), Some(&(0.0, 2.0 * CELL_HEIGHT)));

        // Orphan module after subsystem columns.
        assert_eq!(positions.get(&orphan_id), Some(&(2.0 * CELL_WIDTH, 0.0)));

        let again = layout(&graph, &Lens::default());
        assert_eq!(positions, again);
    }
}
