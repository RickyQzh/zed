use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use collections::HashMap;
use gpui::{AppContext as _, Context, EventEmitter, SharedString, Task};
use util::rel_path::RelPath;
use worktree::WorktreeId;

use crate::{
    build_initial_graph, BuildGraphOptions, GraphPatch, GraphRevision, Intent, ModuleRef, NodeId,
    NodeKey, SemanticGraph, SourceLocation,
};

/// Map from node id → intent text for that node.
pub type IntentIndex = HashMap<NodeId, Intent>;

/// Path within a worktree. Mirrors `project::ProjectPath` without depending on `project`.
#[derive(Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct ProjectPath {
    pub worktree_id: WorktreeId,
    pub path: Arc<RelPath>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphStatus {
    Idle,
    Indexing,
    Partial { reason: SharedString },
    Error { message: SharedString },
}

#[derive(Debug, Clone)]
pub struct SemanticGraphSnapshot {
    pub revision: GraphRevision,
    pub graph: Arc<SemanticGraph>,
    pub intents: Arc<IntentIndex>,
    pub status: GraphStatus,
}

pub enum SemanticGraphEvent {
    Updated { revision: GraphRevision },
}

pub struct SemanticGraphStore {
    graph: SemanticGraph,
    intents: IntentIndex,
    status: GraphStatus,
    reindex_task: Option<Task<()>>,
    reindex_generation: u64,
}

impl EventEmitter<SemanticGraphEvent> for SemanticGraphStore {}

impl SemanticGraphStore {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            graph: SemanticGraph::default(),
            intents: IntentIndex::default(),
            status: GraphStatus::Idle,
            reindex_task: None,
            reindex_generation: 0,
        }
    }

    pub fn snapshot(&self) -> SemanticGraphSnapshot {
        SemanticGraphSnapshot {
            revision: self.graph.revision(),
            graph: Arc::new(self.graph.clone()),
            intents: Arc::new(self.intents.clone()),
            status: self.status.clone(),
        }
    }

    pub fn status(&self) -> &GraphStatus {
        &self.status
    }

    pub fn replace_graph(
        &mut self,
        graph: SemanticGraph,
        intents: IntentIndex,
        cx: &mut Context<Self>,
    ) {
        self.replace_graph_with_status(graph, intents, GraphStatus::Idle, cx);
    }

    pub fn replace_graph_with_status(
        &mut self,
        graph: SemanticGraph,
        intents: IntentIndex,
        status: GraphStatus,
        cx: &mut Context<Self>,
    ) {
        self.graph = graph;
        self.intents = intents;
        self.status = status;
        let revision = self.graph.revision();
        cx.emit(SemanticGraphEvent::Updated { revision });
        cx.notify();
    }

    /// Rebuild the graph for `root` on a background thread.
    ///
    /// The last good snapshot stays visible while `status` is [`GraphStatus::Indexing`].
    /// A newer `reindex` increments a generation counter so a superseded build is ignored.
    /// Assigning `reindex_task` drops the previous task and cancels it.
    ///
    /// When the built graph would exceed `options.max_auto_nodes`, nodes are truncated and
    /// status becomes [`GraphStatus::Partial`].
    ///
    /// `options.intent_llm` gates optional LLM intent enrichment (stub); false keeps the
    /// offline static-intent path and does not require a model service.
    pub fn reindex(
        &mut self,
        root: Arc<Path>,
        worktree_id: WorktreeId,
        options: BuildGraphOptions,
        cx: &mut Context<Self>,
    ) {
        self.reindex_generation = self.reindex_generation.wrapping_add(1);
        let generation = self.reindex_generation;
        self.status = GraphStatus::Indexing;
        cx.notify();

        let max_auto_nodes = options.max_auto_nodes;
        let build = cx.background_spawn(async move {
            build_initial_graph(&root, worktree_id, options)
        });
        self.reindex_task = Some(cx.spawn(async move |this, cx| {
            let result = build.await;
            match this.update(cx, |this, cx| {
                if this.reindex_generation != generation {
                    return;
                }
                match result {
                    Ok((graph, intents, truncated)) => {
                        let status = if truncated {
                            GraphStatus::Partial {
                                reason: SharedString::from(format!(
                                    "Graph truncated to {max_auto_nodes} nodes (max_auto_nodes)"
                                )),
                            }
                        } else {
                            GraphStatus::Idle
                        };
                        this.replace_graph_with_status(graph, intents, status, cx);
                    }
                    Err(error) => {
                        this.status = GraphStatus::Error {
                            message: SharedString::from(format!("{error:#}")),
                        };
                        cx.notify();
                    }
                }
            }) {
                Ok(()) => {}
                Err(_) => {
                    // Store was dropped while indexing; nothing to update.
                }
            }
        }));
    }

    pub fn apply_patch(
        &mut self,
        patch: GraphPatch,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.graph.apply_patch(patch)?;
        let revision = self.graph.revision();
        cx.emit(SemanticGraphEvent::Updated { revision });
        cx.notify();
        Ok(())
    }

    pub fn set_intents(&mut self, intents: Vec<Intent>, cx: &mut Context<Self>) {
        let mut index = IntentIndex::default();
        for intent in intents {
            index.insert(intent.subject, intent);
        }
        self.intents = index;
        let revision = self.graph.revision();
        cx.emit(SemanticGraphEvent::Updated { revision });
        cx.notify();
    }

    /// Nodes whose [`SourceLocation`] or module root matches `path`.
    pub fn nodes_for_path(&self, path: &ProjectPath) -> Vec<NodeId> {
        let mut matches = Vec::new();
        for node in self.graph.nodes.values() {
            if location_matches_path(node.location.as_ref(), path)
                || module_key_matches_path(&node.key, path)
            {
                matches.push(node.id);
            }
        }
        matches.sort();
        matches.dedup();
        matches
    }
}

fn location_matches_path(location: Option<&SourceLocation>, path: &ProjectPath) -> bool {
    location.is_some_and(|location| {
        location.worktree_id == path.worktree_id && location.path.as_ref() == path.path.as_ref()
    })
}

fn module_key_matches_path(key: &NodeKey, path: &ProjectPath) -> bool {
    match key {
        NodeKey::Module {
            worktree_id,
            module_ref,
        } if *worktree_id == path.worktree_id => module_ref_covers_path(module_ref, &path.path),
        _ => false,
    }
}

fn module_ref_covers_path(module_ref: &ModuleRef, path: &Arc<RelPath>) -> bool {
    let root = match module_ref {
        ModuleRef::CargoPackage { manifest_dir, .. } => manifest_dir.as_ref(),
        ModuleRef::PathModule { path } => path.as_ref(),
        ModuleRef::LanguagePackage { root, .. } => root.as_ref(),
    };
    path.as_ref() == root || path.starts_with(root)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use gpui::{AppContext as _, TestAppContext};
    use pretty_assertions::assert_eq;
    use util::rel_path::RelPath;
    use worktree::WorktreeId;

    use super::*;
    use crate::{
        Confidence, ContentHash, GraphPatch, Intent, IntentSource, ModuleRef, Node, NodeId, NodeKey,
        NodeKind, SourceLocation, Timestamp,
    };

    #[gpui::test]
    async fn store_applies_patch_and_notifies(cx: &mut TestAppContext) {
        let store = cx.new(|cx| SemanticGraphStore::new(cx));
        let notified = Arc::new(AtomicBool::new(false));
        let _subscription = cx.update({
            let notified = notified.clone();
            |cx| {
                cx.subscribe(&store, move |_store, event, _cx| match event {
                    SemanticGraphEvent::Updated { .. } => {
                        notified.store(true, Ordering::SeqCst);
                    }
                })
            }
        });

        store.update(cx, |store, cx| {
            let key = NodeKey::Project {
                worktree_id: WorktreeId::from_usize(1),
            };
            let id = NodeId::from_key(&key);
            store
                .apply_patch(
                    GraphPatch {
                        base: store.snapshot().revision,
                        removed_nodes: vec![],
                        removed_edges: vec![],
                        upsert_nodes: vec![Node::project(id, key, "p")],
                        upsert_edges: vec![],
                    },
                    cx,
                )
                .unwrap();
        });

        let snap = store.read_with(cx, |s, _| s.snapshot());
        assert_eq!(snap.graph.nodes.len(), 1);
        assert!(
            notified.load(Ordering::SeqCst),
            "expected SemanticGraphEvent::Updated"
        );
    }

    #[gpui::test]
    async fn store_set_intents_indexes_by_subject(cx: &mut TestAppContext) {
        let store = cx.new(|cx| SemanticGraphStore::new(cx));
        let subject = NodeId(42);
        store.update(cx, |store, cx| {
            store.set_intents(
                vec![Intent {
                    subject,
                    summary: "owns buffer state".into(),
                    bullets: vec!["edit".into()],
                    confidence: Confidence::High,
                    source: IntentSource::Static,
                    evidence: vec![],
                    updated_at: Timestamp(1),
                    content_hash: ContentHash(1),
                }],
                cx,
            );
        });
        let snap = store.read_with(cx, |s, _| s.snapshot());
        assert_eq!(snap.intents.len(), 1);
        assert_eq!(
            snap.intents.get(&subject).map(|intent| intent.summary.as_ref()),
            Some("owns buffer state")
        );
        assert_eq!(snap.status, GraphStatus::Idle);
    }

    #[gpui::test]
    async fn store_nodes_for_path_matches_location_and_module_root(cx: &mut TestAppContext) {
        let store = cx.new(|cx| SemanticGraphStore::new(cx));
        let worktree_id = WorktreeId::from_usize(1);
        let module_path = RelPath::from_unix_str("crates/editor").unwrap();
        let file_path = RelPath::from_unix_str("crates/editor/src/editor.rs").unwrap();

        let module_key = NodeKey::Module {
            worktree_id,
            module_ref: ModuleRef::PathModule {
                path: Arc::from(module_path),
            },
        };
        let module_id = NodeId::from_key(&module_key);
        let module_node = Node {
            id: module_id,
            key: module_key.clone(),
            kind: NodeKind::Module,
            display_name: "editor".into(),
            abbrev: None,
            location: Some(SourceLocation {
                worktree_id,
                path: Arc::from(module_path),
                range: None,
                symbol: None,
            }),
            payload: crate::NodePayload::Module(crate::ModulePayload {
                language: Some("rust".into()),
                module_kind: crate::ModuleKind::FileModule,
                public_exports: vec![],
                deps_out_count: 0,
                deps_in_count: 0,
                loc_estimate: None,
            }),
            flags: crate::NodeFlags::default(),
        };

        let entry_key = NodeKey::Entry {
            module: module_key.into(),
            symbol_key: crate::SymbolKey {
                qualified_name: "editor::Editor".into(),
                kind: crate::SymbolKind::Struct,
            },
        };
        let entry_id = NodeId::from_key(&entry_key);
        let entry_node = Node {
            id: entry_id,
            key: entry_key,
            kind: NodeKind::Entry,
            display_name: "Editor".into(),
            abbrev: None,
            location: Some(SourceLocation {
                worktree_id,
                path: Arc::from(file_path),
                range: None,
                symbol: None,
            }),
            payload: crate::NodePayload::Entry(crate::EntryPayload {
                entry_kind: crate::EntryKind::PublicApi,
            }),
            flags: crate::NodeFlags::default(),
        };

        store.update(cx, |store, cx| {
            store
                .apply_patch(
                    GraphPatch {
                        base: store.snapshot().revision,
                        removed_nodes: vec![],
                        removed_edges: vec![],
                        upsert_nodes: vec![module_node, entry_node],
                        upsert_edges: vec![],
                    },
                    cx,
                )
                .unwrap();
        });

        let exact = store.read_with(cx, |store, _| {
            store.nodes_for_path(&ProjectPath {
                worktree_id,
                path: file_path.into(),
            })
        });
        let mut expected_exact = vec![entry_id, module_id];
        expected_exact.sort();
        assert_eq!(exact, expected_exact);

        let under_module = store.read_with(cx, |store, _| {
            store.nodes_for_path(&ProjectPath {
                worktree_id,
                path: RelPath::from_unix_str("crates/editor/src/element.rs")
                    .unwrap()
                    .into(),
            })
        });
        assert_eq!(under_module, vec![module_id]);
    }

    #[gpui::test]
    async fn reindex_transitions_indexing_to_idle(cx: &mut TestAppContext) {
        let root = Arc::<Path>::from(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("test_data/simple_workspace")
                .into_boxed_path(),
        );
        let worktree_id = WorktreeId::from_usize(1);
        let store = cx.new(|cx| SemanticGraphStore::new(cx));

        store.update(cx, |store, cx| {
            store.reindex(root.clone(), worktree_id, BuildGraphOptions::default(), cx);
        });

        let status = store.read_with(cx, |store, _| store.status().clone());
        assert_eq!(status, GraphStatus::Indexing);

        cx.run_until_parked();

        let snap = store.read_with(cx, |store, _| store.snapshot());
        assert_eq!(snap.status, GraphStatus::Idle);
        assert!(
            snap.graph.nodes.values().any(|node| {
                node.kind == NodeKind::Module && node.display_name.as_ref() == "core_lib"
            }),
            "expected cargo fixture modules after reindex"
        );
        assert!(!snap.intents.is_empty());
    }

    #[gpui::test]
    async fn reindex_sets_partial_when_over_max_auto_nodes(cx: &mut TestAppContext) {
        let root = Arc::<Path>::from(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("test_data/simple_workspace")
                .into_boxed_path(),
        );
        let worktree_id = WorktreeId::from_usize(1);
        let store = cx.new(|cx| SemanticGraphStore::new(cx));

        // Fixture has multiple modules/nodes; a tiny budget must truncate.
        let max_auto_nodes = 2;
        store.update(cx, |store, cx| {
            store.reindex(
                root.clone(),
                worktree_id,
                BuildGraphOptions {
                    max_auto_nodes,
                    ..BuildGraphOptions::default()
                },
                cx,
            );
        });
        cx.run_until_parked();

        let snap = store.read_with(cx, |store, _| store.snapshot());
        assert!(
            matches!(snap.status, GraphStatus::Partial { .. }),
            "expected Partial when modules exceed max_auto_nodes, got {:?}",
            snap.status
        );
        assert!(
            snap.graph.nodes.len() <= max_auto_nodes,
            "graph must respect max_auto_nodes budget, got {} nodes",
            snap.graph.nodes.len()
        );
        let module_count = snap
            .graph
            .nodes
            .values()
            .filter(|node| node.kind == NodeKind::Module)
            .count();
        assert!(
            module_count <= max_auto_nodes,
            "module count must not exceed budget"
        );
    }

    #[gpui::test]
    async fn reindex_keeps_last_snapshot_until_build_applies(cx: &mut TestAppContext) {
        let root = Arc::<Path>::from(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("test_data/simple_workspace")
                .into_boxed_path(),
        );
        let worktree_id = WorktreeId::from_usize(1);
        let store = cx.new(|cx| SemanticGraphStore::new(cx));

        let stub_key = NodeKey::Project { worktree_id };
        let stub_id = NodeId::from_key(&stub_key);
        store.update(cx, |store, cx| {
            store
                .apply_patch(
                    GraphPatch {
                        base: store.snapshot().revision,
                        removed_nodes: vec![],
                        removed_edges: vec![],
                        upsert_nodes: vec![Node::project(stub_id, stub_key, "stub")],
                        upsert_edges: vec![],
                    },
                    cx,
                )
                .unwrap();
        });

        store.update(cx, |store, cx| {
            store.reindex(root.clone(), worktree_id, BuildGraphOptions::default(), cx);
        });

        store.read_with(cx, |store, _| {
            assert_eq!(store.status(), &GraphStatus::Indexing);
            assert!(
                store.graph.nodes.contains_key(&stub_id),
                "reindex must keep the last good snapshot while Indexing"
            );
            assert_eq!(store.graph.nodes.len(), 1);
        });

        cx.run_until_parked();

        let snap = store.read_with(cx, |store, _| store.snapshot());
        assert_eq!(snap.status, GraphStatus::Idle);
        assert!(
            !snap.graph.nodes.contains_key(&stub_id),
            "completed reindex must replace the previous snapshot"
        );
        assert!(
            snap.graph.nodes.values().any(|node| {
                node.kind == NodeKind::Module && node.display_name.as_ref() == "core_lib"
            }),
            "expected cargo fixture modules after reindex apply"
        );
    }

    #[gpui::test]
    async fn reindex_ignores_superseded_generation(cx: &mut TestAppContext) {
        let root = Arc::<Path>::from(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("test_data/simple_workspace")
                .into_boxed_path(),
        );
        let worktree_id = WorktreeId::from_usize(1);
        let store = cx.new(|cx| SemanticGraphStore::new(cx));

        let stub_key = NodeKey::Project { worktree_id };
        let stub_id = NodeId::from_key(&stub_key);
        store.update(cx, |store, cx| {
            store
                .apply_patch(
                    GraphPatch {
                        base: store.snapshot().revision,
                        removed_nodes: vec![],
                        removed_edges: vec![],
                        upsert_nodes: vec![Node::project(stub_id, stub_key, "stub")],
                        upsert_edges: vec![],
                    },
                    cx,
                )
                .unwrap();
            store.reindex(root.clone(), worktree_id, BuildGraphOptions::default(), cx);
            store.reindex_generation = store.reindex_generation.wrapping_add(1);
        });

        cx.run_until_parked();

        store.read_with(cx, |store, _| {
            assert_eq!(store.status(), &GraphStatus::Indexing);
            assert!(
                store.graph.nodes.contains_key(&stub_id),
                "stale reindex generation must not replace the last good snapshot"
            );
            assert_eq!(store.graph.nodes.len(), 1);
        });
    }
}
