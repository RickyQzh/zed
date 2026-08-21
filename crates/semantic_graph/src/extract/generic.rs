use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use util::rel_path::RelPath;
use worktree::WorktreeId;

use crate::extract::traits::{FsExtractCtx, SemanticExtractor};
use crate::{
    Edge, GraphPatch, GraphRevision, ModuleKind, ModulePayload, ModuleRef, Node, NodeFlags, NodeId,
    NodeKey, SourceLocation,
};

const SKIP_DIR_NAMES: &[&str] = &[".git", "target", "node_modules"];

/// Thin multi-language folder extractor (no language-specific parsing).
pub struct GenericThinExtractor;

impl SemanticExtractor for GenericThinExtractor {
    fn id(&self) -> &'static str {
        "generic_thin"
    }

    fn priority(&self) -> i32 {
        0
    }

    fn extract_sync(&self, ctx: &FsExtractCtx) -> Result<GraphPatch> {
        extract_generic(&ctx.root, ctx.worktree_id, 2)
    }
}

impl GenericThinExtractor {
    pub fn extract_root(
        root: &Path,
        worktree_id: WorktreeId,
        max_depth: u32,
    ) -> Result<GraphPatch> {
        extract_generic(root, worktree_id, max_depth)
    }
}

/// Sync helper: walk directories up to `max_depth`, emit folder [`Module`](crate::NodeKind::Module) nodes.
pub fn extract_generic(root: &Path, worktree_id: WorktreeId, max_depth: u32) -> Result<GraphPatch> {
    let project_key = NodeKey::Project { worktree_id };
    let project_id = NodeId::from_key(&project_key);
    let project_name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project");
    let project_node = Node::project(project_id, project_key, project_name)
        .with_location(project_readme_location(root, worktree_id)?);

    let mut upsert_nodes = vec![project_node];
    let mut upsert_edges = Vec::new();

    // BFS: (absolute_dir, relative_path, depth, parent_node_id)
    let mut queue: VecDeque<(PathBuf, Arc<RelPath>, u32, NodeId)> = VecDeque::new();
    queue.push_back((root.to_path_buf(), RelPath::empty_arc(), 0, project_id));

    while let Some((abs_dir, rel_path, depth, parent_id)) = queue.pop_front() {
        if depth > 0 {
            let display_name = abs_dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("folder");
            let module_key = NodeKey::Module {
                worktree_id,
                module_ref: ModuleRef::PathModule {
                    path: Arc::clone(&rel_path),
                },
            };
            let module_id = NodeId::from_key(&module_key);
            let module_node = Node::module(
                module_id,
                module_key,
                display_name,
                Some(SourceLocation {
                    worktree_id,
                    path: Arc::clone(&rel_path),
                    range: None,
                    symbol: None,
                }),
                ModulePayload {
                    language: None,
                    module_kind: ModuleKind::Folder,
                    public_exports: Vec::new(),
                    deps_out_count: 0,
                    deps_in_count: 0,
                    loc_estimate: None,
                },
                NodeFlags::default(),
            );
            upsert_nodes.push(module_node);
            upsert_edges.push(Edge::contains(parent_id, module_id));

            if depth >= max_depth {
                continue;
            }

            enqueue_children(&abs_dir, &rel_path, depth, module_id, &mut queue)?;
        } else {
            // Root depth 0: children become top-level modules under Project.
            if max_depth == 0 {
                continue;
            }
            enqueue_children(&abs_dir, &rel_path, depth, parent_id, &mut queue)?;
        }
    }

    // Also emit a Module for the root folder itself when it has a README / src,
    // so static intents can attach README evidence to a Module subject.
    // Prefer a PathModule at "." (empty rel path) owned by the project.
    let root_module_key = NodeKey::Module {
        worktree_id,
        module_ref: ModuleRef::PathModule {
            path: RelPath::empty_arc(),
        },
    };
    let root_module_id = NodeId::from_key(&root_module_key);
    if !upsert_nodes.iter().any(|node| node.id == root_module_id) {
        let root_module = Node::module(
            root_module_id,
            root_module_key,
            project_name,
            Some(SourceLocation {
                worktree_id,
                path: RelPath::empty_arc(),
                range: None,
                symbol: None,
            }),
            ModulePayload {
                language: None,
                module_kind: ModuleKind::Folder,
                public_exports: Vec::new(),
                deps_out_count: 0,
                deps_in_count: 0,
                loc_estimate: None,
            },
            NodeFlags::default(),
        );
        upsert_nodes.push(root_module);
        upsert_edges.push(Edge::contains(project_id, root_module_id));
    }

    Ok(GraphPatch {
        base: GraphRevision(0),
        removed_nodes: Vec::new(),
        removed_edges: Vec::new(),
        upsert_nodes,
        upsert_edges,
    })
}

fn project_readme_location(root: &Path, worktree_id: WorktreeId) -> Result<Option<SourceLocation>> {
    const README_NAMES: &[&str] = &["README.md", "Readme.md", "readme.md"];
    let Some(name) = README_NAMES
        .iter()
        .copied()
        .find(|name| root.join(name).is_file())
    else {
        return Ok(None);
    };
    let path = RelPath::from_unix_str(name)
        .with_context(|| format!("invalid README relative path {name}"))?;
    Ok(Some(SourceLocation {
        worktree_id,
        path: Arc::from(path),
        range: None,
        symbol: None,
    }))
}

fn enqueue_children(
    abs_dir: &Path,
    parent_rel: &Arc<RelPath>,
    parent_depth: u32,
    parent_id: NodeId,
    queue: &mut VecDeque<(PathBuf, Arc<RelPath>, u32, NodeId)>,
) -> Result<()> {
    let entries = std::fs::read_dir(abs_dir)
        .with_context(|| format!("failed to read directory {}", abs_dir.display()))?;
    let mut children: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry =
            entry.with_context(|| format!("failed to read entry under {}", abs_dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to get file type for {}", path.display()))?;
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with('.') && name != ".git" {
            // Skip hidden dirs except we explicitly skip .git via SKIP list;
            // other dotdirs are ignored for the thin map.
            continue;
        }
        if SKIP_DIR_NAMES.contains(&name) {
            continue;
        }
        children.push(path);
    }
    children.sort();
    for child_abs in children {
        let name = child_abs
            .file_name()
            .and_then(|name| name.to_str())
            .context("directory name is not utf-8")?;
        let child_rel = if parent_rel.is_empty() {
            RelPath::from_unix_str(name)
                .with_context(|| format!("invalid relative path {name}"))?
                .into()
        } else {
            Arc::from(
                parent_rel.join(
                    RelPath::from_unix_str(name)
                        .with_context(|| format!("invalid relative path component {name}"))?,
                ),
            )
        };
        queue.push_back((child_abs, child_rel, parent_depth + 1, parent_id));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;
    use worktree::WorktreeId;

    use super::{GenericThinExtractor, extract_generic};
    use crate::intent::StaticIntentProvider;
    use crate::{NodeKind, SemanticGraph};

    #[test]
    fn generic_extractor_uses_readme_for_top_folder() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("README.md"), "# Widgets\n\nUI widgets.\n").unwrap();

        let worktree_id = WorktreeId::from_usize(1);
        let patch = GenericThinExtractor::extract_root(root, worktree_id, 2).unwrap();
        let mut graph = SemanticGraph::default();
        graph.apply_patch(patch).unwrap();

        assert!(
            graph
                .nodes
                .values()
                .any(|node| node.kind == NodeKind::Module),
            "expected at least one Module from generic extract"
        );

        let intents = StaticIntentProvider::enrich(&graph, root).unwrap();
        let has_readme_intent = intents.values().any(|intent| {
            intent.summary.to_lowercase().contains("widget")
                || intent
                    .evidence
                    .iter()
                    .any(|evidence| evidence.excerpt.to_lowercase().contains("widget"))
        });
        assert!(
            has_readme_intent,
            "expected README heading/paragraph in static intents, got: {:?}",
            intents
                .values()
                .map(|intent| intent.summary.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn extract_generic_skips_ignored_directories() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::create_dir_all(root.join(".git/objects")).unwrap();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::create_dir_all(root.join("lib")).unwrap();

        let patch = extract_generic(root, WorktreeId::from_usize(1), 2).unwrap();
        let mut graph = SemanticGraph::default();
        graph.apply_patch(patch).unwrap();

        let module_names: Vec<_> = graph
            .nodes
            .values()
            .filter(|node| node.kind == NodeKind::Module)
            .map(|node| node.display_name.to_string())
            .collect();

        assert!(
            module_names
                .iter()
                .any(|name| name == "src" || name == "lib"),
            "expected src/lib modules, got {module_names:?}"
        );
        assert!(
            !module_names.iter().any(|name| {
                name == "target" || name == ".git" || name == "node_modules" || name == "debug"
            }),
            "ignored dirs leaked into modules: {module_names:?}"
        );
    }
}
