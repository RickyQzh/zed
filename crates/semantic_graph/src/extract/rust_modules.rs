use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use util::rel_path::RelPath;
use worktree::WorktreeId;

use crate::{
    Edge, GraphPatch, GraphRevision, ModuleKind, ModulePayload, ModuleRef, Node, NodeFlags, NodeId,
    NodeKey, SourceLocation,
};

/// Depth-limited filesystem module discovery under a Cargo package `src/`.
///
/// Phase A does not parse `mod` declarations; it maps `src/*.rs` and `src/*/`
/// directory trees up to `depth`.
pub fn extract_rust_modules(
    workspace_root: &Path,
    package_root: &Path,
    package_node_id: NodeId,
    worktree_id: WorktreeId,
    depth: u32,
) -> Result<GraphPatch> {
    extract_rust_modules_with_base(
        workspace_root,
        package_root,
        package_node_id,
        worktree_id,
        depth,
        GraphRevision(0),
    )
}

pub fn extract_rust_modules_with_base(
    workspace_root: &Path,
    package_root: &Path,
    package_node_id: NodeId,
    worktree_id: WorktreeId,
    depth: u32,
    base: GraphRevision,
) -> Result<GraphPatch> {
    let src = package_root.join("src");
    let has_crate_root = src.join("lib.rs").is_file() || src.join("main.rs").is_file();
    if !has_crate_root || depth == 0 {
        return Ok(GraphPatch {
            base,
            removed_nodes: Vec::new(),
            removed_edges: Vec::new(),
            upsert_nodes: Vec::new(),
            upsert_edges: Vec::new(),
        });
    }

    let mut upsert_nodes = Vec::new();
    let mut upsert_edges = Vec::new();

    // (abs_path, rel_unix_path, parent_node, remaining_depth, is_dir)
    let mut queue: VecDeque<(PathBuf, String, NodeId, u32, bool)> = VecDeque::new();
    queue.push_back((src.clone(), String::new(), package_node_id, depth, true));

    while let Some((abs_path, rel_under_src, parent_id, remaining, is_dir)) = queue.pop_front() {
        if remaining == 0 {
            continue;
        }

        if is_dir {
            let entries = match std::fs::read_dir(&abs_path) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            let mut children: Vec<_> = entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .collect();
            children.sort();

            for child in children {
                let file_name = match child.file_name().and_then(|name| name.to_str()) {
                    Some(name) => name.to_string(),
                    None => continue,
                };

                if file_name == "bin" && rel_under_src.is_empty() {
                    continue;
                }

                if child.is_dir() {
                    let child_rel = join_under_src(&rel_under_src, &file_name);
                    let module_rel = module_rel_path(workspace_root, package_root, &child_rel)?;
                    let node = file_module_node(
                        worktree_id,
                        &module_rel,
                        &file_name,
                        ModuleKind::FileModule,
                    )?;
                    let node_id = node.id;
                    upsert_nodes.push(node);
                    upsert_edges.push(Edge::contains(parent_id, node_id));
                    queue.push_back((child, child_rel, node_id, remaining - 1, true));
                } else if child.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                    let stem = match child.file_stem().and_then(|stem| stem.to_str()) {
                        Some(stem) => stem,
                        None => continue,
                    };
                    if rel_under_src.is_empty() && matches!(stem, "lib" | "main" | "mod") {
                        continue;
                    }
                    if stem == "mod" {
                        continue;
                    }
                    let child_rel = join_under_src(&rel_under_src, &format!("{stem}.rs"));
                    let module_rel = module_rel_path(workspace_root, package_root, &child_rel)?;
                    let node =
                        file_module_node(worktree_id, &module_rel, stem, ModuleKind::FileModule)?;
                    let node_id = node.id;
                    upsert_nodes.push(node);
                    upsert_edges.push(Edge::contains(parent_id, node_id));
                    // File modules are leaves for FS discovery (no further children).
                }
            }
        }
    }

    Ok(GraphPatch {
        base,
        removed_nodes: Vec::new(),
        removed_edges: Vec::new(),
        upsert_nodes,
        upsert_edges,
    })
}

fn join_under_src(rel_under_src: &str, name: &str) -> String {
    if rel_under_src.is_empty() {
        name.to_string()
    } else {
        format!("{rel_under_src}/{name}")
    }
}

fn module_rel_path(
    workspace_root: &Path,
    package_root: &Path,
    under_src: &str,
) -> Result<Arc<RelPath>> {
    let abs = package_root.join("src").join(under_src);
    rel_path_from_root(workspace_root, &abs)
}

fn file_module_node(
    worktree_id: WorktreeId,
    path: &Arc<RelPath>,
    display_name: &str,
    module_kind: ModuleKind,
) -> Result<Node> {
    let key = NodeKey::Module {
        worktree_id,
        module_ref: ModuleRef::PathModule {
            path: Arc::clone(path),
        },
    };
    let id = NodeId::from_key(&key);
    Ok(Node::module(
        id,
        key,
        display_name,
        Some(SourceLocation {
            worktree_id,
            path: Arc::clone(path),
            range: None,
            symbol: None,
        }),
        ModulePayload {
            language: Some("rust".into()),
            module_kind,
            public_exports: Vec::new(),
            deps_out_count: 0,
            deps_in_count: 0,
            loc_estimate: None,
        },
        NodeFlags::default(),
    ))
}

fn rel_path_from_root(root: &Path, absolute: &Path) -> Result<Arc<RelPath>> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let absolute = absolute
        .canonicalize()
        .unwrap_or_else(|_| normalize_path(absolute));
    let relative = absolute.strip_prefix(&root).with_context(|| {
        format!(
            "path {} is not under workspace root {}",
            absolute.display(),
            root.display()
        )
    })?;
    let unix = relative
        .to_str()
        .with_context(|| format!("non-utf8 relative path {}", relative.display()))?
        .replace('\\', "/");
    let rel = RelPath::from_unix_str(&unix)
        .with_context(|| format!("invalid relative path {unix}"))?;
    Ok(rel.into())
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::tempdir;
    use util::rel_path::RelPath;
    use worktree::WorktreeId;

    use super::extract_rust_modules_with_base;
    use crate::{
        GraphPatch, ModuleKind, ModulePayload, ModuleRef, Node, NodeFlags, NodeId, NodeKey,
        NodeKind, SemanticGraph,
    };

    #[test]
    fn rust_modules_emits_file_and_dir_children() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let package = root.join("crates/pkg");
        std::fs::create_dir_all(package.join("src/helper")).unwrap();
        std::fs::write(package.join("src/lib.rs"), "mod util;\nmod helper;\n").unwrap();
        std::fs::write(package.join("src/util.rs"), "pub fn x() {}\n").unwrap();
        std::fs::write(package.join("src/helper/mod.rs"), "").unwrap();
        std::fs::write(package.join("src/helper/inner.rs"), "").unwrap();

        let worktree_id = WorktreeId::from_usize(1);
        let package_key = NodeKey::Module {
            worktree_id,
            module_ref: ModuleRef::CargoPackage {
                package_name: "pkg".into(),
                manifest_dir: Arc::from(RelPath::from_unix_str("crates/pkg").unwrap()),
            },
        };
        let package_id = NodeId::from_key(&package_key);
        let package_node = Node::module(
            package_id,
            package_key,
            "pkg",
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
        );

        let mut graph = SemanticGraph::default();
        graph
            .apply_patch(GraphPatch {
                base: graph.revision(),
                removed_nodes: vec![],
                removed_edges: vec![],
                upsert_nodes: vec![package_node],
                upsert_edges: vec![],
            })
            .unwrap();

        let patch = extract_rust_modules_with_base(
            root,
            &package,
            package_id,
            worktree_id,
            2,
            graph.revision(),
        )
        .unwrap();
        graph.apply_patch(patch).unwrap();

        let names: Vec<_> = graph
            .nodes
            .values()
            .filter(|node| node.kind == NodeKind::Module && node.display_name.as_ref() != "pkg")
            .map(|node| node.display_name.to_string())
            .collect();
        assert!(names.iter().any(|name| name == "util"), "{names:?}");
        assert!(names.iter().any(|name| name == "helper"), "{names:?}");
        assert!(names.iter().any(|name| name == "inner"), "{names:?}");

        let children = graph.children.get(&package_id).cloned().unwrap_or_default();
        assert!(children.len() >= 2);
    }
}
