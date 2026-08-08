use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use worktree::WorktreeId;

use crate::extract::{extract_cargo_workspace, extract_generic};
use crate::intent::StaticIntentProvider;
use crate::{
    GraphStatus, IntentIndex, SemanticGraph, SemanticGraphSnapshot,
};

/// Sync orchestration: Cargo workspace when present, otherwise generic thin extract,
/// then static intent enrichment.
pub fn build_initial_graph(
    root: &Path,
    worktree_id: WorktreeId,
) -> Result<(SemanticGraph, IntentIndex)> {
    let mut graph = SemanticGraph::default();
    if root.join("Cargo.toml").is_file() {
        graph.apply_patch(extract_cargo_workspace(root, worktree_id)?)?;
    } else {
        graph.apply_patch(extract_generic(root, worktree_id, 2)?)?;
    }
    let intents = StaticIntentProvider::enrich(&graph, root)?;
    Ok((graph, intents))
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
    if file_name == "Cargo.toml" {
        InvalidationScope::WorkspacePackages
    } else if path
        .extension()
        .and_then(|ext| ext.to_str())
        == Some("rs")
    {
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
    use crate::NodeKind;

    #[test]
    fn build_initial_graph_cargo_fixture_has_core_lib_intent() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/simple_workspace");
        let (graph, intents) =
            build_initial_graph(&root, WorktreeId::from_usize(1)).unwrap();
        assert!(
            graph
                .nodes
                .values()
                .any(|node| node.kind == NodeKind::Module && node.display_name.as_ref() == "core_lib")
        );
        assert!(!intents.is_empty());

        let snap =
            GraphIndexer::reindex_cargo_or_generic(&root, WorktreeId::from_usize(1)).unwrap();
        assert_eq!(snap.graph.nodes.len(), graph.nodes.len());
        assert_eq!(snap.intents.len(), intents.len());
    }
}
