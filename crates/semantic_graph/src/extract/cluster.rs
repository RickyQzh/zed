use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, Result};
use gpui::SharedString;
use serde::Deserialize;

use crate::{
    Edge, EdgeId, EdgeKind, GraphPatch, ModuleRef, Node, NodeFlags, NodeId, NodeKey, NodeKind,
    SemanticGraph, SubsystemPayload,
};

/// Clustering knobs (settings defaults: min 3, max 16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClusterConfig {
    pub min_subsystems: usize,
    pub max_subsystems: usize,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            min_subsystems: 3,
            max_subsystems: 16,
        }
    }
}

/// User / repo pins from `semantic_map.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PinConfig {
    /// Subsystem slug → exact member module paths (e.g. `crates/app`).
    pub subsystems: Vec<PinnedSubsystem>,
    /// Exact module path → forced subsystem slug.
    pub module_pins: Vec<ModulePin>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedSubsystem {
    pub slug: String,
    pub members: Vec<String>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModulePin {
    pub path: String,
    pub subsystem: String,
}

#[derive(Debug, Deserialize)]
struct RawPinFile {
    #[serde(default)]
    subsystems: BTreeMap<String, RawSubsystem>,
    #[serde(default)]
    pins: RawPins,
}

#[derive(Debug, Deserialize, Default)]
struct RawSubsystem {
    #[serde(default)]
    members: Vec<String>,
    summary: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct RawPins {
    #[serde(default)]
    modules: BTreeMap<String, RawModulePin>,
}

#[derive(Debug, Deserialize)]
struct RawModulePin {
    subsystem: String,
}

/// Parse repo-root `semantic_map.toml` into [`PinConfig`].
pub fn load_pin_config(text: &str) -> Result<PinConfig> {
    let raw: RawPinFile =
        toml::from_str(text).context("failed to parse semantic_map.toml")?;
    let mut subsystems: Vec<_> = raw
        .subsystems
        .into_iter()
        .map(|(slug, value)| PinnedSubsystem {
            slug,
            members: value.members,
            summary: value.summary,
        })
        .collect();
    subsystems.sort_by(|left, right| left.slug.cmp(&right.slug));

    let mut module_pins: Vec<_> = raw
        .pins
        .modules
        .into_iter()
        .map(|(path, value)| ModulePin {
            path,
            subsystem: value.subsystem,
        })
        .collect();
    module_pins.sort_by(|left, right| left.path.cmp(&right.path));

    Ok(PinConfig {
        subsystems,
        module_pins,
    })
}

/// Load pins from `root/semantic_map.toml` when present.
pub fn load_pin_config_from_root(root: &std::path::Path) -> Result<PinConfig> {
    let path = root.join("semantic_map.toml");
    if !path.is_file() {
        return Ok(PinConfig::default());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    load_pin_config(&text)
}

/// Cluster top-level Modules into Subsystem nodes; rewrite Contains forest.
///
/// v1 algorithm (spec §5.3.3): seed one cluster per project-child Module, merge
/// by undirected DependsOn affinity until count ≤ max (and prefer ≥ min when
/// affinity remains), name from path segment / package prefix, then apply pins.
pub fn cluster_subsystems(
    graph: &SemanticGraph,
    config: ClusterConfig,
    pins: &PinConfig,
) -> Result<GraphPatch> {
    let project_id = graph
        .nodes
        .values()
        .find(|node| node.kind == NodeKind::Project)
        .map(|node| node.id);
    let Some(project_id) = project_id else {
        return Ok(GraphPatch {
            base: graph.revision(),
            ..GraphPatch::default()
        });
    };

    let project_key = graph
        .nodes
        .get(&project_id)
        .map(|node| node.key.clone())
        .context("missing project node")?;
    let NodeKey::Project { .. } = &project_key else {
        anyhow::bail!("project node key is not Project");
    };

    let top_modules: Vec<NodeId> = graph
        .edges
        .values()
        .filter(|edge| edge.kind == EdgeKind::Contains && edge.from == project_id)
        .filter_map(|edge| {
            let node = graph.nodes.get(&edge.to)?;
            (node.kind == NodeKind::Module).then_some(edge.to)
        })
        .collect();

    if top_modules.is_empty() {
        return Ok(GraphPatch {
            base: graph.revision(),
            ..GraphPatch::default()
        });
    }

    let mut clusters = seed_clusters(graph, &top_modules);
    agglomerate(&mut clusters, graph, config);

    // module_id → slug
    let mut assignment: BTreeMap<NodeId, String> = BTreeMap::new();
    for cluster in &clusters {
        for &module_id in &cluster.members {
            assignment.insert(module_id, cluster.slug.clone());
        }
    }

    let mut pinned_modules: BTreeSet<NodeId> = BTreeSet::new();
    apply_pins(
        graph,
        pins,
        &top_modules,
        &mut assignment,
        &mut pinned_modules,
    );

    // Ensure every top module is assigned.
    for &module_id in &top_modules {
        assignment
            .entry(module_id)
            .or_insert_with(|| "uncategorized".to_string());
    }

    // Group by slug.
    let mut by_slug: BTreeMap<String, Vec<NodeId>> = BTreeMap::new();
    for (module_id, slug) in &assignment {
        by_slug.entry(slug.clone()).or_default().push(*module_id);
    }

    let mut upsert_nodes = Vec::new();
    let mut upsert_edges = Vec::new();
    let mut removed_edges = Vec::new();

    // Drop Project → top Module Contains edges (reparent under subsystems).
    for edge in graph.edges.values() {
        if edge.kind == EdgeKind::Contains
            && edge.from == project_id
            && top_modules.contains(&edge.to)
        {
            removed_edges.push(edge.id);
        }
    }

    for (slug, members) in by_slug {
        let subsystem_key = NodeKey::Subsystem {
            project: project_key.clone().into(),
            slug: SharedString::from(slug.as_str()),
        };
        let subsystem_id = NodeId::from_key(&subsystem_key);
        let pinned = members.iter().any(|id| pinned_modules.contains(id));
        let cluster_score = if pinned { 1.0 } else { affinity_score(graph, &members) };
        upsert_nodes.push(Node::subsystem(
            subsystem_id,
            subsystem_key,
            slug.as_str(),
            SubsystemPayload {
                member_count: members.len() as u32,
                cluster_score,
                pinned,
            },
            NodeFlags {
                user_pinned_subsystem: pinned,
                ..NodeFlags::default()
            },
        ));
        upsert_edges.push(Edge::contains(project_id, subsystem_id));

        for module_id in members {
            upsert_edges.push(Edge::contains(subsystem_id, module_id));
            if pinned_modules.contains(&module_id) {
                if let Some(node) = graph.nodes.get(&module_id) {
                    let mut updated = node.clone();
                    updated.flags.user_pinned_subsystem = true;
                    upsert_nodes.push(updated);
                }
            }
        }
    }

    Ok(GraphPatch {
        base: graph.revision(),
        removed_nodes: Vec::new(),
        removed_edges,
        upsert_nodes,
        upsert_edges,
    })
}

#[derive(Debug, Clone)]
struct Cluster {
    slug: String,
    members: Vec<NodeId>,
}

fn seed_clusters(graph: &SemanticGraph, top_modules: &[NodeId]) -> Vec<Cluster> {
    // Prefer grouping by shared path prefix under repo roots like crates/, apps/.
    let mut by_prefix: BTreeMap<String, Vec<NodeId>> = BTreeMap::new();
    for &module_id in top_modules {
        let Some(node) = graph.nodes.get(&module_id) else {
            continue;
        };
        let path = module_path(node).unwrap_or_else(|| node.display_name.to_string());
        let prefix = seed_prefix(&path);
        by_prefix.entry(prefix).or_default().push(module_id);
    }

    by_prefix
        .into_iter()
        .map(|(prefix, members)| {
            let slug = if members.len() == 1 {
                graph
                    .nodes
                    .get(&members[0])
                    .map(|node| node.display_name.to_string())
                    .unwrap_or(prefix)
            } else {
                prefix
            };
            Cluster { slug, members }
        })
        .collect()
}

fn seed_prefix(path: &str) -> String {
    let parts: Vec<_> = path.split('/').filter(|part| !part.is_empty()).collect();
    match parts.as_slice() {
        [root, _rest @ ..] if matches!(*root, "crates" | "apps" | "packages" | "libs") => {
            // Seed as one-per-package under those roots (second segment), not the whole crates/.
            if parts.len() >= 2 {
                parts[1].to_string()
            } else {
                root.to_string()
            }
        }
        [first, ..] => (*first).to_string(),
        [] => "subsystem".to_string(),
    }
}

fn agglomerate(clusters: &mut Vec<Cluster>, graph: &SemanticGraph, config: ClusterConfig) {
    let max = config.max_subsystems.max(1);
    let min = config.min_subsystems.min(max).max(1);
    let mut merge_counter = 0usize;

    while clusters.len() > max {
        if !merge_best_pair(clusters, graph, false, &mut merge_counter) {
            merge_best_pair(clusters, graph, true, &mut merge_counter);
        }
    }

    // Continue merging while above min and a positive-affinity pair exists.
    while clusters.len() > min {
        if !merge_best_pair(clusters, graph, false, &mut merge_counter) {
            break;
        }
    }

    // Deduplicate / normalize empty.
    clusters.retain(|cluster| !cluster.members.is_empty());
}

fn merge_best_pair(
    clusters: &mut Vec<Cluster>,
    graph: &SemanticGraph,
    allow_zero: bool,
    merge_counter: &mut usize,
) -> bool {
    if clusters.len() < 2 {
        return false;
    }

    let mut best: Option<(usize, usize, f32)> = None;
    for left in 0..clusters.len() {
        for right in (left + 1)..clusters.len() {
            let score = cluster_pair_affinity(graph, &clusters[left], &clusters[right]);
            if !allow_zero && score <= 0.0 {
                continue;
            }
            let replace = match best {
                None => true,
                Some((_, _, best_score)) => {
                    score > best_score
                        || (score == best_score
                            && (left, right)
                                < (best.map(|(a, b, _)| (a, b)).unwrap_or((left, right))))
                }
            };
            if replace {
                best = Some((left, right, score));
            }
        }
    }

    let Some((left, right, _)) = best else {
        return false;
    };

    let right_cluster = clusters.remove(right);
    let left_cluster = &mut clusters[left];
    left_cluster.members.extend(right_cluster.members);
    left_cluster.slug = merged_slug(&left_cluster.slug.clone(), &right_cluster.slug, merge_counter);
    true
}

fn merged_slug(left: &str, right: &str, merge_counter: &mut usize) -> String {
    if left == right {
        return left.to_string();
    }
    // Synthetic names use `subsystem-{n}`; never collapse those on the shared
    // "subsystem" token (e.g. subsystem-19 + subsystem-18 must not become "subsystem").
    if !is_synthetic_subsystem_slug(left) && !is_synthetic_subsystem_slug(right) {
        let left_parts: Vec<_> = left.split(|c| c == '-' || c == '_').collect();
        let right_parts: Vec<_> = right.split(|c| c == '-' || c == '_').collect();
        if let (Some(a), Some(b)) = (left_parts.first(), right_parts.first()) {
            if a == b && !a.is_empty() && *a != "subsystem" {
                return (*a).to_string();
            }
        }
    }
    *merge_counter += 1;
    format!("subsystem-{merge_counter}")
}

fn is_synthetic_subsystem_slug(slug: &str) -> bool {
    slug.strip_prefix("subsystem-")
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

fn cluster_pair_affinity(graph: &SemanticGraph, left: &Cluster, right: &Cluster) -> f32 {
    let mut score = 0.0;
    for &from in &left.members {
        for &to in &right.members {
            if depends_either_way(graph, from, to) {
                score += 1.0;
            }
        }
    }
    score
}

fn depends_either_way(graph: &SemanticGraph, a: NodeId, b: NodeId) -> bool {
    let ab = EdgeId::from_endpoints(EdgeKind::DependsOn.discriminant(), a, b);
    let ba = EdgeId::from_endpoints(EdgeKind::DependsOn.discriminant(), b, a);
    graph.edges.contains_key(&ab) || graph.edges.contains_key(&ba)
}

fn affinity_score(graph: &SemanticGraph, members: &[NodeId]) -> f32 {
    if members.len() < 2 {
        return 0.0;
    }
    let mut score = 0.0;
    for i in 0..members.len() {
        for j in (i + 1)..members.len() {
            if depends_either_way(graph, members[i], members[j]) {
                score += 1.0;
            }
        }
    }
    score
}

fn apply_pins(
    graph: &SemanticGraph,
    pins: &PinConfig,
    top_modules: &[NodeId],
    assignment: &mut BTreeMap<NodeId, String>,
    pinned_modules: &mut BTreeSet<NodeId>,
) {
    for subsystem in &pins.subsystems {
        for member in &subsystem.members {
            if let Some(module_id) = find_top_module_by_member_path(graph, top_modules, member) {
                assignment.insert(module_id, subsystem.slug.clone());
                pinned_modules.insert(module_id);
            }
        }
    }
    for module_pin in &pins.module_pins {
        if let Some(module_id) =
            find_top_module_by_member_path(graph, top_modules, &module_pin.path)
        {
            assignment.insert(module_id, module_pin.subsystem.clone());
            pinned_modules.insert(module_id);
        }
    }
}

fn find_top_module_by_member_path(
    graph: &SemanticGraph,
    top_modules: &[NodeId],
    member: &str,
) -> Option<NodeId> {
    let normalized = member.trim_matches('/');
    top_modules.iter().find_map(|&module_id| {
        let node = graph.nodes.get(&module_id)?;
        let path = module_path(node)?;
        if path == normalized {
            return Some(module_id);
        }
        if node.display_name.as_ref() == normalized {
            return Some(module_id);
        }
        None
    })
}

fn module_path(node: &Node) -> Option<String> {
    match &node.key {
        NodeKey::Module { module_ref, .. } => match module_ref {
            ModuleRef::CargoPackage { manifest_dir, .. } => {
                Some(manifest_dir.as_unix_str().to_string())
            }
            ModuleRef::PathModule { path } => Some(path.as_unix_str().to_string()),
            ModuleRef::LanguagePackage { root, .. } => Some(root.as_unix_str().to_string()),
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use util::rel_path::RelPath;
    use worktree::WorktreeId;

    use super::{
        cluster_subsystems, load_pin_config, merged_slug, ClusterConfig, PinConfig, PinnedSubsystem,
    };
    use crate::{
        Edge, EdgeKind, GraphPatch, ModuleKind, ModulePayload, ModuleRef, Node, NodeFlags, NodeId,
        NodeKey, NodeKind, SemanticGraph,
    };

    fn path_module(worktree_id: WorktreeId, path: &str, name: &str) -> Node {
        let rel = RelPath::from_unix_str(path).unwrap();
        let key = NodeKey::Module {
            worktree_id,
            module_ref: ModuleRef::PathModule {
                path: Arc::from(rel),
            },
        };
        let id = NodeId::from_key(&key);
        Node::module(
            id,
            key,
            name,
            None,
            ModulePayload {
                language: None,
                module_kind: ModuleKind::Folder,
                public_exports: Vec::new(),
                deps_out_count: 0,
                deps_in_count: 0,
                loc_estimate: None,
            },
            NodeFlags::default(),
        )
    }

    #[test]
    fn clusterer_respects_pins() {
        let worktree_id = WorktreeId::from_usize(1);
        let project_key = NodeKey::Project { worktree_id };
        let project_id = NodeId::from_key(&project_key);
        let project = Node::project(project_id, project_key, "demo");

        let module_a = path_module(worktree_id, "crates/a", "a");
        let module_b = path_module(worktree_id, "crates/b", "b");
        let module_c = path_module(worktree_id, "crates/c", "c");
        let id_a = module_a.id;
        let id_b = module_b.id;
        let id_c = module_c.id;

        let mut graph = SemanticGraph::default();
        graph
            .apply_patch(GraphPatch {
                base: graph.revision(),
                removed_nodes: vec![],
                removed_edges: vec![],
                upsert_nodes: vec![project, module_a, module_b, module_c],
                upsert_edges: vec![
                    Edge::contains(project_id, id_a),
                    Edge::contains(project_id, id_b),
                    Edge::contains(project_id, id_c),
                ],
            })
            .unwrap();

        let pins = PinConfig {
            subsystems: vec![PinnedSubsystem {
                slug: "ui".into(),
                members: vec!["crates/a".into(), "crates/b".into()],
                summary: None,
            }],
            module_pins: vec![],
        };

        let patch = cluster_subsystems(&graph, ClusterConfig::default(), &pins).unwrap();
        graph.apply_patch(patch).unwrap();

        let ui_id = graph
            .nodes
            .values()
            .find(|node| node.kind == NodeKind::Subsystem && node.display_name.as_ref() == "ui")
            .map(|node| node.id)
            .expect("subsystem:ui");

        let ui_children: Vec<_> = graph
            .edges
            .values()
            .filter(|edge| edge.kind == EdgeKind::Contains && edge.from == ui_id)
            .map(|edge| edge.to)
            .collect();
        assert!(ui_children.contains(&id_a));
        assert!(ui_children.contains(&id_b));
        assert!(!ui_children.contains(&id_c));
    }

    #[test]
    fn load_pin_config_parses_subsystems_and_module_pins() {
        let text = r#"
[subsystems.ui]
members = ["crates/app"]
summary = "UI surface"

[pins.modules."crates/sandbox"]
subsystem = "agent"
"#;
        let pins = load_pin_config(text).unwrap();
        assert_eq!(pins.subsystems.len(), 1);
        assert_eq!(pins.subsystems[0].slug, "ui");
        assert_eq!(pins.subsystems[0].members, vec!["crates/app"]);
        assert_eq!(pins.module_pins.len(), 1);
        assert_eq!(pins.module_pins[0].path, "crates/sandbox");
        assert_eq!(pins.module_pins[0].subsystem, "agent");
    }

    #[test]
    fn merged_slug_does_not_collapse_synthetic_subsystem_names() {
        let mut counter = 0usize;
        let merged = merged_slug("subsystem-19", "subsystem-18", &mut counter);
        assert_ne!(merged, "subsystem");
        assert!(
            merged.starts_with("subsystem-"),
            "expected synthetic slug, got {merged}"
        );
        assert_eq!(counter, 1);

        // Non-synthetic shared prefixes still collapse to the common token.
        let shared = merged_slug("editor-core", "editor-ui", &mut counter);
        assert_eq!(shared, "editor");
        assert_eq!(counter, 1);
    }
}
