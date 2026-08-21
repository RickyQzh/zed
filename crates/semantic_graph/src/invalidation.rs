use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use collections::{FxHasher, HashSet};
use gpui::SharedString;
use util::rel_path::RelPath;
use worktree::WorktreeId;

use crate::extract::{
    cluster_subsystems, extract_cargo_workspace, extract_generic, extract_rust_modules_with_base,
    load_pin_config_from_root, ClusterConfig, PinConfig,
};
use crate::intent::{LlmIntentProvider, StaticIntentProvider};
use crate::{
    Confidence, ContentHash, EdgeId, Evidence, EvidenceKind, GraphPatch, GraphStatus, Intent,
    IntentIndex, IntentSource, ModuleRef, NodeId, NodeKind, SemanticGraph, SemanticGraphSnapshot,
    SourceLocation, Timestamp,
};

/// Settings / knobs for [`build_initial_graph`] (and store reindex).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildGraphOptions {
    pub max_auto_nodes: usize,
    pub module_depth: u32,
    pub cluster: ClusterConfig,
    pub intent_llm: bool,
}

impl Default for BuildGraphOptions {
    fn default() -> Self {
        Self {
            max_auto_nodes: usize::MAX,
            // Matches `semantic_map.module_depth` default in settings.
            module_depth: 3,
            cluster: ClusterConfig::default(),
            intent_llm: false,
        }
    }
}

/// Sync orchestration: Cargo workspace when present, otherwise generic thin extract,
/// then rust module depth (Cargo), subsystem clustering, then static intents
/// (including pin `summary` → Subsystem intents), then optional LLM enrichment
/// (settings-gated stub).
///
/// When the resulting graph exceeds `max_auto_nodes`, excess nodes are dropped and the
/// third return value is `true` (caller should surface [`GraphStatus::Partial`]).
///
/// `options.intent_llm` gates [`LlmIntentProvider`]; when false (default), enrichment
/// is skipped and no model service is required.
pub fn build_initial_graph(
    root: &Path,
    worktree_id: WorktreeId,
    options: BuildGraphOptions,
) -> Result<(SemanticGraph, IntentIndex, bool)> {
    let max_auto_nodes = options.max_auto_nodes;
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
                options.module_depth,
                max_auto_nodes,
            )?;
        }
    } else {
        let patch = extract_generic(root, worktree_id, options.module_depth)?;
        truncated |= apply_patch_with_budget(&mut graph, patch, max_auto_nodes)?;
    }

    // Always cluster (including pin-driven subsystems), then trim so Subsystem
    // nodes outrank Modules/Entries when near the auto-node cap.
    let pins = load_pin_config_from_root(root)?;
    let patch = cluster_subsystems(&graph, options.cluster, &pins)?;
    graph.apply_patch(patch)?;

    let mut intents = StaticIntentProvider::enrich(&graph, root)?;
    for intent in intents_from_pin_summaries(&graph, &pins, worktree_id, root)? {
        intents.insert(intent.subject, intent);
    }
    for intent in LlmIntentProvider.enrich_if_enabled(options.intent_llm, &graph, &intents) {
        intents.insert(intent.subject, intent);
    }
    truncated |= enforce_max_auto_nodes(&mut graph, &mut intents, max_auto_nodes)?;
    Ok((graph, intents, truncated))
}

/// Emit Static Subsystem intents from `semantic_map.toml` pin `summary` fields.
fn intents_from_pin_summaries(
    graph: &SemanticGraph,
    pins: &PinConfig,
    worktree_id: WorktreeId,
    _root: &Path,
) -> Result<Vec<Intent>> {
    let pin_location = RelPath::from_unix_str("semantic_map.toml")
        .ok()
        .map(|path| SourceLocation {
            worktree_id,
            path: Arc::from(path),
            range: None,
            symbol: None,
        });

    let mut intents = Vec::new();
    let updated_at = Timestamp(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0),
    );

    for pin in &pins.subsystems {
        let Some(summary) = pin.summary.as_ref() else {
            continue;
        };
        let summary = summary.trim();
        if summary.is_empty() {
            continue;
        }
        let Some(subject) = graph.nodes.values().find_map(|node| {
            (node.kind == NodeKind::Subsystem && node.display_name.as_ref() == pin.slug)
                .then_some(node.id)
        }) else {
            continue;
        };

        let excerpt = SharedString::from(summary.to_string());
        let evidence = vec![Evidence {
            kind: EvidenceKind::ManifestMetadata,
            location: pin_location.clone(),
            excerpt: excerpt.clone(),
            weight: 1.0,
        }];
        let content_hash = {
            let mut hasher = FxHasher::default();
            EvidenceKind::ManifestMetadata.hash(&mut hasher);
            excerpt.hash(&mut hasher);
            ContentHash(hasher.finish())
        };

        intents.push(Intent {
            subject,
            summary: SharedString::from(format!("{}: {summary}", pin.slug)),
            bullets: Vec::new(),
            confidence: Confidence::High,
            source: IntentSource::Static,
            evidence,
            updated_at,
            content_hash,
        });
    }
    Ok(intents)
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
/// Retention priority: Project → Subsystem → Module → Type → Entry → External.
/// Entry and External are dropped before Module/Subsystem so a tight cap keeps
/// the crate map rather than lib/bin entry nodes.
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
        NodeKind::Type => 3,
        NodeKind::Entry => 4,
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

/// Sync helper wrapping [`build_initial_graph`] into a snapshot (tests and UI).
pub struct GraphIndexer;

impl GraphIndexer {
    pub fn reindex_cargo_or_generic(
        root: &Path,
        worktree_id: WorktreeId,
    ) -> Result<SemanticGraphSnapshot> {
        Self::reindex_with_options(root, worktree_id, BuildGraphOptions::default())
    }

    pub fn reindex_with_options(
        root: &Path,
        worktree_id: WorktreeId,
        options: BuildGraphOptions,
    ) -> Result<SemanticGraphSnapshot> {
        let (graph, intents, truncated) = build_initial_graph(root, worktree_id, options)?;
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

    use std::sync::Arc;

    use util::rel_path::RelPath;

    use super::{build_initial_graph, enforce_max_auto_nodes, BuildGraphOptions, GraphIndexer};
    use crate::intent::StaticIntentProvider;
    use crate::{
        Edge, EdgeKind, EcosystemKind, EntryKind, ExternalPayload, GraphPatch, IntentIndex, ModuleKind,
        ModulePayload, ModuleRef, Node, NodeFlags, NodeId, NodeKey, NodeKind, NodePayload,
        SemanticGraph, SubsystemPayload, SymbolKey, SymbolKind,
    };

    fn build_opts(max_auto_nodes: usize, intent_llm: bool) -> BuildGraphOptions {
        BuildGraphOptions {
            max_auto_nodes,
            intent_llm,
            ..BuildGraphOptions::default()
        }
    }

    #[test]
    fn build_initial_graph_cargo_fixture_has_core_lib_intent() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/simple_workspace");
        let (graph, intents, truncated) =
            build_initial_graph(&root, WorktreeId::from_usize(1), build_opts(usize::MAX, false))
                .unwrap();
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
        let ui_intent = intents.get(&ui_id).expect("pin summary should become ui intent");
        assert!(
            ui_intent.summary.to_lowercase().contains("application binary"),
            "expected pin summary in ui intent, got {:?}",
            ui_intent.summary
        );
        assert!(matches!(ui_intent.source, crate::IntentSource::Static));

        let snap =
            GraphIndexer::reindex_cargo_or_generic(&root, WorktreeId::from_usize(1)).unwrap();
        assert_eq!(snap.graph.nodes.len(), graph.nodes.len());
        assert_eq!(snap.intents.len(), intents.len());
    }

    #[test]
    fn build_initial_graph_truncates_when_over_max_auto_nodes() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/simple_workspace");
        let max_auto_nodes = 2;
        let (graph, _intents, truncated) = build_initial_graph(
            &root,
            WorktreeId::from_usize(1),
            build_opts(max_auto_nodes, false),
        )
        .unwrap();
        assert!(truncated);
        assert!(graph.nodes.len() <= max_auto_nodes);
    }

    #[test]
    fn build_initial_graph_clusters_before_trim_prefers_subsystems() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/simple_workspace");
        // Budget equal to post-extract size would previously skip clustering entirely.
        // Cluster then trim must still surface Subsystem nodes (pins / auto clusters)
        // ahead of dropping Modules.
        let unlimited =
            build_initial_graph(&root, WorktreeId::from_usize(1), build_opts(usize::MAX, false))
                .unwrap();
        let (full_graph, _, _) = unlimited;
        let module_only_count = full_graph
            .nodes
            .values()
            .filter(|node| node.kind != NodeKind::Subsystem)
            .count();
        // Cap just below the pre-cluster node count so clustering was previously skipped,
        // but large enough that Project + at least one Subsystem can survive the trim.
        let max_auto_nodes = module_only_count.max(2);
        let (graph, _intents, truncated) = build_initial_graph(
            &root,
            WorktreeId::from_usize(1),
            build_opts(max_auto_nodes, false),
        )
        .unwrap();
        assert!(truncated);
        assert!(graph.nodes.len() <= max_auto_nodes);
        assert!(
            graph
                .nodes
                .values()
                .any(|node| node.kind == NodeKind::Subsystem),
            "expected clustering to run before trim so Subsystem nodes are retained"
        );
    }

    #[test]
    fn build_with_intent_llm_disabled_keeps_static_intents_offline() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/simple_workspace");
        let (graph, intents, truncated) =
            build_initial_graph(&root, WorktreeId::from_usize(1), build_opts(usize::MAX, false))
                .unwrap();
        assert!(!truncated);
        assert!(!intents.is_empty());
        assert!(
            intents
                .values()
                .all(|intent| matches!(intent.source, crate::IntentSource::Static)),
            "disabled LLM path must leave static intents untouched without a model"
        );
        let module_intents = StaticIntentProvider::enrich(&graph, &root).unwrap();
        assert!(
            intents.len() >= module_intents.len(),
            "build must include static module intents plus any pin-summary subsystem intents"
        );
        for (node_id, intent) in &module_intents {
            let built = intents.get(node_id).expect("module static intent retained");
            assert_eq!(built.summary, intent.summary);
        }
    }

    #[test]
    fn build_with_intent_llm_enabled_stub_leaves_static_intents() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/simple_workspace");
        let (graph, intents_off, _) =
            build_initial_graph(&root, WorktreeId::from_usize(1), build_opts(usize::MAX, false))
                .unwrap();
        let (_graph_on, intents_on, truncated) =
            build_initial_graph(&root, WorktreeId::from_usize(1), build_opts(usize::MAX, true))
                .unwrap();
        assert!(!truncated);
        // Stub returns no LLM intents; static index must stay intact (no error required).
        assert_eq!(intents_on.len(), intents_off.len());
        for (node_id, intent) in &intents_off {
            let on = intents_on.get(node_id).expect("static intent retained");
            assert_eq!(on.summary, intent.summary);
            assert!(matches!(on.source, crate::IntentSource::Static));
        }
        let _ = graph;
    }

    #[test]
    fn enforce_max_auto_nodes_drops_entries_and_externals_before_modules() {
        let worktree_id = WorktreeId::from_usize(1);
        let project_key = NodeKey::Project { worktree_id };
        let project_id = NodeId::from_key(&project_key);

        let subsystem_key = NodeKey::Subsystem {
            project: project_key.clone().into(),
            slug: "core".into(),
        };
        let subsystem_id = NodeId::from_key(&subsystem_key);

        let module_key = NodeKey::Module {
            worktree_id,
            module_ref: ModuleRef::CargoPackage {
                package_name: "editor".into(),
                manifest_dir: Arc::from(RelPath::from_unix_str("crates/editor").unwrap()),
            },
        };
        let module_id = NodeId::from_key(&module_key);
        let module_key_ref = crate::NodeKeyRef::from(module_key.clone());

        let entry_key = NodeKey::Entry {
            module: module_key_ref.clone(),
            symbol_key: SymbolKey {
                qualified_name: "lib".into(),
                kind: SymbolKind::Module,
            },
        };
        let entry_id = NodeId::from_key(&entry_key);

        let external_key = NodeKey::External {
            ecosystem: EcosystemKind::Cargo,
            name: "serde".into(),
            version_req: None,
        };
        let external_id = NodeId::from_key(&external_key);

        let mut graph = SemanticGraph::default();
        graph
            .apply_patch(GraphPatch {
                base: graph.revision(),
                removed_nodes: vec![],
                removed_edges: vec![],
                upsert_nodes: vec![
                    Node::project(project_id, project_key, "demo"),
                    Node::subsystem(
                        subsystem_id,
                        subsystem_key,
                        "core",
                        SubsystemPayload {
                            member_count: 1,
                            cluster_score: 1.0,
                            pinned: false,
                        },
                        NodeFlags::default(),
                    ),
                    Node::module(
                        module_id,
                        module_key,
                        "editor",
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
                    ),
                    Node::entry(
                        entry_id,
                        entry_key,
                        "lib",
                        None,
                        EntryKind::LibRoot,
                        NodeFlags::default(),
                    ),
                    Node {
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
                    },
                ],
                upsert_edges: vec![
                    Edge::contains(project_id, subsystem_id),
                    Edge::contains(subsystem_id, module_id),
                    Edge::contains(module_id, entry_id),
                    Edge::depends_on(module_id, external_id),
                ],
            })
            .unwrap();

        assert_eq!(graph.nodes.len(), 5);
        let mut intents = IntentIndex::default();
        let truncated = enforce_max_auto_nodes(&mut graph, &mut intents, 3).unwrap();
        assert!(truncated);
        assert_eq!(graph.nodes.len(), 3);
        let kinds: Vec<NodeKind> = graph.nodes.values().map(|node| node.kind).collect();
        assert!(kinds.contains(&NodeKind::Project));
        assert!(kinds.contains(&NodeKind::Subsystem));
        assert!(kinds.contains(&NodeKind::Module));
        assert!(
            !kinds.contains(&NodeKind::Entry),
            "Entries must be dropped before Modules when the cap is tight, kept {kinds:?}"
        );
        assert!(
            !kinds.contains(&NodeKind::External),
            "Externals must be dropped before Modules when the cap is tight, kept {kinds:?}"
        );
    }

    #[test]
    fn build_initial_graph_small_max_drops_entries_before_crate_modules() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/simple_workspace");
        let (full, _, _) =
            build_initial_graph(&root, WorktreeId::from_usize(1), build_opts(usize::MAX, false))
                .unwrap();
        let crate_map_count = full
            .nodes
            .values()
            .filter(|node| {
                matches!(
                    node.kind,
                    NodeKind::Project | NodeKind::Subsystem | NodeKind::Module
                )
            })
            .count();
        assert!(
            full.nodes.values().any(|node| node.kind == NodeKind::Entry),
            "fixture should emit Entry nodes so the trim can drop them"
        );

        let (graph, _intents, truncated) = build_initial_graph(
            &root,
            WorktreeId::from_usize(1),
            build_opts(crate_map_count, false),
        )
        .unwrap();
        assert!(truncated);
        assert!(graph.nodes.len() <= crate_map_count);
        assert!(
            graph
                .nodes
                .values()
                .any(|node| node.kind == NodeKind::Module && node.display_name.as_ref() == "core_lib"),
            "crate Modules must survive a cap that fits Project+Subsystem+Module only"
        );
        assert!(
            graph
                .nodes
                .values()
                .all(|node| node.kind != NodeKind::Entry && node.kind != NodeKind::External),
            "Entries/Externals must be dropped first; leftover kinds: {:?}",
            graph
                .nodes
                .values()
                .map(|node| node.kind)
                .collect::<Vec<_>>()
        );
    }
}
