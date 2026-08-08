use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use worktree::WorktreeId;

use crate::extract::{
    cluster_subsystems, extract_cargo_workspace, extract_generic, extract_rust_modules_with_base,
    load_pin_config_from_root, ClusterConfig,
};
use crate::intent::StaticIntentProvider;
use crate::{
    GraphStatus, IntentIndex, ModuleRef, NodeKind, SemanticGraph, SemanticGraphSnapshot,
};

const DEFAULT_MODULE_DEPTH: u32 = 2;

/// Sync orchestration: Cargo workspace when present, otherwise generic thin extract,
/// then rust module depth (Cargo), subsystem clustering, then static intents.
pub fn build_initial_graph(
    root: &Path,
    worktree_id: WorktreeId,
) -> Result<(SemanticGraph, IntentIndex)> {
    let mut graph = SemanticGraph::default();
    if root.join("Cargo.toml").is_file() {
        graph.apply_patch(extract_cargo_workspace(root, worktree_id)?)?;
        apply_rust_module_depth(&mut graph, root, worktree_id, DEFAULT_MODULE_DEPTH)?;
    } else {
        graph.apply_patch(extract_generic(root, worktree_id, 2)?)?;
    }

    let pins = load_pin_config_from_root(root)?;
    graph.apply_patch(cluster_subsystems(
        &graph,
        ClusterConfig::default(),
        &pins,
    )?)?;

    let intents = StaticIntentProvider::enrich(&graph, root)?;
    Ok((graph, intents))
}

fn apply_rust_module_depth(
    graph: &mut SemanticGraph,
    root: &Path,
    worktree_id: WorktreeId,
    depth: u32,
) -> Result<()> {
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

    for (package_node_id, manifest_dir) in packages {
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
            graph.apply_patch(patch)?;
        }
    }
    Ok(())
}

/// Sync test helper wrapping [`build_initial_graph`] into a snapshot.
pub struct GraphIndexer;

impl GraphIndexer {
    pub fn reindex_cargo_or_generic(
        root: &Path,
        worktree_id: WorktreeId,
    ) -> Result<SemanticGraphSnapshot> {
        let (graph, intents) = build_initial_graph(root, worktree_id)?;
        Ok(SemanticGraphSnapshot {
            revision: graph.revision(),
            graph: Arc::new(graph),
            intents: Arc::new(intents),
            status: GraphStatus::Idle,
        })
    }
}

/// Placeholder for dirty-path → reindex job mapping (Task 7).
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
        let (graph, intents) = build_initial_graph(&root, WorktreeId::from_usize(1)).unwrap();
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
}
