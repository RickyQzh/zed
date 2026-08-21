use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, Result};
use gpui::SharedString;
use serde::Deserialize;

use crate::{
    Edge, EdgeId, EdgeKind, GraphPatch, ModuleRef, Node, NodeFlags, NodeId, NodeKey, NodeKind,
    SemanticGraph, SourceLocation, SubsystemPayload,
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
    let raw: RawPinFile = toml::from_str(text).context("failed to parse semantic_map.toml")?;
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

/// Slug for leftover modules that are neither pinned nor in a prefix group.
pub const OTHER_SUBSYSTEM_SLUG: &str = "other";

/// Cluster top-level Modules into Subsystem nodes; rewrite Contains forest.
///
/// Pins from `semantic_map.toml` are applied first. Remaining modules are
/// grouped by package-name prefix (`gpui` + `gpui_macros` → `gpui`) when two
/// or more share a root; everything else goes into a single [`OTHER_SUBSYSTEM_SLUG`]
/// bucket. When the result still exceeds `max_subsystems`, the smallest
/// unpinned prefix groups fold into `other` — never a synthetic `subsystem-N`
/// mega-blob with leftover crate-named singletons.
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

    let mut assignment: BTreeMap<NodeId, String> = BTreeMap::new();
    let mut pinned_modules: BTreeSet<NodeId> = BTreeSet::new();
    apply_pins(
        graph,
        pins,
        &top_modules,
        &mut assignment,
        &mut pinned_modules,
    );

    let pinned_slugs: BTreeSet<String> = assignment.values().cloned().collect();
    let unpinned: Vec<NodeId> = top_modules
        .iter()
        .copied()
        .filter(|module_id| !assignment.contains_key(module_id))
        .collect();
    // Reserve room for leftover groups; never steal a slot from an applied pin.
    let unpinned_budget = config
        .max_subsystems
        .max(1)
        .saturating_sub(pinned_slugs.len())
        .max(1);
    assign_unpinned_by_prefix_or_other(
        graph,
        &top_modules,
        &unpinned,
        unpinned_budget,
        &mut assignment,
    );

    // Ensure every top module is assigned.
    for &module_id in &top_modules {
        assignment
            .entry(module_id)
            .or_insert_with(|| OTHER_SUBSYSTEM_SLUG.to_string());
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
        let cluster_score = if pinned {
            1.0
        } else {
            affinity_score(graph, &members)
        };
        let location = member_location_for_subsystem(graph, &slug, &members);
        upsert_nodes.push(
            Node::subsystem(
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
            )
            .with_location(location),
        );
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

fn member_location_for_subsystem(
    graph: &SemanticGraph,
    slug: &str,
    members: &[NodeId],
) -> Option<SourceLocation> {
    let mut candidates: Vec<&Node> = members
        .iter()
        .filter_map(|module_id| graph.nodes.get(module_id))
        .filter(|node| node.location.is_some())
        .collect();
    candidates.sort_by(|left, right| {
        member_open_rank(slug, left)
            .cmp(&member_open_rank(slug, right))
            .then_with(|| left.display_name.as_ref().cmp(right.display_name.as_ref()))
            .then_with(|| left.id.cmp(&right.id))
    });
    candidates.first().and_then(|node| node.location.clone())
}

fn member_open_rank(slug: &str, node: &Node) -> (u8, u8) {
    let test_rank = u8::from(node.flags.is_test);
    let name = node.display_name.as_ref();
    let slug_norm = slug.replace('-', "_");
    let name_fit = if name == slug || name == slug_norm {
        0
    } else if slug.starts_with(name) || slug_norm.starts_with(name) {
        1
    } else {
        2
    };
    (test_rank, name_fit)
}

fn assign_unpinned_by_prefix_or_other(
    graph: &SemanticGraph,
    top_modules: &[NodeId],
    unpinned: &[NodeId],
    max_unpinned_groups: usize,
    assignment: &mut BTreeMap<NodeId, String>,
) {
    if unpinned.is_empty() {
        return;
    }

    let all_names: BTreeSet<String> = top_modules
        .iter()
        .filter_map(|module_id| {
            graph
                .nodes
                .get(module_id)
                .map(|node| node.display_name.to_string())
        })
        .collect();

    let mut by_prefix: BTreeMap<String, Vec<NodeId>> = BTreeMap::new();
    for &module_id in unpinned {
        let Some(node) = graph.nodes.get(&module_id) else {
            assignment.insert(module_id, OTHER_SUBSYSTEM_SLUG.to_string());
            continue;
        };
        let prefix = package_prefix_root(node.display_name.as_ref(), &all_names);
        by_prefix.entry(prefix).or_default().push(module_id);
    }

    let mut prefix_groups: Vec<(String, Vec<NodeId>)> = Vec::new();
    let mut leftovers: Vec<NodeId> = Vec::new();
    for (prefix, members) in by_prefix {
        if members.len() >= 2 && prefix != OTHER_SUBSYSTEM_SLUG {
            prefix_groups.push((prefix, members));
        } else {
            leftovers.extend(members);
        }
    }

    // Keep largest prefix groups; fold the rest (and true singletons) into `other`.
    prefix_groups.sort_by(|left, right| {
        right
            .1
            .len()
            .cmp(&left.1.len())
            .then_with(|| left.0.cmp(&right.0))
    });

    let other_needed = !leftovers.is_empty()
        || prefix_groups.len() > max_unpinned_groups
        || (prefix_groups.len() == max_unpinned_groups && !leftovers.is_empty());
    let group_slots = if other_needed {
        max_unpinned_groups.saturating_sub(1)
    } else {
        max_unpinned_groups
    };

    for (index, (slug, members)) in prefix_groups.into_iter().enumerate() {
        if index < group_slots {
            for module_id in members {
                assignment.insert(module_id, slug.clone());
            }
        } else {
            leftovers.extend(members);
        }
    }

    for module_id in leftovers {
        assignment.insert(module_id, OTHER_SUBSYSTEM_SLUG.to_string());
    }
}

fn package_prefix_root(name: &str, all_names: &BTreeSet<String>) -> String {
    let mut best = name.to_string();
    for candidate in all_names {
        if candidate.len() < best.len()
            && name.starts_with(candidate.as_str())
            && name.as_bytes().get(candidate.len()) == Some(&b'_')
        {
            best = candidate.clone();
        }
    }
    best
}

pub fn is_synthetic_subsystem_slug(slug: &str) -> bool {
    slug.strip_prefix("subsystem-")
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
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
        ClusterConfig, OTHER_SUBSYSTEM_SLUG, PinConfig, PinnedSubsystem, cluster_subsystems,
        is_synthetic_subsystem_slug, load_pin_config,
    };
    use crate::{
        Edge, EdgeKind, GraphPatch, ModuleKind, ModulePayload, ModuleRef, Node, NodeFlags, NodeId,
        NodeKey, NodeKind, SemanticGraph, SourceLocation,
    };

    fn path_module(worktree_id: WorktreeId, path: &str, name: &str) -> Node {
        let rel: Arc<RelPath> = RelPath::from_unix_str(path).unwrap().into();
        let key = NodeKey::Module {
            worktree_id,
            module_ref: ModuleRef::PathModule {
                path: Arc::clone(&rel),
            },
        };
        let id = NodeId::from_key(&key);
        Node::module(
            id,
            key,
            name,
            Some(SourceLocation {
                worktree_id,
                path: rel,
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

    fn subsystem_members(graph: &SemanticGraph, slug: &str) -> Vec<NodeId> {
        let subsystem_id = graph
            .nodes
            .values()
            .find(|node| node.kind == NodeKind::Subsystem && node.display_name.as_ref() == slug)
            .map(|node| node.id)
            .unwrap_or_else(|| panic!("expected subsystem {slug}"));
        graph
            .edges
            .values()
            .filter(|edge| edge.kind == EdgeKind::Contains && edge.from == subsystem_id)
            .map(|edge| edge.to)
            .collect()
    }

    fn graph_with_modules(
        names: &[&str],
        deps: &[(&str, &str)],
    ) -> (SemanticGraph, std::collections::HashMap<String, NodeId>) {
        let worktree_id = WorktreeId::from_usize(1);
        let project_key = NodeKey::Project { worktree_id };
        let project_id = NodeId::from_key(&project_key);
        let project = Node::project(project_id, project_key, "demo");

        let mut modules = Vec::new();
        let mut ids = std::collections::HashMap::new();
        let mut contains = Vec::new();
        for name in names {
            let module = path_module(worktree_id, &format!("crates/{name}"), name);
            contains.push(Edge::contains(project_id, module.id));
            ids.insert((*name).to_string(), module.id);
            modules.push(module);
        }

        let mut edges = contains;
        for (from, to) in deps {
            edges.push(Edge::depends_on(ids[*from], ids[*to]));
        }

        let mut upsert_nodes = vec![project];
        upsert_nodes.extend(modules);

        let mut graph = SemanticGraph::default();
        graph
            .apply_patch(GraphPatch {
                base: graph.revision(),
                removed_nodes: vec![],
                removed_edges: vec![],
                upsert_nodes,
                upsert_edges: edges,
            })
            .unwrap();
        (graph, ids)
    }

    fn cluster_and_apply(graph: &mut SemanticGraph, config: ClusterConfig, pins: &PinConfig) {
        let patch = cluster_subsystems(graph, config, pins).unwrap();
        graph.apply_patch(patch).unwrap();
    }

    fn subsystem_slugs(graph: &SemanticGraph) -> Vec<String> {
        let mut slugs: Vec<String> = graph
            .nodes
            .values()
            .filter(|node| node.kind == NodeKind::Subsystem)
            .map(|node| node.display_name.to_string())
            .collect();
        slugs.sort();
        slugs
    }

    #[test]
    fn leftover_modules_go_to_other_not_singletons_or_mega_blob() {
        // Dense DependsOn mesh used to agglomerate into subsystem-N plus
        // leftover crate-named singletons. Pins first; leftovers → other.
        let (mut graph, ids) = graph_with_modules(
            &[
                "gpui",
                "editor",
                "project",
                "agent",
                "which_key",
                "ztracing",
            ],
            &[
                ("editor", "gpui"),
                ("project", "gpui"),
                ("agent", "gpui"),
                ("editor", "project"),
                ("agent", "editor"),
            ],
        );
        let pins = PinConfig {
            subsystems: vec![PinnedSubsystem {
                slug: "editing".into(),
                members: vec!["crates/editor".into()],
                summary: Some("Editor".into()),
            }],
            module_pins: vec![],
        };

        cluster_and_apply(
            &mut graph,
            ClusterConfig {
                min_subsystems: 3,
                max_subsystems: 4,
            },
            &pins,
        );

        let slugs = subsystem_slugs(&graph);
        assert!(
            slugs.iter().all(|slug| !is_synthetic_subsystem_slug(slug)),
            "affinity merge must not emit subsystem-N, got {slugs:?}"
        );
        assert!(slugs.contains(&"editing".to_string()), "{slugs:?}");
        assert!(
            slugs.contains(&OTHER_SUBSYSTEM_SLUG.to_string()),
            "unlisted crates should share {OTHER_SUBSYSTEM_SLUG}, got {slugs:?}"
        );

        let editing = subsystem_members(&graph, "editing");
        assert_eq!(editing, vec![ids["editor"]]);

        let other = subsystem_members(&graph, OTHER_SUBSYSTEM_SLUG);
        for name in ["gpui", "project", "agent", "which_key", "ztracing"] {
            assert!(
                other.contains(&ids[name]),
                "{name} should be in other, not a singleton subsystem"
            );
        }
        assert_eq!(other.len(), 5);
    }

    #[test]
    fn unpinned_modules_group_by_package_name_prefix() {
        let (mut graph, ids) = graph_with_modules(
            &["gpui", "gpui_macros", "gpui_util", "editor", "which_key"],
            &[("gpui_macros", "gpui"), ("editor", "gpui")],
        );
        cluster_and_apply(&mut graph, ClusterConfig::default(), &PinConfig::default());

        let slugs = subsystem_slugs(&graph);
        assert!(
            slugs.iter().all(|slug| !is_synthetic_subsystem_slug(slug)),
            "got {slugs:?}"
        );
        assert!(slugs.contains(&"gpui".to_string()), "{slugs:?}");
        assert!(
            slugs.contains(&OTHER_SUBSYSTEM_SLUG.to_string()),
            "{slugs:?}"
        );

        let gpui_members = subsystem_members(&graph, "gpui");
        assert!(gpui_members.contains(&ids["gpui"]));
        assert!(gpui_members.contains(&ids["gpui_macros"]));
        assert!(gpui_members.contains(&ids["gpui_util"]));

        let other = subsystem_members(&graph, OTHER_SUBSYSTEM_SLUG);
        assert!(other.contains(&ids["editor"]));
        assert!(other.contains(&ids["which_key"]));
        assert!(!other.contains(&ids["gpui"]));
    }

    #[test]
    fn extra_prefix_groups_fold_into_other_when_over_max() {
        let (mut graph, ids) = graph_with_modules(
            &[
                "gpui",
                "gpui_macros",
                "agent",
                "agent_ui",
                "git",
                "git_ui",
                "solo",
            ],
            &[],
        );
        cluster_and_apply(
            &mut graph,
            ClusterConfig {
                min_subsystems: 1,
                max_subsystems: 2,
            },
            &PinConfig::default(),
        );

        let slugs = subsystem_slugs(&graph);
        assert!(
            slugs.iter().all(|slug| !is_synthetic_subsystem_slug(slug)),
            "got {slugs:?}"
        );
        assert!(slugs.len() <= 2, "expected ≤ max_subsystems, got {slugs:?}");
        assert!(
            slugs.contains(&OTHER_SUBSYSTEM_SLUG.to_string()),
            "overflow prefix groups must fold into other, got {slugs:?}"
        );
        assert!(
            subsystem_members(&graph, OTHER_SUBSYSTEM_SLUG).contains(&ids["solo"]),
            "true singletons belong in other"
        );
    }

    #[test]
    fn subsystem_location_comes_from_member_modules() {
        let (mut graph, _) = graph_with_modules(&["gpui", "gpui_macros", "editor"], &[]);
        cluster_and_apply(&mut graph, ClusterConfig::default(), &PinConfig::default());

        let subsystems: Vec<_> = graph
            .nodes
            .values()
            .filter(|node| node.kind == NodeKind::Subsystem)
            .collect();
        assert!(!subsystems.is_empty(), "expected clustered subsystems");
        for node in subsystems {
            assert!(
                node.location.is_some(),
                "subsystem {} should have a location when members have locations",
                node.display_name
            );
        }
    }

    #[test]
    fn subsystem_location_prefers_namesake_member() {
        let (mut graph, ids) = graph_with_modules(&["editor", "language"], &[]);
        cluster_and_apply(
            &mut graph,
            ClusterConfig::default(),
            &PinConfig {
                subsystems: vec![PinnedSubsystem {
                    slug: "editing".into(),
                    members: vec!["crates/editor".into(), "crates/language".into()],
                    summary: None,
                }],
                module_pins: Vec::new(),
            },
        );

        let editing = graph
            .nodes
            .values()
            .find(|node| {
                node.kind == NodeKind::Subsystem && node.display_name.as_ref() == "editing"
            })
            .expect("editing subsystem");
        let editor_location = graph
            .nodes
            .get(&ids["editor"])
            .and_then(|node| node.location.clone());
        assert_eq!(
            editing.location, editor_location,
            "double-click editing should open editor"
        );
    }
}
