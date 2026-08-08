use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use collections::HashSet;
use worktree::WorktreeId;

use crate::extract::{
    cluster_subsystems, extract_cargo_workspace, extract_generic, extract_rust_modules_with_base,
    load_pin_config_from_root, ClusterConfig,
};
use crate::intent::StaticIntentProvider;
use crate::{
    EdgeId, GraphPatch, GraphStatus, IntentIndex, ModuleRef, NodeId, NodeKind, SemanticGraph,
    SemanticGraphSnapshot,
};

const DEFAULT_MODULE_DEPTH: u32 = 2;

/// Sync orchestration: Cargo workspace when present, otherwise generic thin extract,
/// then rust module depth (Cargo), subsystem clustering, then static intents.
///
/// When the resulting graph exceeds `max_auto_nodes`, excess nodes are dropped and the
/// third return value is `true` (caller should surface [`GraphStatus::Partial`]).
pub fn build_initial_graph(
    root: &Path,
    worktree_id: WorktreeId,
    max_auto_nodes: usize,
) -> Result<(SemanticGraph, IntentIndex, bool)> {
    let mut graph = SemanticGraph::default();
    let mut truncated = false;
    if root.join("Cargo.toml").is_file() {
        let patch = extract_cargo_workspace(root, worktree_id)?;
        truncated |= apply_patch_with_budget(&mut graph, patch, max_auto_nodes)?;
        if !truncated {
            truncated |= apply_rust_module_depth(
                &mut graph,
                root,
                worktree_id,
                DEFAULT_MODULE_DEPTH,
                max_auto_nodes,
            )?;
        }
    } else {
        let patch = extract_generic(root, worktree_id, 2)?;
        truncated |= apply_patch_with_budget(&mut graph, patch, max_auto_nodes)?;
    }

    if graph.nodes.len() < max_auto_nodes {
        let pins = load_pin_config_from_root(root)?;
        let patch = cluster_subsystems(&graph, ClusterConfig::default(), &pins)?;
        truncated |= apply_patch_with_budget(&mut graph, patch, max_auto_nodes)?;
    }

    let mut intents = StaticIntentProvider::enrich(&graph, root)?;
    truncated |= enforce_max_auto_nodes(&mut graph, &mut intents, max_auto_nodes)?;
    Ok((graph, intents, truncated))
}

/// Apply `patch`, then drop overflow nodes so the graph stays within `max_auto_nodes`.
fn apply_patch_with_budget(
    graph: &mut SemanticGraph,
    patch: GraphPatch,
    max_auto_nodes: usize,
) -> Result<bool> {
    graph.apply_patch(patch)?;
    let mut empty_intents = IntentIndex::default();
    enforce_max_auto_nodes(graph, &mut empty_intents, max_auto_nodes)
}

/// Drop lowest-priority nodes until `graph.nodes.len() <= max_auto_nodes`.
///
/// Retention priority: Project → Subsystem → Module → Entry → Type → External.
/// Returns `true` when any nodes were removed.
pub fn enforce_max_auto_nodes(
    graph: &mut SemanticGraph,
    intents: &mut IntentIndex,
    max_auto_nodes: usize,
) -> Result<bool> {
    if graph.nodes.len() <= max_auto_nodes {
        return Ok(false);
    }

    let mut ranked: Vec<(u8, NodeId)> = graph
        .nodes
        .values()
        .map(|node| (node_kind_budget_rank(node.kind), node.id))
        .collect();
    ranked.sort_by_key(|&(rank, id)| (rank, id));

    let keep: HashSet<NodeId> = ranked
        .into_iter()
        .take(max_auto_nodes)
        .map(|(_, id)| id)
        .collect();

    let removed_nodes: Vec<NodeId> = graph
        .nodes
        .keys()
        .copied()
        .filter(|id| !keep.contains(id))
        .collect();
    let removed_edges: Vec<EdgeId> = graph
        .edges
        .values()
        .filter(|edge| !keep.contains(&edge.from) || !keep.contains(&edge.to))
        .map(|edge| edge.id)
        .collect();

    graph.apply_patch(GraphPatch {
        base: graph.revision(),
        removed_nodes,
        removed_edges,
        upsert_nodes: Vec::new(),
        upsert_edges: Vec::new(),
    })?;
    intents.retain(|node_id, _| keep.contains(node_id));
    Ok(true)
}

fn node_kind_budget_rank(kind: NodeKind) -> u8 {
    match kind {
        NodeKind::Project => 0,
        NodeKind::Subsystem => 1,
        NodeKind::Module => 2,
        NodeKind::Entry => 3,
        NodeKind::Type => 4,
        NodeKind::External => 5,
    }
}

fn apply_rust_module_depth(
    graph: &mut SemanticGraph,
    root: &Path,
    worktree_id: WorktreeId,
    depth: u32,
    max_auto_nodes: usize,
) -> Result<bool> {
    let packages: Vec<_> = graph
        .nodes
        .values()
        .filter(|node| node.kind == NodeKind::Module)
        .filter_map(|node| match &node.key {
            crate::NodeKey::Module {
                module_ref: ModuleRef::CargoPackage { manifest_dir, .. },
                ..
            } => Some((node.id, manifest_dir.as_unix_str().to_string())),
            _ => None,
        })
        .collect();

    let mut truncated = false;
    for (package_node_id, manifest_dir) in packages {
        if graph.nodes.len() >= max_auto_nodes {
            truncated = true;
            break;
        }
        let package_root = root.join(&manifest_dir);
        let patch = extract_rust_modules_with_base(
            root,
            &package_root,
            package_node_id,
            worktree_id,
            depth,
            graph.revision(),
        )?;
        if !patch.upsert_nodes.is_empty() || !patch.upsert_edges.is_empty() {
            truncated |= apply_patch_with_budget(graph, patch, max_auto_nodes)?;
        }
    }
    Ok(truncated)
}

/// Sync test helper wrapping [`build_initial_graph`] into a snapshot.
pub struct GraphIndexer;

impl GraphIndexer {
    pub fn reindex_cargo_or_generic(
        root: &Path,
        worktree_id: WorktreeId,
    ) -> Result<SemanticGraphSnapshot> {
        let (graph, intents, truncated) = build_initial_graph(root, worktree_id, usize::MAX)?;
        Ok(SemanticGraphSnapshot {
            revision: graph.revision(),
            graph: Arc::new(graph),
            intents: Arc::new(intents),
            status: if truncated {
                GraphStatus::Partial {
                    reason: "truncated".into(),
                }
            } else {
                GraphStatus::Idle
            },
        })
    }
}

/// Classify dirty paths into scopes for future incremental reindex jobs.
/// Full-graph rebuild currently goes through [`SemanticGraphStore::reindex`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidationScope {
    WorkspacePackages,
    PackageModuleTree,
    FileOutline,
    EnrichmentOnly,
    SubsystemMembership,
}

/// Classify a changed path into an invalidation scope.
pub fn invalidation_scope_for_path(path: &Path) -> InvalidationScope {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if file_name == "Cargo.toml" || file_name == "semantic_map.toml" {
        if file_name == "semantic_map.toml" {
            InvalidationScope::SubsystemMembership
        } else {
            InvalidationScope::WorkspacePackages
        }
    } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
        InvalidationScope::FileOutline
    } else {
        InvalidationScope::PackageModuleTree
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use worktree::WorktreeId;

    use super::{build_initial_graph, GraphIndexer};
    use crate::{EdgeKind, NodeKind};

    #[test]
    fn build_initial_graph_cargo_fixture_has_core_lib_intent() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/simple_workspace");
        let (graph, intents, truncated) =
            build_initial_graph(&root, WorktreeId::from_usize(1), usize::MAX).unwrap();
        assert!(!truncated);
        assert!(
            graph.nodes.values().any(|node| {
                node.kind == NodeKind::Module && node.display_name.as_ref() == "core_lib"
            })
        );
        assert!(!intents.is_empty());

        let ui_id = graph
            .nodes
            .values()
            .find(|node| node.kind == NodeKind::Subsystem && node.display_name.as_ref() == "ui")
            .map(|node| node.id)
            .expect("fixture pins crates/app under subsystem ui");
        let app_id = graph
            .nodes
            .values()
            .find(|node| node.kind == NodeKind::Module && node.display_name.as_ref() == "app")
            .map(|node| node.id)
            .expect("app module");
        assert!(
            graph.edges.values().any(|edge| {
                edge.kind == EdgeKind::Contains && edge.from == ui_id && edge.to == app_id
            }),
            "expected Contains(ui → app) from semantic_map.toml pins"
        );

        let snap =
            GraphIndexer::reindex_cargo_or_generic(&root, WorktreeId::from_usize(1)).unwrap();
        assert_eq!(snap.graph.nodes.len(), graph.nodes.len());
        assert_eq!(snap.intents.len(), intents.len());
    }

    #[test]
    fn build_initial_graph_truncates_when_over_max_auto_nodes() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/simple_workspace");
        let max_auto_nodes = 2;
        let (graph, _intents, truncated) =
            build_initial_graph(&root, WorktreeId::from_usize(1), max_auto_nodes).unwrap();
        assert!(truncated);
        assert!(graph.nodes.len() <= max_auto_nodes);
    }
}
