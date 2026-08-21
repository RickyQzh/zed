use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Instant;

use worktree::WorktreeId;

use crate::{
    build_initial_graph, extract_cargo_workspace, BuildGraphOptions, EdgeKind, GraphIndexer,
    GraphStatus, NodeKind, SemanticGraph, SemanticGraphSnapshot,
};

fn simple_workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/simple_workspace")
}

fn zed_workspace_root() -> PathBuf {
    let from_manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    from_manifest
        .canonicalize()
        .unwrap_or(from_manifest)
}

fn dogfood_options() -> BuildGraphOptions {
    BuildGraphOptions {
        max_auto_nodes: 500,
        module_depth: 2,
        ..BuildGraphOptions::default()
    }
}

fn module_names(graph: &SemanticGraph) -> BTreeSet<String> {
    graph
        .nodes
        .values()
        .filter(|node| node.kind == NodeKind::Module)
        .map(|node| node.display_name.to_string())
        .collect()
}

fn module_id(graph: &SemanticGraph, name: &str) -> crate::NodeId {
    graph
        .nodes
        .values()
        .find(|node| node.kind == NodeKind::Module && node.display_name.as_ref() == name)
        .map(|node| node.id)
        .unwrap_or_else(|| panic!("expected module named {name}"))
}

fn snapshot_is_usable(snapshot: &SemanticGraphSnapshot) {
    assert!(
        !matches!(snapshot.status, GraphStatus::Error { .. }),
        "graph status should not be Error, got {:?}",
        snapshot.status
    );
    assert!(
        !snapshot.graph.nodes.is_empty(),
        "graph must contain at least one node"
    );
}

/// A user looking at the map of the fixture should see both crates, their
/// dependency, and the fixture's human-readable intents.
#[test]
fn user_can_understand_simple_workspace_from_map() {
    let root = simple_workspace_root();
    let snapshot = GraphIndexer::reindex_cargo_or_generic(&root, WorktreeId::from_usize(1))
        .expect("fixture graph should build");
    snapshot_is_usable(&snapshot);
    assert!(
        !matches!(snapshot.status, GraphStatus::Partial { .. }),
        "small fixture should not truncate"
    );

    let names = module_names(&snapshot.graph);
    assert!(names.contains("app"), "expected app module, got {names:?}");
    assert!(
        names.contains("core_lib"),
        "expected core_lib module, got {names:?}"
    );

    let app_id = module_id(&snapshot.graph, "app");
    let core_lib_id = module_id(&snapshot.graph, "core_lib");
    assert!(
        snapshot.graph.edges.values().any(|edge| {
            edge.kind == EdgeKind::DependsOn && edge.from == app_id && edge.to == core_lib_id
        }),
        "expected DependsOn(app → core_lib) so the map shows why the binary exists"
    );

    let core_lib_intent = snapshot
        .intents
        .get(&core_lib_id)
        .expect("core_lib should have a static intent from its Cargo.toml description");
    let intent_text = format!(
        "{} {}",
        core_lib_intent.summary,
        core_lib_intent
            .evidence
            .iter()
            .map(|evidence| evidence.excerpt.to_string())
            .collect::<Vec<_>>()
            .join(" ")
    )
    .to_lowercase();
    assert!(
        intent_text.contains("domain") || intent_text.contains("logic"),
        "core_lib intent should mention domain/logic from the fixture description, got {:?}",
        core_lib_intent.summary
    );

    let pin_intent = snapshot.intents.values().find(|intent| {
        intent
            .summary
            .to_lowercase()
            .contains("application binary")
    });
    assert!(
        pin_intent.is_some(),
        "pin summary “Application binary” should appear as a subsystem intent, intents={:?}",
        snapshot
            .intents
            .values()
            .map(|intent| intent.summary.to_string())
            .collect::<Vec<_>>()
    );
}

/// Cargo-only parse of the Zed workspace — fast enough to always run.
#[test]
fn cargo_extractor_lists_zed_workspace_members() {
    let root = zed_workspace_root();
    assert!(
        root.join("Cargo.toml").is_file(),
        "expected workspace Cargo.toml at {}",
        root.display()
    );

    let patch = extract_cargo_workspace(&root, WorktreeId::from_usize(1))
        .expect("Zed workspace Cargo.toml members should parse");
    let mut graph = SemanticGraph::default();
    graph
        .apply_patch(patch)
        .expect("cargo extract patch should apply");

    let names = module_names(&graph);
    for expected in ["gpui", "editor", "project"] {
        assert!(
            names.contains(expected),
            "expected Cargo package {expected} in Zed workspace members, got {names:?}"
        );
    }
}

/// Full-graph dogfood of this repo. Ignored when a timed trial exceeds 5s.
#[test]
fn dogfood_zed_workspace_indexes_well_known_crates() {
    let root = zed_workspace_root();
    let started = Instant::now();
    let (graph, intents, _truncated) =
        build_initial_graph(&root, WorktreeId::from_usize(1), dogfood_options())
            .expect("dogfood build_initial_graph should return Ok");
    let elapsed = started.elapsed();

    assert!(
        !graph.nodes.is_empty(),
        "dogfood graph should be non-empty"
    );
    assert!(
        !intents.is_empty() || graph.nodes.values().any(|node| node.kind == NodeKind::Module),
        "dogfood should surface modules even if static intents are sparse"
    );

    let names = module_names(&graph);
    for expected in ["gpui", "editor", "project"] {
        assert!(
            names.contains(expected),
            "expected well-known crate {expected} on the map, got {names:?}"
        );
    }

    assert!(
        elapsed.as_secs() < 60,
        "dogfood unexpectedly took {:?} — investigate extractor hang",
        elapsed
    );
}
